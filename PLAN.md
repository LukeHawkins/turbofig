# Turbofig — Build Plan

**Mission:** the always-on Figma design agent that is blazing fast, token-light, and never makes you re-pair a plugin. One Rust binary + a thin eval-first Figma plugin. An AI acts as a senior designer: hand it a moodboard or a brief and it builds real Figma work, cheaply and fast.

Run a phase with `/phase N`. Each `- [ ]` item is one commit, independently verifiable, in dependency order. Record *why* behind big choices in `DECISIONS.md`, not here.

---

## Guiding principles

- **Eval-first.** One tool runs arbitrary Figma Plugin API JS (`turbofig_execute`). ~4 tools total, not 98. All capability flows through eval. New Figma APIs work same-day.
- **Always-on.** The daemon never dies. Fixed ports, infinite-backoff reconnect. The plugin re-pairs silently. Zero handshake ritual, ever. This is priority one.
- **Token-first.** Every design choice is judged on token cost. Shaped compact returns, downscaled + milestone-only screenshots, subagent context firewalling, batched execute. Measured against the console-mcp baseline, not assumed.
- **Subagent firewall.** Anything that must *look* at Figma runs in a disposable subagent. Screenshots live and die there. The main context stays lean.
- **Prove it is FAR better.** Phase 5 gates on measured token + speed wins vs the current stack. If it is not far better, it does not ship.
- **Hybrid-ready.** The plugin dispatches on a `{type}` field. v1 ships `EXECUTE` (eval, dev-install). A community-safe vocabulary build is additive later, no rework.
- **Maintainable by design.** Tiny tool surface, no per-API upkeep, generic taste baseline that users train on top of.
- **STE + one commit per item.** Simplified Technical English in all prose. No em dashes. No Co-Authored-By on commits.

## Model usage guide

| Work | Model |
|---|---|
| Architecture, the rmcp/session transport, routing design | Opus |
| Implementation, plugin code, helpers, CRUD, refactors | Sonnet |
| Search, lookups, file finds | Haiku (Explore) |

`/phase` delegates each item to a Sonnet subagent by default, orchestrated by the main session. See `.claude/commands/phase.md`.

## Port contract (deliberate deviation from boilerplate)

Turbofig serves a user-facing protocol, not a dev server. Default HTTP MCP port **3846** (drop-in for existing curl skills), default WebSocket port **3847**. Both overridable by env (`TURBOFIG_MCP_PORT`, `TURBOFIG_WS_PORT`). See `DECISIONS.md`.

## Testing policy

Tests ship with every item, in the same commit. Never defer tests to a later phase. An item is done only when its new behaviour has a test that runs and passes: Rust via `cargo test` (`#[test]` / `#[tokio::test]`), plugin/JS logic via `bun test`. `/phase` verifies the tests exist and are real (not empty stubs) before it commits. The Phase 12 hardening pass raises coverage; it does not introduce the first tests.

## Commit hygiene (enforced)

Author is the git config only: Luke Hawkins <hi@lukehawkins.eu>. A committed `commit-msg` hook (`.githooks/commit-msg`) rejects any `Co-Authored-By` / `Signed-off-by` trailer. Enable it once per clone: `git config core.hooksPath .githooks`.

---

## Phase 0 — Scaffolding & toolchain

Goal: a green, committable skeleton. Rust daemon crate + TS plugin package in one repo, tooling wired.

- [x] Replace all `{PLACEHOLDER}` tokens across CLAUDE.md, README.md, STACK.md, ARCHITECTURE.md
- [x] Cargo workspace + `daemon` crate: `cargo build` produces a binary that starts and exits cleanly
- [x] Bun workspace root `package.json` + `plugin` package: `bun install` + plugin typecheck pass
- [x] Wire `biome.json`, `rustfmt.toml`, clippy in CI; `lint` script covers both Rust and TS
- [x] GitHub Actions CI: build daemon + typecheck plugin + lint + test on push
- [x] `.gitignore` covers `target/`, `node_modules/`, `dist/`, `.env`
- [x] First commit: working skeleton, CI green

## Phase 1 — Transport de-risk (day-one risk)

