# Architecture

## Transport chain

```
Claude / AI client
    |
    |  one of three inbound transports to the daemon:
    |   1. HTTP POST /mcp   (port 18846, MCP streamable-http, SSE response)
    |   2. file-bridge      (~/.turbofig/inbox -> outbox; write and read files)
    |   3. curl on 18846    (the same HTTP endpoint, as a fallback)
    |
Rust daemon  (daemon/)
    |
    | WebSocket  (port 18847)
    |
Figma plugin UI thread  (plugin/src/ui.html)   holds the socket
    |
    | postMessage / onmessage
    |
Figma plugin main thread  (plugin/src/code.ts)   runs the Figma API
    |
    | Figma Plugin API
    v
Figma document
```

The three inbound transports converge on one shared `AppState`, so a call from
any of them routes to the plugin the same way. The file-bridge exists for
locked-down clients that cannot use curl or a native MCP server. See
`DECISIONS.md` item 15 and `skills/file-bridge.md`.

## Daemon

The daemon is a single Rust process. It runs three servers as three `tokio::spawn` tasks that share one `Arc<AppState>`:

- **MCP HTTP server** (port 18846): speaks the MCP streamable-http protocol via `rmcp`. Uses `legacy_session_mode` so that clients on the 2025-03-26 spec can supply an `mcp-session-id` header. Each MCP session is stateful. Each tool reads the `mcp-session-id` from the HTTP request parts and maps the session to a plugin connection by `fileKey` (Phase 4). `allowed_hosts` is pinned explicitly to `localhost`, `127.0.0.1`, `::1` rather than left to `rmcp`'s default, so a future crate upgrade cannot silently widen the daemon's DNS-rebinding guard.
- **WebSocket server** (port 18847): holds the persistent connection from the Figma plugin. The plugin sends `FILE_INFO` (`fileKey` + name) on connect. The daemon routes a tool call to the plugin by sending a request over this socket and awaiting a `RESULT`. A per-request timeout (`TURBOFIG_REQUEST_TIMEOUT_MS`, default 30000) stops a silent plugin from hanging a call; `EXECUTE` additionally carries that timeout to the plugin as `timeoutMs` and the daemon itself waits `timeoutMs + 1000ms`, so the plugin's own "I gave up" reply usually beats the daemon's bare timeout (see `DECISIONS.md`). The daemon holds one connection per open file in a `conn_id`-keyed registry; at most one live connection may hold a given `fileKey`, so a reconnect or a second window on the same file evicts the older connection instead of leaving both ambiguous. A `RESULT` only resolves the pending request if it arrives on the same connection the request was sent to, so one connection can never forge another's reply. Message and frame size are capped at 32 MiB; a keepalive ping goes out every 15s and a connection with no pong in 45s is dropped. On socket close the daemon drops only that connection and fails only that connection's in-flight requests at once, so other files keep working.
- **File-bridge** (default `~/.turbofig`): watches `inbox/` and writes `outbox/`. A client writes a job file and reads the result file, so no curl and no MCP connection are needed. This is the primary transport for locked-down Claude Enterprise accounts. It parses each job into the same typed parameter structs the MCP tools use (one `#[serde(tag = "op")]` enum, so a bad field fails to parse the same way for both transports), claims it (removes the inbox file), deletes any stale same-id outbox result, and runs it in its own task so one slow job never blocks the loop. The ops are `status`, `execute`, `get_selection`, and `screenshot`. Screenshot file-mode writes the PNG into `outbox/<requestId>-<nanos>.png` (`AppState.screenshot_dir`) and returns its path; a subagent reads it. The outbox is a drop box, not storage: a backstop sweep deletes results and PNGs older than 24h. An inbox entry that never reads or parses (a bad write, an unreadable file, a non-UTF-8 name) gets one error result after a short grace window and is then left alone, instead of being retried and re-logged forever.

Each capability has one shared `run_*` routine, all funnelled through one send/await/timeout/cancel helper (`plugin_call::call_plugin`). Both the MCP tool and the file-bridge op call the same routine: `run_status`, `run_execute`, `run_get_selection`, and `run_screenshot`.

The daemon is always-on. A launchd service starts it at login and `KeepAlive` restarts it on crash. It is decoupled from any client session, so a client disconnect or session end never stops it.

