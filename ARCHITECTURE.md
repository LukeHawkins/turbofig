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

- **MCP HTTP server** (port 18846): speaks the MCP streamable-http protocol via `rmcp`. Uses `legacy_session_mode` so that clients on the 2025-03-26 spec can supply an `mcp-session-id` header. Each MCP session is stateful. Each tool reads the `mcp-session-id` from the HTTP request parts and maps the session to a plugin connection by `fileKey` (Phase 4).
- **WebSocket server** (port 18847): holds the persistent connection from the Figma plugin. The plugin sends `FILE_INFO` (`fileKey` + name) on connect. The daemon routes a tool call to the plugin by sending a request over this socket and awaiting a `RESULT`. A per-request timeout (`TURBOFIG_REQUEST_TIMEOUT_MS`, default 30000) stops a silent plugin from hanging a call. The daemon holds one connection per open file in a `conn_id`-keyed registry. On socket close it drops only that connection and fails only that connection's in-flight requests at once, so other files keep working.
- **File-bridge** (default `~/.turbofig`): watches `inbox/` and writes `outbox/`. A client writes a job file and reads the result file, so no curl and no MCP connection are needed. This is the primary transport for locked-down Claude Enterprise accounts. It claims each job (removes the inbox file) before running it, and runs each job in its own task so one slow job never blocks the loop. The ops are `status`, `execute`, `get_selection`, and `screenshot`. Screenshot file-mode writes the PNG into `outbox/<requestId>.png` (`AppState.screenshot_dir`) and returns its path; a subagent reads it. The outbox is ephemeral: the daemon reuses request-id filenames across restarts, so read a screenshot promptly.

Each capability has one shared `run_*` routine. Both the MCP tool and the file-bridge op call the same routine: `run_status`, `run_execute`, `run_get_selection`, and `run_screenshot`. This is the pattern Phase 2 set with `run_status`.

The daemon is always-on. A launchd service starts it at login and `KeepAlive` restarts it on crash. It is decoupled from any client session, so a client disconnect or session end never stops it.

## Plugin

The plugin has two threads, as required by the Figma plugin model:

- **Main thread** (`plugin/src/code.ts`): runs in the Figma sandbox. Has access to the Figma Plugin API. Receives messages from the UI thread and executes Figma API calls.
- **UI thread** (`plugin/src/ui/` → built to `dist/ui.html`): runs in a sandboxed iframe. Holds the WebSocket connection to the daemon. Relays messages between the daemon and the main thread via `parent.postMessage` / `figma.ui.onmessage`. The panel has three screens toggled by `.screen`/`.active` class swap, each with its own `figma.ui.resize` call via a `RESIZE` message: **main** (300×200, default, wordmark + status + file row + footer nav), **advanced** (300×440, taste profile selector + activity log + port field), **about** (300×260, large wordmark + tagline + description + repo/author links). The header wordmark uses a pure-CSS motion ghost effect (`text-shadow` with `color-mix(in srgb, var(--text) N%, transparent)`) so it is theme-aware without JS — light and dark mode both work via the injected `--figma-color-*` tokens. All font-size values must be on the impeccable type scale (12, 14, 16, 20, 25, 31, 39, 49, 61 px); the test suite enforces this automatically.

The plugin dispatches on a `{type}` field in each message:

| Type | Direction | Description |
|---|---|---|
| `FILE_INFO` | plugin to daemon | Sent on connect and after a profile change: fileKey, root name, and `profileId` (Phase 2; `profileId` added Phase 8) |
| `STATUS` | daemon to plugin | Liveness ping carrying a `requestId` (Phase 2) |
| `RESULT` | plugin to daemon | Reply carrying the matching `requestId` (Phase 2) |
| `EXECUTE` | daemon to plugin | Run arbitrary Figma Plugin API JS (Phase 3) |
| `GET_SELECTION` | daemon to plugin | Return compact selection info (Phase 3) |
| `SCREENSHOT` | daemon to plugin | Export PNG (Phase 3) |
| `SET_PROFILE` | UI iframe to plugin main thread | Writes the profile id to `figma.root.setPluginData("turbofig:profile", id)` then re-emits `FILE_INFO` with the updated `profileId` (Phase 8) |

