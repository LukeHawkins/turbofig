# Turbofig Build Plan

**Mission:** the always-on Figma design agent that is blazing fast, token-light, and never makes you re-pair a plugin. One Rust binary + a thin eval-first Figma plugin. An AI acts as a senior designer: hand it a moodboard or a brief and it builds real Figma work, cheaply and fast.

Run a phase with `/phase N`. Each `- [ ]` item is one commit, independently verifiable, in dependency order. Record *why* behind big choices in `DECISIONS.md`, not here.

---

## Guiding principles

- **Eval-first.** One tool runs arbitrary Figma Plugin API JS (`turbofig_execute`). ~4 tools total, not 98. All capability flows through eval. New Figma APIs work same-day.
- **Always-on and robust.** The daemon is an independent service (launchd), decoupled from any client session, so a session end or a client crash never kills an in-flight job. It restarts on crash (KeepAlive). Fixed ports, infinite-backoff reconnect, silent re-pair. Long jobs checkpoint and resume. This is priority one.
- **Token-first.** Every design choice is judged on token cost. Shaped compact returns, downscaled + milestone-only screenshots, subagent context firewalling, batched execute. Measured against the console-mcp baseline, not assumed.
- **Subagent firewall.** Anything that must *look* at Figma runs in a disposable subagent. Screenshots live and die there. The main context stays lean. This is enforced, not left to prompt discipline.
- **Prove it is FAR better.** Phase 5 takes a provisional baseline. The binding ship or no-ship gate runs after Phase 7, once the helpers, orchestration, and firewall exist. If it is not far better then, it does not ship.
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

Turbofig serves a user-facing protocol, not a dev server. Default HTTP MCP port **18846**, default WebSocket port **18847**. Both overridable by env (`TURBOFIG_MCP_PORT`, `TURBOFIG_WS_PORT`). See `DECISIONS.md`.

## Testing policy

Tests ship with every item, in the same commit. Never defer tests to a later phase. An item is done only when its new behaviour has a test that runs and passes: Rust via `cargo test` (`#[test]` / `#[tokio::test]`), plugin/JS logic via `bun test`. `/phase` verifies the tests exist and are real (not empty stubs) before it commits. The Phase 12 hardening pass raises coverage, it does not introduce the first tests.

## Commit hygiene (enforced)

Author is the git config only: Luke Hawkins <hi@lukehawkins.eu>. A committed `commit-msg` hook (`.githooks/commit-msg`) rejects any `Co-Authored-By` / `Signed-off-by` trailer. Enable it once per clone: `git config core.hooksPath .githooks`.

---

## Phase 0: Scaffolding & toolchain

Goal: a green, committable skeleton. Rust daemon crate + TS plugin package in one repo, tooling wired.

- [x] Replace all `{PLACEHOLDER}` tokens across CLAUDE.md, README.md, STACK.md, ARCHITECTURE.md
- [x] Cargo workspace + `daemon` crate: `cargo build` produces a binary that starts and exits cleanly
- [x] Bun workspace root `package.json` + `plugin` package: `bun install` + plugin typecheck pass
- [x] Wire `biome.json`, `rustfmt.toml`, clippy in CI; `lint` script covers both Rust and TS
- [x] GitHub Actions CI: build daemon + typecheck plugin + lint + test on push
- [x] `.gitignore` covers `target/`, `node_modules/`, `dist/`, `.env`
- [x] First commit: working skeleton, CI green

## Phase 1: Transport de-risk (day-one risk)

