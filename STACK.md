# Stack

## Rust (daemon/)

Phase 0 only:
- `tokio` 1 (features = ["rt-multi-thread", "net", "macros"])
- `serde_json` 1

Phase 1 (now in Cargo.toml):
- `rmcp` 3.1.0 (transport-streamable-http-server, legacy_session_mode)
- `axum` 0.8
- `serde_json` 1

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

- rmcp 3.1.0 and axum 0.8 are in Cargo.toml (Phase 1, done). tokio-tungstenite, image, and rustls are not yet added (Phase 2+).
- Never use npm, pnpm, or yarn. Bun only.
