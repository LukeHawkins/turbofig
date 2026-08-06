# Turbofig

The always-on Figma design agent: blazing fast, token-light, never re-pair a plugin. One Rust daemon + a thin eval-first Figma plugin. An AI acts as a senior designer that builds real Figma work from a brief or a moodboard.

**This file is the hub.** Read a branch only when the task needs it. Build order and tasks live in `PLAN.md`. The *why* behind choices lives in `DECISIONS.md`.

---

## Architecture (one screen)

```
Claude/AI  --curl / native MCP / file-bridge-->  Rust daemon  --WebSocket-->  Figma plugin  -->  Figma
             POST /mcp :18846  |  ~/.turbofig                  :18847           (eval)
```

- **Daemon (Rust, `daemon/`):** one always-on process running three servers as three `tokio::spawn`s that share one `Arc<AppState>`: the MCP HTTP endpoint (`rmcp`, streamable-http, `legacy_session_mode`, stateful `mcp-session-id`), a WebSocket server for the plugins (one connection per open file), and a file-bridge that watches `~/.turbofig/inbox` and writes `outbox`. The file-bridge lets a locked-down client drive the daemon with file writes and reads only (no curl, no MCP). A per-request timeout stops a silent plugin hanging a call. A `conn_id`-keyed connection registry holds N plugins at once. `resolve_route` sends each call to the right file by explicit `fileKey`, by the session-to-file pairing, or by the sole connected plugin. A socket close cancels only that connection's in-flight requests, so files stay isolated.
- **Plugin (TypeScript, `plugin/`):** thin. UI iframe holds the WebSocket + infinite-backoff reconnect; main thread runs the Figma API. Dispatches on a `{type}` message: `EXECUTE` (eval), `GET_SELECTION`, `SCREENSHOT`, `FILE_INFO`.
- **Helpers (JS, `helpers/`):** compact craft library injected into the eval context (auto-layout, decks, components, variables, perf rules).
- **Skills (`skills/`):** the design-worker recipes + the generic taste baseline. User brand packs load on top.

Tool surface (locked, 4): `turbofig_execute`, `turbofig_get_selection`, `turbofig_screenshot`, `turbofig_status`. All capability flows through `execute`. Never grow this. Each tool takes an optional `fileKey` to target one of several open files; omit it to use the paired or sole file. Read shaping is opt-in per call: `get_selection` takes optional `fields` (extra node props) and `depth` (child traversal, capped at 5); `screenshot` takes `maxDim` (downscale cap, default 1200) and `fullRes` (skip downscaling), and defaults to file mode. Large reads and inline screenshots over budget return an advisory `warning`.

## Always-on rules

- **STE.** Simplified Technical English in all prose. Active voice, short sentences, one idea each. No em dashes. No hedging.
- **Commits.** One commit per PLAN.md item. Imperative message, no `feat:`/`chore:` prefixes. **Never** add Co-Authored-By, Signed-off-by, or any AI attribution. Author is the git config only (Luke Hawkins <hi@lukehawkins.eu>).
- **Verify before commit.** Rust: `cargo build` + `cargo test` + `cargo clippy`. TS: `bun run typecheck` + `bun test`. The `lint` script must pass.
- **Tests with every change.** Every item that adds behaviour ships with tests in the same commit. Tests are never deferred to a later phase. Untested behaviour is not done.
- **Read before you write.** Always read a file before modifying it.
- **Keep this file current.** Any commit that changes architecture, ports, the tool surface, files, or data flow updates this file in the same commit.
- **Subagents, parallel by default.** Run the top-level session on **Opus** as the orchestrator: it reasons, plans, verifies, and commits. `/phase` delegates each item to a **Sonnet** subagent for the bulk implementation. Dispatch independent items as parallel workers in one batch; go sequential only for real dependencies or shared files. Use Haiku/Explore for search. Reserve Opus itself only for the transport/session and routing design (Phase 1 and Phase 4 tricky bits). Give each worker only the context it needs (paths, not file contents).

## Token discipline (the product's whole point)

- Shaped returns: ids-first, opt-in `fields`, `depth` limit. Never dump a full node tree by default.
- Screenshots: downscaled + file-mode by default; inline high-res only on request; milestone-only, never per-step.
- Subagent firewall: anything that looks at Figma runs in a disposable subagent; images never reach the main context.
- Batch: many node ops per `execute` call.

## Tooling

- **Rust** (daemon): Cargo, clippy, rustfmt. Crates now: `rmcp`, `axum` (`ws` feature), `tokio` (`sync`, `time`, `fs`), `serde_json`, `serde`, `futures-util`, `base64` (decode screenshot PNG), `notify` (event-driven file-bridge wakes), `http` (read the `mcp-session-id` header from the request parts for session routing), `image` (`png` only, downscale screenshots to a longest-edge cap). Dev: `reqwest`, `tokio-tungstenite` (test WS client), `tempfile`. Later crates: `rustls` (Phase 10).
- **Bun** (plugin + scripts): never npm/pnpm/yarn. Biome for TS lint+format. TypeScript strict.
- **Ports** are a product contract, not dev servers: HTTP `18846`, WS `18847`, both env-overridable. This intentionally overrides the usual "randomised high ports" rule (see `DECISIONS.md`). Two more env vars: `TURBOFIG_REQUEST_TIMEOUT_MS` (default 30000) and `TURBOFIG_BRIDGE_DIR` (default `~/.turbofig`).

## Branches (read on demand)

- `PLAN.md`: phased build plan. Run with `/phase N`.
- `DECISIONS.md`: the why behind big choices (eval-first, Rust, ports, distribution).
- `ARCHITECTURE.md`: deeper architecture and data flow.
- `STACK.md`: the exact stack and versions.
- `.claude/commands/`: `phase`, `plan`, `kickoff`, `audit`, `harden`, `goodbye` (and `design`, added in Phase 7).
