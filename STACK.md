# Stack

## Rust (daemon/)

In Cargo.toml now:
- `tokio` 1 (features = ["rt-multi-thread", "net", "macros", "sync", "time", "fs"])
- `rmcp` 3.1.0 (features = ["server", "macros", "transport-streamable-http-server"]; `legacy_session_mode` is a config field, not a feature)
- `axum` 0.8 (features = ["ws"]): the ws feature serves the plugin WebSocket
- `serde_json` 1
- `serde` 1 (features = ["derive"])
- `futures-util` 0.3: WebSocket sink/stream split
- `base64` 0.22: decode the plugin's PNG screenshot bytes
- `notify` 6: event-driven file-bridge wakes (FSEvents / inotify), no busy polling
- `http` 1: reads the `mcp-session-id` header from MCP HTTP request parts for session routing
- `image` 0.25 (default-features off, features = ["png"]): downscale screenshots to a longest-edge cap
- `reqwest` 0.12 (features = ["json"]): dev-dependency, transport integration test
- `tokio-tungstenite` 0.24: dev-dependency, a WebSocket client for tests
- `tempfile` 3: dev-dependency, temp dirs for the file-bridge tests

Not yet added:
- `rustls` (musl and cross-platform builds)

## TypeScript (plugin/)

- TypeScript 5.6
- `@figma/plugin-typings` (latest)

## Tooling

- Bun (latest)
- Biome 2.4

## Notes

- The WebSocket server uses the axum `ws` feature, not a `tokio-tungstenite` server dep. `tokio-tungstenite` is a dev-dependency: a WebSocket client for the integration tests.
- `image` handles PNG-only screenshot downscaling. `rustls` is not yet added.
- Never use npm, pnpm, or yarn. Bun only.