Goal: prove the exact curl contract works in Rust before building on it. Kills the one rmcp unknown (#1108).

- [ ] Daemon serves `POST /mcp` via `rmcp` streamable-http + `legacy_session_mode`
- [ ] `initialize` returns an `mcp-session-id` response header; subsequent calls require + reuse it (stateful)
- [ ] Responses are SSE `data:` lines with body `{result:{content:[{text}]}}`
- [ ] One tool `turbofig_status` returns `{ok:true}` via `tools/call`
- [ ] Test script reproduces the exact curl handshake from the current skills and passes
- [ ] Fallback documented: if `legacy_session_mode` fails, switch to hand-rolled axum SSE (record in DECISIONS.md)

## Phase 2 — Always-on daemon + silent plugin pairing (pain #1)

Goal: end the handshake permanently. Daemon holds the WebSocket beside HTTP; the plugin re-pairs on its own.

- [ ] Daemon hosts a WebSocket server on `TURBOFIG_WS_PORT` beside the HTTP MCP endpoint (two `tokio::spawn`s, one process)
- [ ] Minimal Figma plugin: `manifest.json` (`documentAccess: dynamic-page`, `api` pinned), WS client
- [ ] Plugin auto-reconnects with infinite exponential backoff (cap ~30s); no 5-retry cap
- [ ] Plugin sends `FILE_INFO` (`fileKey` + `root.name`) on connect
- [ ] Daemon routes `turbofig_status` through to the live plugin and back
- [ ] Always-on install: a launchd/login-item service starts the daemon at login
- [ ] Verify: restart the daemon AND restart Figma; the plugin re-pairs with zero manual steps

## Phase 3 — Eval-first vertical slice (usable daily)

Goal: a real, usable create+read+screenshot loop end to end.

- [ ] Plugin `{type}` dispatch table; add `EXECUTE` (async IIFE eval, timeout, result via `requestId`)
- [ ] `turbofig_execute` tool: send JS, run it in the plugin, return the result
- [ ] `GET_SELECTION` + `turbofig_get_selection`: compact `{id,name,type,x,y,w,h}` shape
- [ ] `SCREENSHOT` + `turbofig_screenshot`: `exportAsync` PNG, param `{scale, return:"file"|"inline"}`
- [ ] Verify: from Claude via curl, create a frame in a live file, read selection, get a screenshot back
- [ ] Error boundary: eval failures return a clean message to the caller, never crash the daemon

## Phase 4 — True multi-file routing

Goal: N independent Claude sessions ↔ N files at once. Stronger than console-mcp's broadcast.

- [ ] Daemon session registry keyed by `mcp-session-id` and plugin connection `fileKey`
- [ ] Pair a session to a file (by `fileKey` or an explicit pick); route every call to the right plugin
- [ ] Handle multiple plugin connections (one per open file) simultaneously
- [ ] Graceful handling when a target file closes mid-session (clear error, no cross-talk)
- [ ] Verify: two files + two Claude sessions operate concurrently, fully isolated

## Phase 5 — Token & speed engine (the differentiator, MEASURED)

Goal: prove FAR better than console-mcp. This phase gates the whole project.

- [ ] Shaped returns: ids-first, opt-in `fields`, `depth` limit, never a full-tree dump by default
- [ ] Screenshots default to downscaled + file-mode; inline high-res only on request
- [ ] Batching helper: many node ops in one `execute` round-trip
- [ ] Benchmark harness: measure tokens + wall-time for "design a webpage" and "20-slide deck"
- [ ] Run the harness against the current console-mcp stack and record baseline numbers
- [ ] Gate: hit targets (≈10×+ token cut; deck under ~8 min) and record results in DECISIONS.md

## Phase 6 — Core helper library

Goal: reliable, fast Figma craft as a compact JS namespace injected into the eval context.

- [ ] Perf rules baked in: fonts via `Promise.all`, `skipInvisibleInstanceChildren`, `findAllWithCriteria`, `commitUndo`, chunk 50-100 nodes
- [ ] Layout primitives: auto-layout builders (vertical/horizontal, sizing, spacing, padding)
- [ ] Deck/slide scaffolds; component instantiation; variable read/write; text + font handling; export
- [ ] Async-only API surface (all `*Async` variants) for `documentAccess: dynamic-page`
- [ ] Compact API reference doc (one file) so the model learns the helpers cheaply
- [ ] Verify: build a multi-slide deck in a few batched calls using only helpers

## Phase 7 — Design-worker orchestration (the senior designer)

Goal: the agent workflow that turns a brief into real design, cheaply. Accepts context files.

- [ ] `.claude/commands/design.md`: plan-first spec → parallel builder subagents → QA critic subagent → refine
- [ ] Context intake: accept a moodboard image, brand docs, and reference URLs as inputs
- [ ] QA critic runs in a subagent (context firewall); returns a text critique, never raw images to the parent
- [ ] Rubric-driven critique (hierarchy, spacing scale, contrast/AA, alignment, restraint)
- [ ] Verify: hand it a moodboard + brief; it produces a coherent website design
- [ ] Verify: main-context token use for the run stays low (measured against Phase 5 harness)

## Phase 8 — Anti-slop baseline + trainability

Goal: good taste out of the box; trainable on top; redistributable without private context.

- [ ] Baseline taste pack (generic): spacing scale, type scale, grid, contrast/AA, hierarchy, restraint rules
- [ ] Anti-ai-slop guardrails: avoid the common tells (centered everything, generic gradients, emoji bullets, dead whitespace)
- [ ] Loadable brand/project packs (design tokens, components, rules) switchable at runtime
- [ ] Clean separation: the public build ships only the generic baseline, never Luke's private packs
- [ ] Verify: switch between two brand packs cleanly; generic build carries no private data

## Phase 9 — Slick Figma plugin UI

Goal: a clean, good-looking plugin panel. Clear connection state.

- [ ] Connection status (connected / reconnecting / offline) + active file + session
- [ ] Pairing view (which Claude session is bound to this file)
- [ ] Live activity log + a plugin-version / stale-warning indicator
- [ ] Visual polish: on-brand, tidy, no clutter

## Phase 10 — Packaging & distribution (pure export, easy updates)

Goal: users install a compiled binary with no source and no compiler; updates are trivial.

- [ ] CI build matrix: static binaries for macOS arm64 + x64, Linux x64 (musl, rustls), Windows x64
- [ ] npm publish: thin JS launcher `turbofig-mcp` + per-platform binary packages as `optionalDependencies` (esbuild/Biome pattern; no postinstall network fetch, works on locked-down networks)
- [ ] `npx turbofig-mcp` starts the daemon on any platform with no Rust toolchain
- [ ] Secondary channels: `cargo install turbofig-mcp`, a Homebrew tap, raw GitHub Release binaries + `curl | sh` installer
- [ ] Auto-update nudge: daemon checks for a newer version on startup; plugin warns on version mismatch on connect
- [ ] Plugin delivery: `npx turbofig-mcp plugin` prints/open the dev-install steps and the manifest path
- [ ] Play nice with Luke's setup: keep curl-on-3846 working AND register cleanly as a native `mcpServers` entry
- [ ] Migrate Luke's existing `figma-*` skills onto turbofig and confirm the daily workflow runs

## Phase 11 — Presentation (README-first)

Goal: newcomers discover, understand, and install from the README alone.

- [ ] README: positioning vs figma-mcp / figma-console-mcp / Framelink (a clear "why this" table)
- [ ] Quickstart: install → import plugin → first design, in under 5 minutes
- [ ] A screencast GIF of a moodboard → website design run
- [ ] Docs: brand-pack authoring, skill authoring, architecture overview
- [ ] Badges + crates.io/npm links + license

## Phase 12 — Hardening & maintainability

Goal: robust, testable, cheap to maintain.

- [ ] Deprecation preamble injected into the eval prompt (sync→async "do not use" table); a monthly changelog-watch note
- [ ] Error-feedback retry loop: a failed eval returns the error so the agent self-corrects
- [ ] Test suites: Rust (daemon routing/session) + plugin (dispatch/eval), co-located
- [ ] Benchmark regression guard in CI (token + speed budgets from Phase 5)
- [ ] `CONTRIBUTING.md` + a maintenance runbook

<!--
Add phases as the project grows. Each item:
  - is one commit when run via /phase
  - is independently verifiable
  - is in dependency order
Record the *why* behind big choices in DECISIONS.md, not here.
-->