### Module layout (`daemon/src/`)

| Module | Holds |
|---|---|
| `lib.rs` | Crate docs, `mod` declarations, re-exports only |
| `config.rs` | Env-driven settings: ports, request timeout, bridge dir |
| `state.rs` | `AppState`, the connection registry, session pairing, the pending-request map |
| `routing.rs` | `RouteError`, `resolve_route` |
| `plugin_call.rs` | The one register/send/await/timeout/cancel path all four ops share, plus the pending-cleanup drop guard |
| `ops/` | `status.rs`, `execute.rs`, `selection.rs`, `screenshot.rs` (the four `run_*` routines), `budget.rs` (context-firewall size warnings) |
| `image.rs` | A cheap PNG header probe (`probe_dims`) kept separate from the expensive decode/resize/encode path (`resize_png`), so a screenshot that needs no resize never pays for either |
| `mcp.rs` | MCP tool parameter structs, `TurbofigHandler`, `HELP_TEXT`, `build_router`, the Origin-rejection middleware |
| `ws.rs` | `handle_socket`, the WS Origin check, keepalive ping/pong, `serve_ws` |
| `bridge/mod.rs`, `bridge/job.rs` | The inbox scan loop; `job.rs`'s typed `Job` enum reuses the MCP param structs |

## Plugin

The plugin has two threads, as required by the Figma plugin model:

- **Main thread** (`plugin/src/code.ts`): runs in the Figma sandbox. Has access to the Figma Plugin API. Receives messages from the UI thread and executes Figma API calls.
- **UI thread** (`plugin/src/ui/` → built to `dist/ui.html`): runs in a sandboxed iframe. Holds the WebSocket connection to the daemon. Relays messages between the daemon and the main thread via `parent.postMessage` / `figma.ui.onmessage`. The panel has three screens toggled by `.screen`/`.active` class swap, each with its own `figma.ui.resize` call via a `RESIZE` message: **main** (300×150, default, wordmark + status + file row + footer nav), **advanced** (300×240, activity log + port field), **about** (300×375, large wordmark + tagline + description + repo/author links). The header wordmark uses a pure-CSS motion ghost effect (`text-shadow` with `color-mix(in srgb, var(--text) N%, transparent)`) so it is theme-aware without JS — light and dark mode both work via the injected `--figma-color-*` tokens.

The plugin dispatches on a `{type}` field in each message:

| Type | Direction | Description |
|---|---|---|
| `READY` | UI to main thread | Sent once, on load; the main thread replies with `FILE_INFO` and `PORT` (Phase 12) |
| `FILE_INFO` | plugin to daemon | Sent on connect, and again on a detected file rename: fileKey and root name (Phase 2) |
| `STATUS` | daemon to plugin | Liveness ping carrying a `requestId`; bypasses the job queue (Phase 2) |
| `RESULT` | plugin to daemon | Reply carrying the matching `requestId` (Phase 2) |
| `EXECUTE` | daemon to plugin | Run arbitrary Figma Plugin API JS; queued (Phase 3). Carries `timeoutMs`: the plugin must stop waiting and reply `ok:false` at that point; the daemon itself waits `timeoutMs + 1000ms` |
| `GET_SELECTION` | daemon to plugin | Return compact selection info; queued (Phase 3) |
| `SCREENSHOT` | daemon to plugin | Export PNG; queued (Phase 3) |

This dispatch table is hybrid-ready. A community-safe command vocabulary is additive: add new types without reworking the existing structure.

`EXECUTE`, `GET_SELECTION` and `SCREENSHOT` run one at a time, FIFO, through a single queue in the main thread (`createDispatcher` in `code.ts`), so two overlapping jobs can never interleave and create duplicate nodes. `STATUS`, `SET_PORT`, `RESIZE` and `READY` bypass the queue and run immediately. Every queued reply is capped at 16 MiB (`capResultMessage`); an oversized reply becomes an `ok:false` error naming the size instead of reaching the WebSocket. An `EXECUTE` carrying `timeoutMs` races the job against a timer and replies with a timeout error if the job is still running; a synchronous infinite loop in the user's code cannot be interrupted this way, since JavaScript is single-threaded.

