# Turbofig

The always-on Figma design agent: blazing fast, token-light, never re-pair a plugin. One Rust daemon + a thin eval-first Figma plugin. An AI acts as a senior designer that builds real Figma work from a brief or a moodboard.

**This file is the hub.** Read a branch only when the task needs it. The *why* behind choices lives in `DECISIONS.md`. The original phase plan is retired; see git history.

---

## Architecture (one screen)

```
Claude/AI  --curl / native MCP / file-bridge-->  Rust daemon  --WebSocket-->  Figma plugin  -->  Figma
             POST /mcp :18846  |  ~/.turbofig                  :18847           (eval)
```

- **Daemon (Rust, `daemon/`):** one always-on process running three servers as three `tokio::spawn`s that share one `Arc<AppState>`: the MCP HTTP endpoint (`rmcp`, streamable-http, `legacy_session_mode`, stateful `mcp-session-id`), a WebSocket server for the plugins (one connection per open file), and a file-bridge that watches `~/.turbofig/inbox` and writes `outbox`. The file-bridge lets a locked-down client drive the daemon with file writes and reads only (no curl, no MCP). A per-request timeout stops a silent plugin hanging a call. A `conn_id`-keyed connection registry holds N plugins at once. `resolve_route` sends each call to the right file by explicit `fileKey`, by the session-to-file pairing, or by the sole connected plugin. A socket close cancels only that connection's in-flight requests, so files stay isolated. On plugin connect the daemon pushes a `WELCOME` message with its version and its HTTP MCP port (`mcpPort`), and it stamps the resolved `sessionId` (empty for the file-bridge) onto every outbound request, so the panel can show the daemon version and the bound session. The HTTP port is self-describing: a plain GET on `/` or any non-`/mcp` path returns a short plain-text help payload (`HELP_TEXT`) that names the four tools, the file-bridge protocol, the `fileKey` targeting model, and the ports, so an AI told only the port can bootstrap without the repo. Both ports validate Origin: the HTTP port rejects any request carrying an Origin header with 403 (a real MCP client never sends one, only a browser does); the WS port rejects the upgrade for any Origin other than absent or `"null"` (the Figma plugin UI iframe reports a null origin). The file-bridge directories (`~/.turbofig`, `inbox/`, `outbox/`) are set to mode 0700 on every start.
- **Plugin (TypeScript, `plugin/`):** thin. UI iframe holds the WebSocket + infinite-backoff reconnect; main thread runs the Figma API. Main-thread dispatch on a `{type}` message: `EXECUTE` (eval), `GET_SELECTION`, `SCREENSHOT`, `FILE_INFO`, `SET_PORT`, `RESIZE` (`figma.ui.resize` when the panel switches screens). `SET_PORT` persists the daemon port in `figma.clientStorage` (per user) and the UI reconnects; the UI reads a `WELCOME`/`PORT`/`sessionId` from the daemon side. The panel matches the Figma theme via the injected `--figma-color-*` variables (it calls `figma.showUI` with `themeColors: true`), so it follows light and dark mode live. It has three screens toggled by a `.screen`/`.active` class swap: a compact main screen, an advanced screen and an About screen. A gear icon in the header opens advanced; a back link returns. Switching screens posts a `RESIZE` message so the main thread calls `figma.ui.resize` (main 300x150, advanced 300x240), so the window is short by default. The main-screen header shows the `turbofig` wordmark, the plugin version, and the connection status (`role="status"`, `aria-live`). The File row shows the file name and two inline icon buttons: a ghost clipboard icon copies the `fileKey`, and a brand-tinted icon copies a compact, token-light Claude Code connect prompt. The prompt is file-bridge-first (write a JSON job to `~/.turbofig/inbox`, read `outbox`, target by `fileKey`) because the file-bridge is the fastest, most token-efficient, dialog-free path; it lists MCP only as a labelled fallback and warns that the daemon is plain HTTP, so an agent must use curl, never a web-fetch tool (which forces HTTPS and fails). Both copies use a hidden-`textarea` + `document.execCommand("copy")` because `navigator.clipboard` is unreliable in the Figma iframe. The `WELCOME` message carries `mcpPort` (from `port_from_env`) so the MCP-fallback line stays accurate under a custom `TURBOFIG_MCP_PORT`. A bound-session line hides when idle. The advanced screen holds the activity log and the configurable daemon port. Reading `figma.fileKey` needs `enablePrivatePluginApi: true` in the manifest; without it the panel gets an empty `fileKey`. The UI is built from `src/ui/` (a pure `ui-logic` module + a browser `main` runtime + an HTML template) by `build-ui.ts` into `dist/ui.html`; `code.ts` and the UI both build as classic IIFE scripts so Figma can load them.
- **Helpers (JS, `helpers/`):** compact craft library injected into the eval context (auto-layout, decks, components, variables, perf rules).
- **Skills (`skills/`):** the design-worker recipes, led by `design.md` (the `/design` command: brief → plan → parallel builder subagents → QA → refine).

