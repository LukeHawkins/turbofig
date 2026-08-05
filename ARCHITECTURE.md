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

- **MCP HTTP server** (port 18846): speaks the MCP streamable-http protocol via `rmcp`. Uses `legacy_session_mode` so that clients on the 2025-03-26 spec can supply an `mcp-session-id` header. Each MCP session is stateful. The daemon maps the session ID to a plugin connection by `fileKey` in Phase 4.
- **WebSocket server** (port 18847): holds the persistent connection from the Figma plugin. The plugin sends `FILE_INFO` (`fileKey` + name) on connect. The daemon routes a tool call to the plugin by sending a request over this socket and awaiting a `RESULT`. A per-request timeout (`TURBOFIG_REQUEST_TIMEOUT_MS`, default 30000) stops a silent plugin from hanging a call. On socket close the daemon drops the registration and fails any in-flight request at once.
- **File-bridge** (default `~/.turbofig`): watches `inbox/` and writes `outbox/`. A client writes a job file and reads the result file, so no curl and no MCP connection are needed. This is the primary transport for locked-down Claude Enterprise accounts. It claims each job (removes the inbox file) before running it, and runs each job in its own task so one slow job never blocks the loop.

`run_status` is the shared status routine. Both the MCP `turbofig_status` tool and the file-bridge `status` op call it.

The daemon is always-on. A launchd service starts it at login and `KeepAlive` restarts it on crash. It is decoupled from any client session, so a client disconnect or session end never stops it.

## Plugin

The plugin has two threads, as required by the Figma plugin model:

- **Main thread** (`plugin/src/code.ts`): runs in the Figma sandbox. Has access to the Figma Plugin API. Receives messages from the UI thread and executes Figma API calls.
- **UI thread** (`plugin/src/ui.html`): runs in a sandboxed iframe. Holds the WebSocket connection to the daemon. Relays messages between the daemon and the main thread via `parent.postMessage` / `figma.ui.onmessage`.

The plugin dispatches on a `{type}` field in each message:

| Type | Direction | Description |
|---|---|---|
| `FILE_INFO` | plugin to daemon | Sent on connect: fileKey + root name (Phase 2) |
| `STATUS` | daemon to plugin | Liveness ping carrying a `requestId` (Phase 2) |
| `RESULT` | plugin to daemon | Reply carrying the matching `requestId` (Phase 2) |
| `EXECUTE` | daemon to plugin | Run arbitrary Figma Plugin API JS (Phase 3) |
| `GET_SELECTION` | daemon to plugin | Return compact selection info (Phase 3) |
| `SCREENSHOT` | daemon to plugin | Export PNG (Phase 3) |

This dispatch table is hybrid-ready. A community-safe command vocabulary is additive: add new types without reworking the existing structure.

## Routing registry (Phase 4, not yet built)

The daemon keeps a session registry keyed by two dimensions:

- `mcp-session-id`: assigned at the MCP `initialize` call; identifies a Claude session.
- `fileKey`: sent by the plugin on connect; identifies an open Figma file.

Each MCP session is paired to a `fileKey` (by explicit pick or first-connected default). All tool calls route to the plugin holding that `fileKey`. The registry handles N sessions and N files concurrently, fully isolated.

## Helper layer

`helpers/` (Phase 6): a compact JS namespace injected into the eval context. Provides auto-layout builders, deck/slide scaffolds, component instantiation, variable read/write, text and font handling, and export utilities. All functions use the async Figma API surface required by `documentAccess: dynamic-page`.

## Skill layer

`skills/` (Phase 7+): the design-worker recipes and the generic taste baseline. Brand/project packs load on top at runtime. The public build ships only the generic baseline.

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