Goal: prove the exact curl contract works in Rust before building on it. Kills the one rmcp unknown (#1108).

- [x] Daemon serves `POST /mcp` via `rmcp` streamable-http + `legacy_session_mode`
- [x] `initialize` returns an `mcp-session-id` response header; subsequent calls require + reuse it (stateful)
- [x] Responses are SSE `data:` lines with body `{result:{content:[{text}]}}`
- [x] One tool `turbofig_status` returns `{ok:true}` via `tools/call`
- [x] Test script reproduces the exact curl handshake from the current skills and passes
- [x] Fallback documented: if `legacy_session_mode` fails, switch to hand-rolled axum SSE (record in DECISIONS.md)

## Phase 2: Always-on, robust daemon + silent plugin pairing (pain #1)

Goal: end the handshake permanently and make the daemon independent of any client session. It holds the WebSocket beside HTTP, re-pairs on its own, and survives client teardown and its own crashes.

- [x] Daemon hosts a WebSocket server on `TURBOFIG_WS_PORT` beside the HTTP MCP endpoint (two `tokio::spawn`s, one process)
- [x] Daemon runs as an independent service decoupled from any Claude session; a client disconnect or session end never stops it (the direct fix for the current supergateway SIGTERM-mid-job crash)
- [x] launchd `KeepAlive` restarts the daemon automatically on crash
- [x] Daemon-side request timeout: if the plugin does not reply within N seconds, return a clean error, never hang the call
- [x] Session reinit: on daemon restart the in-memory registry is lost, so a client with a stale `mcp-session-id` gets a clear reinitialize signal, not a silent failure
- [x] Minimal Figma plugin: `manifest.json` (`documentAccess: dynamic-page`, `api` pinned), WS client
- [x] Plugin auto-reconnects with infinite exponential backoff (cap ~30s); no 5-retry cap
- [x] Plugin sends `FILE_INFO` (`fileKey` + `root.name`) on connect
- [x] Daemon routes `turbofig_status` through to the live plugin and back
- [x] Always-on install: a launchd/login-item service starts the daemon at login
- [x] Verify: restart the daemon AND restart Figma; the plugin re-pairs with zero manual steps

## Phase 3: Eval-first vertical slice (usable daily)

Goal: a real, usable create+read+screenshot loop end to end. This is the first "actually try it" milestone. It MUST be reachable dialog-free through the file-bridge, because native MCP is admin-blocked and curl is gated on Luke's account (see `DECISIONS.md` #15). Each capability shares one `run_*` function between the MCP tool and the file-bridge op, the pattern Phase 2 set with `run_status`.

- [x] Plugin `{type}` dispatch table; add `EXECUTE` (async IIFE eval, timeout, result via `requestId`)
- [x] `turbofig_execute` tool: send JS, run it in the plugin, return the result. Factor `run_execute` so the file-bridge shares it.
- [x] File-bridge ops for the full loop: add `execute`, `get_selection`, `screenshot` job ops that call the same `run_*` functions as the tools, so the whole loop works with file writes and reads only (no curl, no MCP).
- [x] Inject the sync-to-async deprecation preamble into the eval context now, so generated code uses async APIs under `dynamic-page` from the first eval
- [x] `GET_SELECTION` + `turbofig_get_selection`: compact `{id,name,type,x,y,w,h}` shape
- [x] `SCREENSHOT` + `turbofig_screenshot`: `exportAsync` PNG, param `{scale, return:"file"|"inline"}`. File-mode writes the PNG to the bridge outbox dir; a subagent reads it.
- [x] Verify: via the file-bridge (dialog-free), create a frame in a live file, read selection, get a screenshot back. Repeat the same loop over curl to confirm both transports.
- [x] Error boundary: eval failures return a clean message to the caller (tool AND bridge), never crash the daemon

## Active priority: Orchestration-first speed slice (reprioritized 2026-08-05)

Goal: cut a big design job from about an hour to a few minutes with far fewer tokens, while keeping outputs great. This pulls the token/speed and orchestration levers forward from Phases 5-7, because they, not multi-file routing, address the speed and cost pain. Phase 4 is deferred until this slice lands.

**Execution rule (enforced):** the main session orchestrates, holds the shared contracts, verifies, and commits. Every code change is delegated to a subagent. Independent items run as parallel workers in one batch. The main context stays lean.

- [x] Minimal helper library: a compact `tf` craft namespace for the eval context (auto-layout frame, text with font-load, color, rect, `findOrCreate` for idempotency) with perf rules baked in (fonts via `Promise.all`, chunk, `commitUndo`). Pure logic unit-tested; figma glue smoke-tested. (from Phase 6)
- [x] Inject the helper namespace into every eval, beside the deprecation preamble, so generated code calls `tf.*`. (from Phase 6)
- [x] Compact helper API reference (one short doc) so the model learns the helpers cheaply. (from Phase 6)
- [x] Shaped returns + firewall defaults: ids-first, opt-in `fields`/`depth`; screenshots file-mode by default; images live and die in subagents. (from Phase 5)
- [x] Checkpoint + idempotency: persist the plan spec and per-section status to disk via the file-bridge; ops are idempotent by stable node ids (`findOrCreate`), so a re-run never duplicates and can resume. (from Phase 7)
- [x] `.claude/commands/design.md`: brief -> plan-first spec -> parallel firewalled builder subagents (each batches via `tf.*`) -> milestone screenshot per section in a subagent -> QA critic subagent (text critique) -> refine. Runs long jobs and resumes from the last completed section. (from Phase 7)
- [x] Verify live: hand it a brief; it builds a complex page in minutes, main-context tokens stay low, and a mid-run interruption resumes cleanly.

## Phase 4: True multi-file routing (deferred: see Active priority above)

Goal: N independent Claude sessions to N files at once. Stronger than console-mcp's broadcast.

- [x] Daemon session registry keyed by `mcp-session-id` and plugin connection `fileKey`. Replace the single `Mutex<Option<PluginConn>>` from Phase 2 with a multi-connection map.
- [x] Pair a session to a file (by `fileKey` or an explicit pick); route every call to the right plugin
- [x] File-bridge routing: a bridge job may carry a target `fileKey`. With none, default to the single connected plugin and error clearly when the target is ambiguous. The bridge has no `mcp-session-id`, so the job field is how it selects a file.
- [x] Handle multiple plugin connections (one per open file) simultaneously
- [x] Graceful handling when a target file closes mid-session (clear error, no cross-talk)
- [ ] Verify: two files + two Claude sessions operate concurrently, fully isolated

## Phase 5: Token & speed engine (build the levers, provisional baseline)

Goal: build the token and speed levers and take a PROVISIONAL measurement. The binding far-better gate is Phase 7, once helpers + orchestration + firewall exist.

**Reconciled 2026-08-05:** the Active-priority slice delivered the batching helper and the file-mode + ids-first + subagent-firewall defaults (via `tf` and `design.md`). The unchecked items below are the genuine remaining work: coded `fields`/`depth` read shaping, downscaled screenshots (needs the `image` crate), read-size caps, and the benchmark harness + baseline + provisional read (the measurement, skipped by choice).

- [ ] Shaped returns: ids-first, opt-in `fields`, `depth` limit, never a full-tree dump by default
- [ ] Screenshots default to downscaled + file-mode; inline high-res only on explicit request
- [ ] Enforce the context firewall: inline screenshots need an explicit opt-in and warn past a budget; large reads without `depth`/`fields` are capped or warned; file-mode plus subagent-read is the default path
- [x] Batching helper: many node ops in one `execute` round-trip
- [ ] Benchmark harness: measure tokens + wall-time for "design a webpage" and "20-slide deck"
- [ ] Run the harness against the current console-mcp stack and record baseline numbers
- [ ] Provisional read: record turbofig-so-far vs baseline; note the binding gate is Phase 7

## Phase 6: Core helper library

Goal: reliable, fast Figma craft as a compact JS namespace injected into the eval context.

**Reconciled 2026-08-05:** the Active-priority slice delivered the `tf` layout primitives, text and font handling, the async-only surface, the core perf rules (Promise.all fonts, chunk, commitUndo), and the compact API reference. The unchecked items below are still open: the extra perf helpers (`skipInvisibleInstanceChildren`, `findAllWithCriteria`), deck and slide scaffolds, component instantiation, variable read and write, export helpers, and the multi-slide-deck verify.

- [ ] Perf rules baked in: fonts via `Promise.all`, `skipInvisibleInstanceChildren`, `findAllWithCriteria`, `commitUndo`, chunk 50-100 nodes
- [x] Layout primitives: auto-layout builders (vertical/horizontal, sizing, spacing, padding)
- [ ] Deck/slide scaffolds; component instantiation; variable read/write; text + font handling; export
- [x] Async-only API surface (all `*Async` variants) for `documentAccess: dynamic-page`
- [x] Compact API reference doc (one file) so the model learns the helpers cheaply
- [ ] Verify: build a multi-slide deck in a few batched calls using only helpers

## Phase 7: Design-worker orchestration + the binding gate (the senior designer)

Goal: the agent workflow that turns a brief into real design, cheaply and durably. Then the binding ship gate.

**Reconciled 2026-08-05:** the Active-priority slice delivered `design.md`, the firewalled QA critic, the rubric, checkpoint, resume, context intake, and a live landing-page verify. The unchecked items below are still open: a moodboard-driven website verify, the measured low-token verify, and the binding far-better gate. All three need the Phase 5 benchmark harness first.

- [x] `.claude/commands/design.md`: plan-first spec, then parallel builder subagents, then a QA critic subagent, then refine
- [x] Context intake: accept a moodboard image, brand docs, and reference URLs as inputs
- [x] QA critic runs in a subagent (context firewall); returns a text critique, never raw images to the parent
- [x] Rubric-driven critique (hierarchy, spacing scale, contrast/AA, alignment, restraint)
- [x] Checkpoint long jobs: persist the plan spec + per-section progress to disk; operations are idempotent (keyed by stable node ids) so re-running never duplicates
- [x] Resume path: a crashed or interrupted job continues from the last completed section, with completed work intact in the Figma file, instead of restarting
- [ ] Verify: hand it a moodboard + brief; it produces a coherent website design
- [ ] Verify: main-context token use for the run stays low (measured against the Phase 5 harness)
- [ ] Binding gate: re-run the harness on the FULL pipeline (helpers + orchestration + firewall); confirm far better than console-mcp (~10x+ tokens; deck under ~8 min); record in DECISIONS.md. This is the ship or no-ship decision.

## Phase 8: Anti-slop baseline + trainability

Goal: good taste out of the box; trainable on top; redistributable without private context.

- [ ] Baseline taste pack (generic): spacing scale, type scale, grid, contrast/AA, hierarchy, restraint rules
- [ ] Anti-ai-slop guardrails: avoid the common tells (centered everything, generic gradients, emoji bullets, dead whitespace)
- [ ] Objective taste checks the critic scores against: grid adherence, type-scale conformance, contrast/AA pass, spacing rhythm; not eyeball alone
- [ ] Loadable brand/project packs (design tokens, components, rules) bound PER SESSION (keyed like the routing registry), so concurrent files can use different brands at once
- [ ] Clean separation: the public build ships only the generic baseline, never Luke's private packs
- [ ] Verify: switch between two brand packs cleanly; generic build carries no private data

## Phase 9: Slick Figma plugin UI

Goal: a clean, good-looking plugin panel. Clear connection state.

- [ ] Connection status (connected / reconnecting / offline) + active file + session
- [ ] Pairing view (which Claude session is bound to this file)
- [ ] Live activity log + a plugin-version / stale-warning indicator
- [ ] Visual polish: on-brand, tidy, no clutter
- [ ] Configurable daemon port: a port field in the plugin UI so the plugin connects to whatever `TURBOFIG_WS_PORT` the daemon uses (default 18847). Widen the manifest `allowedDomains` to cover localhost on the configured port, within Figma's static-whitelist constraint.

## Phase 10: Packaging & distribution (pure export, easy updates)

Goal: users install a compiled binary with no source and no compiler; updates are trivial. Ship macOS + npx first, then widen.

- [ ] npm publish: thin JS launcher `turbofig` + a macOS arm64 binary package as an `optionalDependency` (esbuild/Biome pattern; no postinstall network fetch, works on locked-down networks)
- [ ] `npx turbofig` starts the daemon on macOS arm64 with no Rust toolchain
- [ ] Widen the CI build matrix to macOS x64, Linux x64 (musl, rustls), Windows x64; publish their binary packages
- [ ] Secondary channels: `cargo install turbofig`, a Homebrew tap, raw GitHub Release binaries + `curl | sh` installer
- [ ] Auto-update nudge: daemon checks for a newer version on startup; plugin warns on version mismatch on connect
- [ ] Plugin delivery: `npx turbofig plugin` prints/opens the dev-install steps and the manifest path
- [ ] Play nice with Luke's setup: the file-bridge is the primary dialog-free path (native MCP is admin-blocked on his account, tested). Keep curl-on-18846 working as a fallback. Register a native `mcpServers` entry for users whose admin allowlists it.
- [ ] Migrate Luke's existing `figma-*` skills onto turbofig and confirm the daily workflow runs over the file-bridge
- [x] Productionize the file-bridge (pulled forward after Phase 3 for speed): client writes are race-safe by a file-stability check (a half-written file fails to parse and is retried, so no second sentinel file is needed) and the daemon now wakes on `notify` filesystem events with a 50 ms backstop poll for near-zero idle CPU. On macOS FSEvents delivery bounds latency at about 10-15 ms; on Linux inotify it is sub-millisecond. See `DECISIONS.md` #15 and `skills/file-bridge.md`.

## Phase 11: Presentation (README-first)

Goal: newcomers discover, understand, and install from the README alone.

- [ ] README: positioning vs figma-mcp / figma-console-mcp / Framelink (a clear "why this" table)
- [ ] Quickstart: install, import plugin, first design, in under 5 minutes
- [ ] State onboarding honestly: reconnect is zero-friction, but first install is a one-time manual dev-import of the plugin; do not oversell "zero ritual"
- [ ] A screencast GIF of a moodboard to website-design run
- [ ] Docs: brand-pack authoring, skill authoring, architecture overview
- [ ] Badges + crates.io/npm links + license

## Phase 12: Hardening & maintainability

Goal: robust, testable, cheap to maintain.

- [ ] Deprecation preamble upkeep: keep the sync-to-async "do not use" table current; a monthly changelog-watch note
- [ ] Error-feedback retry loop: a failed eval returns the error so the agent self-corrects
- [ ] Test suites: Rust (daemon routing/session) + plugin (dispatch/eval), co-located
- [ ] Benchmark regression guard in CI (token + speed budgets from the Phase 7 gate)
- [ ] Long-run resilience test: start a large job, kill the daemon mid-run, confirm `KeepAlive` restarts it and the job resumes with no duplication and no lost completed work
- [ ] `CONTRIBUTING.md` + a maintenance runbook

<!--
Add phases as the project grows. Each item:
  - is one commit when run via /phase
  - is independently verifiable
  - is in dependency order
Record the *why* behind big choices in DECISIONS.md, not here.
-->