Tool surface (locked, 4): `turbofig_execute`, `turbofig_get_selection`, `turbofig_screenshot`, `turbofig_status`. All capability flows through `execute`. Never grow this. Each tool takes an optional `fileKey` to target one of several open files; omit it to use the paired or sole file. Read shaping is opt-in per call: `get_selection` takes optional `fields` (extra node props) and `depth` (child traversal, capped at 5); `screenshot` takes `maxDim` (downscale cap, default 1200) and `fullRes` (skip downscaling), and defaults to file mode. Large reads and inline screenshots over budget return an advisory `warning`.

## Always-on rules

- **STE.** Simplified Technical English in all prose. Active voice, short sentences, one idea each. No em dashes. No hedging.
- **Commits.** One logical change per commit, tests in the same commit. Imperative message, no `feat:`/`chore:` prefixes. **Never** add Co-Authored-By, Signed-off-by, or any AI attribution. Author is the git config only (Luke Hawkins <hi@lukehawkins.eu>).
- **Verify before commit.** Rust: `cargo build` + `cargo test` + `cargo clippy`. TS: `bun run typecheck` + `bun test`. The `lint` script must pass.
- **Tests with every change.** Every item that adds behaviour ships with tests in the same commit. Tests are never deferred to a later phase. Untested behaviour is not done.
- **Read before you write.** Always read a file before modifying it.
- **Rebuild plugin UI after changes.** After editing any file under `plugin/src/`, run `cd plugin && bun run build` to regenerate `dist/ui.html`. Do this before reporting the task done.
- **Keep this file current.** Any commit that changes architecture, ports, the tool surface, files, or data flow updates this file in the same commit.
- **Module layout.** `ARCHITECTURE.md` is the source of truth for the module layout. It is under active refactor, so this file does not copy it. Read `ARCHITECTURE.md` for current file and module structure.

## Token discipline (the product's whole point)

- Shaped returns: ids-first, opt-in `fields`, `depth` limit. Never dump a full node tree by default.
- Screenshots: downscaled + file-mode by default; inline high-res only on request; milestone-only, never per-step.
- Subagent firewall: anything that looks at Figma runs in a disposable subagent; images never reach the main context.
- Batch: many node ops per `execute` call.

## Tooling

- **Rust** (daemon): Cargo, clippy, rustfmt. Crates now: `rmcp`, `axum` (`ws` feature), `tokio` (`sync`, `time`, `fs`), `serde_json`, `serde`, `futures-util`, `base64` (decode screenshot PNG), `notify` (event-driven file-bridge wakes), `http` (read the `mcp-session-id` header from the request parts for session routing), `image` (`png` only, downscale screenshots to a longest-edge cap). Dev: `reqwest`, `tokio-tungstenite` (test WS client), `tempfile`. Later crates: `rustls` (Phase 11).
- **Bun** (plugin + scripts): never npm/pnpm/yarn. Biome for TS lint+format. TypeScript strict.
- **Ports** are a product contract, not dev servers: HTTP `18846`, WS `18847`, both env-overridable. This intentionally overrides the usual "randomised high ports" rule (see `DECISIONS.md`). Two more env vars: `TURBOFIG_REQUEST_TIMEOUT_MS` (default 30000) and `TURBOFIG_BRIDGE_DIR` (default `~/.turbofig`).

## Branches (read on demand)

- `DECISIONS.md`: the why behind big choices (eval-first, Rust, ports, distribution).
- `ARCHITECTURE.md`: deeper architecture and data flow.
- `STACK.md`: the exact stack and versions.
- `docs/discoverability/`: the shippable discoverability hook. A `CLAUDE-snippet.md` a user pastes into a global CLAUDE.md, and an `mcp-config.json` `mcpServers` entry for allowlisted native MCP users.
