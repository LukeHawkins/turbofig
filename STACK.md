# Stack

## Rust (daemon/)

In Cargo.toml now (through Phase 2):
- `tokio` 1 (features = ["rt-multi-thread", "net", "macros", "sync", "time", "fs"])
- `rmcp` 3.1.0 (features = ["server", "macros", "transport-streamable-http-server"]; `legacy_session_mode` is a config field, not a feature)
- `axum` 0.8 (features = ["ws"]): the ws feature serves the plugin WebSocket
- `serde_json` 1
- `serde` 1 (features = ["derive"])
- `futures-util` 0.3: WebSocket sink/stream split
- `reqwest` 0.12 (features = ["json"]): dev-dependency, transport integration test
- `tokio-tungstenite` 0.24: dev-dependency, a WebSocket client for tests
- `tempfile` 3: dev-dependency, temp dirs for the file-bridge tests

Phase 3+ (not yet added):
- `image` 0.25
- `rustls`

## TypeScript (plugin/)

- TypeScript 5.6
- `@figma/plugin-typings` (latest)

## Tooling

- Bun (latest)
- Biome 2.4

## Notes

- The WebSocket server (Phase 2) uses the axum `ws` feature, not a `tokio-tungstenite` server dep. `tokio-tungstenite` is a dev-dependency: a WebSocket client for the integration tests.
- `image` and `rustls` are not yet added (Phase 3+).
- Never use npm, pnpm, or yarn. Bun only.