This dispatch table is hybrid-ready. A community-safe command vocabulary is additive: add new types without reworking the existing structure.

`EXECUTE` runs the JS as an async function built with the Function constructor (validated in Figma's sandbox, see `DECISIONS.md` #17). Three things are injected into the eval scope before user code runs: the sync-to-async deprecation preamble (runs first), the `tf` craft namespace (`createTf(figma)`, passed as a second parameter beside `figma`), and the active file's taste profile (`tf.taste`). The daemon wraps the profile in an IIFE and assigns the result to `tf.taste` before appending user code. So generated code calls `figma.*`, `tf.*`, and `tf.taste.*` directly. Eval errors return a clean message and never crash the plugin.

## Routing registry (Phase 4)

The daemon keeps a connection registry keyed by two dimensions:

- `mcp-session-id`: assigned at the MCP `initialize` call; identifies a Claude session.
- `fileKey`: sent by the plugin on connect; identifies an open Figma file.

Each MCP session is paired to a `fileKey` by an explicit pick or the sole connected plugin. `resolve_route` picks the target connection by explicit `fileKey`, then the session pairing, then the sole named connection. It returns a clear error for no plugin, ambiguous target, or a not-connected file. A socket close cancels only that connection's in-flight requests. The registry handles N sessions and N files concurrently, fully isolated.

## Helper layer

The `tf` namespace is a compact JS craft library injected into every eval. Source lives in `plugin/src/helpers.ts` (pure logic unit-tested; figma glue smoke-tested), exposed via `createTf(figma)` and passed into the eval as `tf`. The compact API reference is `helpers/tf-api.md` (this is what the model reads to learn the helpers cheaply). The library is complete as of Phase 6. Categories and members:

- Layout primitives: `frame` (per-axis sizing, transparent by default), `rect`, `append`, `clear`, `findOrCreate` (idempotent-by-name).
- Text and fonts: `text` (font-load, optional `width` for wrapping), `loadFonts`, `color`, `solid`.
- Decks and slides: `deck`, `slide`, `slidePosition`, `chunk`.
- Component instances: `instance`, `instanceByKey`.
- Variables: `getVariable`, `setVariableValue`, `readVariableValue`.
- Export: `export`.
- Utilities: `skipInvisible`, `findAll`, `commit`.
- Runtime-injected: `tf.taste` (the active file's taste profile; set by the daemon before user code runs).

All functions use the async Figma API surface required by `documentAccess: dynamic-page`.

Idempotency pattern for re-runnable sections: `findOrCreate(parent, name, factory)` then `clear(node)` then rebuild. `findOrCreate` protects only the named node, so `clear` before rebuilding prevents duplicated children on a resume.

## Design orchestration

`.claude/commands/design.md` is the `/design` command: a brief becomes a full page via plan-first spec (persisted to `~/.turbofig/design/<job-id>/plan.json` + `status.json` as the checkpoint) -> parallel firewalled builder subagents (each one batched `tf.*` call, unique per-request bridge id) -> an assembly step that stacks sections in order (parallel builds otherwise overlap at 0,0) -> a QA critic subagent that reads the screenshot and returns text only -> a capped refine loop -> resume from the last completed section. Images live and die in subagents; the orchestrator never holds a screenshot.

## Skill layer

`skills/` (Phase 7+): the design-worker recipes and swappable per-file taste profiles. Profiles live in `skills/profiles/`. Three built-ins ship with the product: the impeccable default, editorial, and minimal. Custom profiles go in `TURBOFIG_PROFILES_DIR` (default `~/.turbofig/profiles`); the daemon scans that directory at startup. Each open file tracks its active profile via plugin data. The daemon prepends the active profile to the eval payload as `tf.taste`.

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
| `TURBOFIG_PROFILES_DIR` | `~/.turbofig/profiles` | Scanned at daemon startup for custom `.js` taste profiles |
