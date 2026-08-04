# Stack

## Rust (daemon/)

Phase 0 only:
- `tokio` 1 (features = full)
- `serde_json` 1

Phase 1 additions (not yet in Cargo.toml):
- `rmcp` 3.1.0 (transport-streamable-http-server, legacy_session_mode)
- `axum` 0.8
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

- rmcp, axum, tokio-tungstenite, image, and rustls are added in Phase 1. They are not in the Phase 0 Cargo.toml.
- Never use npm, pnpm, or yarn. Bun only.
