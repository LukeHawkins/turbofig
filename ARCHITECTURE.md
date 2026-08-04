# Architecture

## Transport chain

```
Claude / AI client
    |
    | HTTP POST /mcp  (port 18846, MCP streamable-http, SSE response)
    |
Rust daemon  (daemon/)
    |
    | WebSocket  (port 18847)
    |
Figma plugin main thread  (plugin/src/code.ts)
    |
    | postMessage / onmessage
    |
Figma plugin UI thread  (plugin/src/ui.html)
    |
    | Figma Plugin API
    v
Figma document
```

## Daemon

The daemon is a single Rust process. It runs the MCP HTTP server today. The WebSocket server arrives in Phase 2.

- **MCP HTTP server** (port 18846): speaks the MCP streamable-http protocol via `rmcp`. Uses `legacy_session_mode` so that clients on the 2025-03-26 spec can supply an `mcp-session-id` header. Each MCP session is stateful. The daemon maps the session ID to a plugin connection by `fileKey` in Phase 4.
- **WebSocket server** (port 18847, Phase 2, not yet built): holds persistent connections from the Figma plugin. Each connection carries a `fileKey` sent in a `FILE_INFO` message on connect. The daemon routes tool calls to the right plugin connection.

The daemon becomes always-on in Phase 2. It starts at login via a launchd service and never exits on reconnect.

## Plugin

The plugin has two threads, as required by the Figma plugin model:

- **Main thread** (`plugin/src/code.ts`): runs in the Figma sandbox. Has access to the Figma Plugin API. Receives messages from the UI thread and executes Figma API calls.
- **UI thread** (`plugin/src/ui.html`): runs in a sandboxed iframe. Holds the WebSocket connection to the daemon. Relays messages between the daemon and the main thread via `parent.postMessage` / `figma.ui.onmessage`.

The plugin dispatches on a `{type}` field in each message:

| Type | Description |
|---|---|
| `FILE_INFO` | Sent on connect: fileKey + root name (Phase 2) |
| `EXECUTE` | Run arbitrary Figma Plugin API JS (Phase 3) |
| `GET_SELECTION` | Return compact selection info (Phase 3) |
| `SCREENSHOT` | Export PNG (Phase 3) |

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
