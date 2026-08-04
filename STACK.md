# Stack

## Rust (daemon/)

In Cargo.toml now (Phase 0 and Phase 1):
- `tokio` 1 (features = ["rt-multi-thread", "net", "macros"])
- `rmcp` 3.1.0 (features = ["server", "macros", "transport-streamable-http-server"]; `legacy_session_mode` is a config field, not a feature)
- `axum` 0.8
- `serde_json` 1
- `reqwest` 0.12 (features = ["json"]) — dev-dependency, for the transport integration test

Phase 2+ (not yet added):
- `tokio-tungstenite` 0.29
- `image` 0.25
- `rustls`

## TypeScript (plugin/)

- TypeScript 5.6
- `@figma/plugin-typings` (latest)

## Tooling

- Bun (latest)
- Biome 2.4

## Notes

- rmcp 3.1.0, axum 0.8, and serde_json are in Cargo.toml (Phase 1, done). tokio-tungstenite, image, and rustls are not yet added (Phase 2+).
- Never use npm, pnpm, or yarn. Bun only.
