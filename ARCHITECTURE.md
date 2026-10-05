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
- **WebSocket server** (port 18847): requires a `token` query parameter on the upgrade, matching `~/.turbofig/token` (constant-time compare, checked after the Origin check, 401 on mismatch or absence). This closes a gap Origin checking alone leaves: a sandboxed `<iframe>` on a malicious web page reports Origin `null`, the same value the real Figma plugin UI reports. See `DECISIONS.md` #39 and `SECURITY.md`. Holds the persistent connection from the Figma plugin. The plugin sends `FILE_INFO` (`fileKey` + name) on connect, and again whenever it re-announces (e.g. a detected file rename). The daemon routes a tool call to the plugin by sending a request over this socket and awaiting a `RESULT`. A per-request timeout (`TURBOFIG_REQUEST_TIMEOUT_MS`, default 30000, clamped to 600000) stops a silent plugin from hanging a call; `EXECUTE`, `GET_SELECTION`, and `SCREENSHOT` all carry that timeout to the plugin as `timeoutMs`, and the daemon itself waits `timeoutMs + 1000ms` for `EXECUTE`, so the plugin's own "I gave up" reply usually beats the daemon's bare timeout (see `DECISIONS.md`). The daemon holds one connection per open file in a `conn_id`-keyed registry. Two live connections may hold the same `fileKey` at once (a reconnect, or a second window on the same file): neither evicts the other, so neither one's in-flight jobs are ever cancelled by the other connecting, and a later FILE_INFO from either connection can never knock the other out of the registry. Routing (`connections_named`/`resolve_route`) dedupes a shared `fileKey` down to the newest (highest conn_id) live connection, so a caller never sees Ambiguous for a single logical file; if that newest connection closes, routing falls back to an older one still open. A frame from a conn_id no longer in the registry is ignored outright. A `RESULT` only resolves the pending request if it arrives on the same connection the request was sent to, so one connection can never forge another's reply. Message and frame size are capped at 32 MiB; a keepalive ping goes out every 15s and a connection with no pong in 45s is dropped. On socket close the daemon drops only that connection and fails only that connection's in-flight requests at once, so other files keep working.
- **File-bridge** (default `~/.turbofig`): watches `inbox/` and writes `outbox/`. A client writes a job file and reads the result file, so no curl and no MCP connection are needed. This is the primary transport for locked-down Claude Enterprise accounts. It parses each job into the same typed parameter structs the MCP tools use (one `#[serde(tag = "op")]` enum, so a bad field fails to parse the same way for both transports): a file that reads and parses as JSON but fails the Job schema (a bad field, an unknown op) is rejected at once with a clear error, never held for the parse-grace window that exists only for a half-written file. A valid job is claimed (removes the inbox file), gets any stale same-id outbox result deleted, and runs in its own task so one slow job never blocks the loop. Job ids are a documented client contract: unique per job, never reused while the first job with that id may still be in flight (`skills/file-bridge.md`, `helpers/tf-api.md`, the plugin's connect prompt). A duplicate id that arrives while its twin is still running is left untouched in the inbox (not claimed, not answered) until the first finishes, so neither job's `.tmp` file or result is ever touched by the other. The ops are `status`, `execute`, `get_selection`, and `screenshot`. Screenshot file-mode writes the PNG into `outbox/<requestId>-<nanos>.png` (`AppState.screenshot_dir`) and returns its path; a subagent reads it. The outbox is a drop box, not storage: a backstop sweep deletes results and PNGs older than 24h. An inbox entry that never reads or parses (a bad write, an unreadable file, a non-UTF-8 name) gets one error result after a short grace window and is then left alone, instead of being retried and re-logged forever.

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
| `GET_SELECTION` | daemon to plugin | Return compact selection info; queued (Phase 3). Carries `timeoutMs`, raced the same way as `EXECUTE` |
| `SCREENSHOT` | daemon to plugin | Export PNG; queued (Phase 3). Carries `timeoutMs`, raced the same way as `EXECUTE` |

This dispatch table is hybrid-ready. A community-safe command vocabulary is additive: add new types without reworking the existing structure.

`EXECUTE`, `GET_SELECTION` and `SCREENSHOT` run one at a time, FIFO, through a single queue in the main thread (`createDispatcher` in `code.ts`), so two overlapping jobs can never interleave and create duplicate nodes. `STATUS`, `SET_PORT`, `RESIZE` and `READY` bypass the queue and run immediately. Every queued reply is capped at 16 MiB (`capResultMessage`); an oversized reply becomes an `ok:false` error naming the size instead of reaching the WebSocket.

Each queued job carries a deadline stamped when the daemon's message is received (enqueue time), not when the job reaches the front of the queue: `timeoutMs` (defaulting to 30000 when absent) is added to the receive time once, up front. If that deadline has already passed by the time a job is dequeued, it never runs at all; the plugin replies `ok:false` with "expired in the queue; the job did not run", making a retry obviously safe. Otherwise the job races against whatever time remains until the deadline, for all three types, not only `EXECUTE`: a hung `exportAsync` or `getNodeByIdAsync` can no longer block the queue forever just because `GET_SELECTION`/`SCREENSHOT` previously had no timeout race. A timeout that fires while the job is actually running states that it may still be running and a retry is not idempotent; a synchronous infinite loop in the user's code still cannot be interrupted this way, since JavaScript is single-threaded. `timeoutMs` is clamped to `2^31 - 1` ms both in the daemon's own config and again in the plugin, since a larger value overflows a JS `setTimeout` and fires almost immediately instead of waiting.

The queue itself is one chained promise (`queueTail`); each enqueue appends a `.then(run).then(post).catch(swallow)` link, so a `job()` or `post()` throw in one link can never leave `queueTail` permanently rejected and silently drop every job queued after it.

`EXECUTE` runs the JS as an async function built with the Function constructor (validated in Figma's sandbox, see `DECISIONS.md` #17). Two things are injected into the eval scope before user code runs: the sync-to-async deprecation preamble (runs first), and the `tf` craft namespace (`createTf(figma)`, passed as a second parameter beside `figma`). So generated code calls `figma.*` and `tf.*` directly. Eval errors return a clean message (with a line/column relative to the user's own code, adjusted for the preamble) and never crash the plugin.

## Routing registry (Phase 4)

The daemon keeps a connection registry keyed by two dimensions:

- `mcp-session-id`: assigned at the MCP `initialize` call; identifies a Claude session.
- `fileKey`: sent by the plugin on connect; identifies an open Figma file.

Each MCP session is paired to a `fileKey` by an explicit pick or the sole connected plugin. `resolve_route` picks the target connection by explicit `fileKey`, then the session pairing, then the sole named connection. It returns a clear error for no plugin, ambiguous target, or a not-connected file. A session pairing is pruned after 24h of inactivity and capped at 1000 entries, so a long-running daemon's session map cannot grow forever. A socket close cancels only that connection's in-flight requests. The registry handles N sessions and N files concurrently, fully isolated.

Two live connections may hold the same `fileKey` at once: `set_connection_info` no longer evicts the older one when a new connection claims an already-registered `fileKey`. Eviction closed neither socket (`ws.rs` kept its `tx`), so a later FILE_INFO from the evicted connection could remove the live one and lose the route, and eviction cancelled in-flight jobs that were still running in Figma. Instead, every live connection stays registered, even sharing a `fileKey`; `connections_named` (the routing-facing view) dedupes a shared key down to the newest (highest conn_id) entry, so a caller still never sees Ambiguous for one logical file, and the same dedupe applies to the status/Ambiguous file lists. If the newest connection closes, routing naturally falls back to an older one still open, since it is still in the raw registry `remove_connection` never touched. A frame tagged with a conn_id no longer in the registry is ignored outright (see `DECISIONS.md` #35).

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

## Embedded plugin and the pairing token

The daemon binary embeds the built Figma plugin (`plugin/manifest.json`,
`plugin/dist/code.js`, `plugin/dist/ui.html`) at compile time via
`daemon/build.rs` and `daemon/src/embedded.rs`'s `embedded_plugin()`, so a
Homebrew-installed daemon (no repo checkout) can still write the plugin out
to disk. `daemon/src/plugin_files.rs`'s `write_plugin_files(home, token)`
writes those three files to `<home>/figma-plugin/`, replacing the
`__TURBOFIG_PAIRING_TOKEN__` placeholder in `ui.html` with the real pairing
token, plus a version+token-hash marker (`plugin_files_outdated` reads it to
detect a stale copy). `plugin/dist` is a Bun build, not a Cargo artifact, so
`build.rs` degrades to a stub (`embedded_plugin()` returns `None`) when it is
missing, keeping a Rust-only `cargo build` and the CI Rust job working. A
later `turbofig setup` command (not built yet) is the one that calls
`write_plugin_files` on a user's machine; see `DECISIONS.md` #38.

`daemon/src/token.rs` owns the pairing token itself: see "WebSocket server"
above and `DECISIONS.md` #39 for the full design.

## Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `TURBOFIG_MCP_PORT` | 18846 | HTTP MCP port |
| `TURBOFIG_WS_PORT` | 18847 | Plugin WebSocket port |
| `TURBOFIG_REQUEST_TIMEOUT_MS` | 30000 | Wait for a plugin reply before returning a timeout result. Clamped to 600000 (10 min); a larger value is logged and clamped, since it would otherwise overflow a JS `setTimeout` on the plugin side |
| `TURBOFIG_BRIDGE_DIR` | `~/.turbofig` | File-bridge inbox and outbox root |