`EXECUTE` runs the JS as an async function built with the Function constructor (validated in Figma's sandbox, see `DECISIONS.md` #17). Two things are injected into the eval scope before user code runs: the sync-to-async deprecation preamble (runs first), and the `tf` craft namespace (`createTf(figma)`, passed as a second parameter beside `figma`). So generated code calls `figma.*` and `tf.*` directly. Eval errors return a clean message (with a line/column relative to the user's own code, adjusted for the preamble) and never crash the plugin.

## Routing registry (Phase 4)

The daemon keeps a connection registry keyed by two dimensions:

- `mcp-session-id`: assigned at the MCP `initialize` call; identifies a Claude session.
- `fileKey`: sent by the plugin on connect; identifies an open Figma file.

Each MCP session is paired to a `fileKey` by an explicit pick or the sole connected plugin. `resolve_route` picks the target connection by explicit `fileKey`, then the session pairing, then the sole named connection. It returns a clear error for no plugin, ambiguous target, or a not-connected file. A session pairing is pruned after 24h of inactivity and capped at 1000 entries, so a long-running daemon's session map cannot grow forever. A socket close cancels only that connection's in-flight requests. The registry handles N sessions and N files concurrently, fully isolated.

At most one live connection may hold a given `fileKey` at a time: `set_connection_info` evicts the older connection (and cancels its in-flight requests) when a new one claims a `fileKey` already in the registry. This covers a reconnect and a second window on the same file; routing always lands on the newest connection instead of reporting it as ambiguous.

## Helper layer

The `tf` namespace is a compact JS craft library injected into every eval. Source lives in `plugin/src/helpers.ts` (pure logic unit-tested; figma glue smoke-tested), exposed via `createTf(figma)` and passed into the eval as `tf`. The compact API reference is `helpers/tf-api.md` (this is what the model reads to learn the helpers cheaply). The library is complete as of Phase 6. Categories and members:

- Layout primitives: `frame` (per-axis sizing, transparent by default), `rect`, `append`, `clear`, `findOrCreate` (idempotent-by-name).
- Text and fonts: `text` (font-load, optional `width` for wrapping), `loadFonts`, `color`, `solid`.
- Decks and slides: `deck`, `slide`, `slidePosition`, `chunk`.
- Component instances: `instance`, `instanceByKey`.
- Variables: `getVariable`, `setVariableValue`, `readVariableValue`.
- Export: `export`.
- Utilities: `skipInvisible`, `findAll`, `commit`.

All functions use the async Figma API surface required by `documentAccess: dynamic-page`.

Idempotency pattern for re-runnable sections: `findOrCreate(parent, name, factory)` then `clear(node)` then rebuild. `findOrCreate` protects only the named node, so `clear` before rebuilding prevents duplicated children on a resume.

## Design orchestration

`.claude/commands/design.md` is the `/design` command: a brief becomes a full page via plan-first spec (persisted to `~/.turbofig/design/<job-id>/plan.json` + `status.json` as the checkpoint) -> parallel firewalled builder subagents (each one batched `tf.*` call, unique per-request bridge id) -> an assembly step that stacks sections in order (parallel builds otherwise overlap at 0,0) -> a QA critic subagent that reads the screenshot and returns text only -> a capped refine loop -> resume from the last completed section. Images live and die in subagents; the orchestrator never holds a screenshot.

## Skill layer

`skills/` (Phase 7+): the design-worker recipes, led by `design.md` (the `/design` command).

## Ports

Both ports are product contracts, not dev-server conventions. See `DECISIONS.md` item 3.

| Port | Protocol | Purpose |
|---|---|---|
| 18846 | HTTP (SSE) | MCP endpoint for Claude / AI clients |
| 18847 | WebSocket | Daemon-to-plugin persistent connection |

Both are overridable via `TURBOFIG_MCP_PORT` and `TURBOFIG_WS_PORT` environment variables.

## Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `TURBOFIG_MCP_PORT` | 18846 | HTTP MCP port |
| `TURBOFIG_WS_PORT` | 18847 | Plugin WebSocket port |
| `TURBOFIG_REQUEST_TIMEOUT_MS` | 30000 | Wait for a plugin reply before returning a timeout result |
| `TURBOFIG_BRIDGE_DIR` | `~/.turbofig` | File-bridge inbox and outbox root |
