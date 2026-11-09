# turbofig offline A/B: baseline fbe7143 vs current main (2026-11-09)

This is an offline, component-level A/B. No live Figma session ran. See "What this does not measure".

Baseline: commit `fbe7143`. Current: commit `4c4c913` (the code is unchanged in `4889312`, which only edits docs).
Both builds ran on the same machine, with the same scripts, the same fake Figma plugin and the same inputs.

## Summary

| Metric | Baseline | Current | Change |
|---|---|---|---|
| MCP `tools/list`, bytes (every turn in MCP mode) | 2,824 | 2,386 | -15.5% |
| MCP `tools/list`, tokens (o200k_base) | 680 | 597 | -12.2% |
| File-bridge connect prompt, tokens (read once per session) | 115 | 504 | +338% (WORSE) |
| File-bridge connect prompt, bytes | 386 | 1,752 | +354% (WORSE) |
| `skills/file-bridge.md`, tokens (on demand) | 1,591 | 982 | -38.3% |
| `helpers/tf-api.md`, tokens (on demand) | 3,638 | 2,403 | -33.9% |
| `docs/agents.md`, tokens (on demand) | 1,183 | 593 | -49.9% |
| The 3 on-demand docs together, tokens | 6,412 | 3,978 | -38.0% |
| Documented file-bridge calls per job (file tools / shell recipe) | 2 / not documented | 2 / 1 | shell recipe: -50% vs 2 |
| webpage-plain scenario, response bytes (5 jobs) | 125 | 55 | -56.0% |
| deck20-plain scenario, response bytes (3 jobs) | 75 | 33 | -56.0% |
| Request bytes, all scenarios | same | same | 0% |
| Reply to `execute` that returns nothing, bytes | 25 | 11 | -56% |
| Reply to `execute` that returns a node, bytes | 23 (`{}`, no data) | 64 (id, name, type) | +41 bytes (WORSE in bytes, better in content) |
| Reply to a script error (`throw`), bytes | 69 | 99 | +30 bytes (WORSE in bytes, better in content) |
| Screenshot resize+encode, daemon image code, 3840x2160 to 1200 (median) | about 97 ms | about 71 ms | about -27% |
| Screenshot job through daemon + fake plugin, 3840x2160 node, default (median, mock) | 126.2 ms | 11.2 ms | see note |

Metrics that got worse: the connect prompt (once per session), the reply to `execute` that returns a node, and script-error replies. Reasons are in "Raw numbers".

## Method

### 1. MCP tools/list
- Same method as `tool-tokens-20261007-221725.md`: `node bench/mcp-stdio-probe.mjs <binary> mcp -- --dump-tools <file>` against a running daemon, then `js-tiktoken` 1.0.21, encoding `o200k_base`, over the dumped `JSON.stringify(tools)` text.
- Check: the baseline reproduces the published figure exactly (2,824 bytes, 680 tokens, 668 in cl100k_base).

### 2. File-bridge consumer docs
- Connect prompt: `prompts/agent-prompt.txt` of each tree, rendered like `fill_agent_prompt` in the daemon (1 connected file `F1`, default bridge dir `~/.turbofig`, MCP port 18846).
- Other docs: the file as it exists in each tree. Tokens use the same tokenizer as item 1.
- Script: `docs.ts` (scratchpad). Raw output: `docs.json`.

### 3. Transport scenarios
- `bun bench/harness.ts --target turbofig-bridge --scenario all --runs 10 --file-key F1 --bridge-dir <dir>`. The same harness file ran against both daemons (the harness is unchanged between the two commits).
- Each daemon ran as `TURBOFIG_BRIDGE_DIR=<dir> TURBOFIG_MCP_PORT=<p> TURBOFIG_WS_PORT=<p+1> turbofig serve` on spare ports (28940/28941 baseline, 28950/28951 current). Release builds.
- The fake plugin (`fakeplugin/fake.ts`) imports the real `createDispatcher` from `plugin/src/code.ts` of each tree. It runs the real execute wrapping, `tf` helpers, `safeResult`, `get_selection` and screenshot handlers against one shared in-memory `figma` mock. It connects with the legacy `?token=` handshake.
- Times here are mock overhead (about 63 ms per job, mostly the daemon inbox scan and the file round trip). They are identical in both builds. They are not Figma speed.

### 4. Typical reply sizes
- `sizes.ts` writes one job file per case into the bridge inbox and measures the exact bytes of the outbox file. Same jobs for both builds, same mock.

### 5. Screenshot pipeline
- Component: the `resize_png` function of each tree, copied verbatim into one scratch crate (`imgbench`) and built with `cargo build --release`. Input: one synthetic 3840x2160 PNG (2,588,397 bytes, gradient, blocks and sparse noise, fixed seed). Target: longest edge 1200. 15 timed runs per process, 1 warm-up, 4 separate process runs.
- End to end: job `screenshot` of a 3840x2160 mock node through daemon and fake plugin, 10 timed runs after 1 warm-up. Compared with `fullRes: true` (no daemon resize) on the same build as a control.

### 6. Round trips
- Counted by reading the documented procedure of each version. This is a documented-procedure count, not a measurement.

## Environment

- Machine: Apple M2, macOS 26.7.1. Rust 1.99.0, Bun 1.4.2.
- Tokenizer: `js-tiktoken` 1.0.21, `o200k_base` (and `cl100k_base` for item 1).
- Builds: `cargo build --release` for each tree (the baseline in a git worktree at `fbe7143`; the plugin `dist` was built first in each tree). Separate `CARGO_TARGET_DIR` per build.
- Scripts and raw data: `/private/tmp/claude-502/-Users-l-hawkins-Development-luke-os/eb187496-2583-4b89-a489-e8160dff049f/scratchpad/bench/` (`fakeplugin/fake.ts`, `docs.ts`, `sizes.ts`, `shot.ts`, `imgbench/`, `harness-*.json`, `sizes-*.json`, `tools-*.json`, `docs.json`). They are not part of the repo.

## Raw numbers

### MCP tools/list (4 tools in both builds)

| Build | Bytes | o200k_base | cl100k_base |
|---|---|---|---|
| baseline | 2,824 | 680 | 668 |
| current | 2,386 | 597 | 590 |

### Consumer docs

| File | Baseline bytes / tokens | Current bytes / tokens |
|---|---|---|
| Connect prompt (rendered) | 386 / 115 | 1,752 / 504 |
| `skills/file-bridge.md` | 6,252 / 1,591 | 3,602 / 982 |
| `helpers/tf-api.md` | 13,669 / 3,638 | 8,379 / 2,403 |
| `docs/agents.md` | 4,472 / 1,183 | 2,321 / 593 |

The connect prompt grew by 389 tokens. The reason: the current prompt carries the whole working procedure (job shape, helper list, one-call shell recipe, error handling, MCP fallback with auth header). The baseline prompt only pointed to the docs. An agent pays the 389 tokens once per session, and the on-demand docs it no longer needs to read are 2,434 tokens smaller in total. The net gain only exists if the agent reads the docs. An agent that never read them in the baseline has a net cost of +389 tokens.

### Transport scenarios (harness, 10 runs each, bridge target)

Bytes are request / response per job. The first job of each scenario has the same code in both builds.

| Scenario | Jobs | Request bytes (both builds) | Response bytes baseline | Response bytes current |
|---|---|---|---|---|
| webpage-plain | 5 | 8,278 (310, 1734, 2694, 2410, 1130) | 125 (25 per job) | 55 (11 per job) |
| deck20-plain | 3 | 8,638 (2475, 1792, 4371) | 75 | 33 |
| read-selection | 2 | 346 | 194 | 180 |
| read-screenshot (inline, fullRes) | 2 | 357 | 3,745 | 3,731 |

Mock time per job: warm median 62 to 65 ms and p95 63 to 65 ms in both builds. No difference is visible. Treat it as mock overhead.

Scenarios `webpage` and `deck20` (the `tf.*` helper variants) were invalid in both builds, and are not reported. Cause: the job code in `bench/scenarios.ts` has a `\n` inside a JS template literal, which becomes a real newline inside a string literal and gives `Unexpected EOF`. This is a defect in the benchmark file, not in either build. It is not fixed here because no tracked file may change. Fixed after this run (2026-11-09): the 3 newline escapes now use `\\n`, so both helper scenarios parse; re-measure them in the next run.

### Typical replies (bytes of the outbox file; request bytes are equal in both builds)

| Reply | Baseline | Current | Content |
|---|---|---|---|
| execute returns a node | 23 | 64 | baseline `{"ok":true,"result":{}}` (no information); current `{"id","name","type"}` |
| execute returns nothing | 25 | 11 | baseline `"result":null` is dropped in current |
| execute returns a string id | 27 | 27 | same |
| status, 1 file | 253 | 138 | current drops `inFlight` and similar fields |
| get_selection, 5 nodes, default fields | 385 | 385 | same |
| get_selection, 5 nodes, `fields:["fills"]` | 694 | 678 | |
| screenshot, file mode (1600x900 node) | 205 | 193 | path length differs; both return a 1200x675 file |
| script error, `throw new Error("boom")` | 69 | 99 | current adds the user source line; baseline reports line 3 for a one-line script (wrong) and current reports line 1 |
| script error, undefined variable | 92 | 118 | same pattern |
| script error, syntax error | 87 | 163 | baseline reports line 100 (wrong); current adds a hint and no wrong line |

Per-reply sizes depend on the example job. The path length in the screenshot reply depends on the bridge dir used in this run.

### Screenshot pipeline

Component, `resize_png` only (median of 15 runs per process):

| Process run | Baseline median / p95 ms | Current median / p95 ms |
|---|---|---|
| 1 | 86.0 / 88.4 | 63.0 / 63.6 |
| 2 | 100.7 / 102.7 | 70.2 / 74.1 |
| 3 | 96.6 / 101.8 | 71.6 / 76.3 |
| 4 | 97.0 / 100.7 | 75.0 / 76.6 |

- Output: baseline 1,637,129 bytes, current 1,509,866 bytes (-7.8%), both 1200x675. Baseline code: Lanczos3 filter and default PNG compression. Current code: CatmullRom filter and fast PNG compression.
- Run-to-run drift of the machine is about 15%, so read the result as "about 25 to 30% faster", not as an exact figure. The ordering held in all 4 runs.
- The input image is noisy, so the compressed output is large. A screenshot of plain UI would give smaller files and different times.

End to end, daemon + fake plugin, 3840x2160 node, mock times (median / p95 of 10):

| Job | Baseline | Current |
|---|---|---|
| `screenshot` default (reply says 1200x675) | 126.2 / 130.4 ms | 11.2 / 12.4 ms |
| `screenshot` with `fullRes: true` (reply says 3840x2160), control | 22.7 / 25.8 ms | 23.9 / 26.1 ms |

The control is equal in both builds. The default job is far faster in the current build because the current plugin code exports the node at the reduced scale. The mock then returns a small PNG, so the daemon has nothing to resize. The baseline plugin exports the full size and the daemon decodes and resizes it. The mock export cost does not model the real Figma export cost, so use the end-to-end figure only as evidence that the large PNG no longer crosses the socket and no longer needs a resize. Do not quote 126 ms vs 11 ms as a Figma speed-up.

### Round trips per job (documented-procedure count)

| Version | Procedure in the docs | Tool calls per job |
|---|---|---|
| baseline (`skills/file-bridge.md` + prompt) | Write the job file, then Read the result file. "If it is not present yet, wait a moment and read again." | 2, plus 1 for each repeated read |
| current (prompt) | Shell recipe: write, wait loop and `cat` in one call (waits 35 s at most) | 1 |
| current, file tools only | Write, then Read; if missing, read one more time | 2, plus at most 1 |

The baseline docs also say to run the `status` op first to learn the `fileKey` when 0 or many files are open. The current docs say `fileKey` is optional when one file is open. Example: a 5-job page costs 10 calls in the baseline procedure and 5 calls in the current shell recipe. Measured agent behaviour may differ.

## What this does not measure

- Live Figma speed. All times here come from an in-memory mock plus the daemon. The real Figma export, layout and font loading costs are absent.
- End-to-end session tokens. The model's own output, its reading of docs, retries and the real count of tool calls need a live run: `bun bench/agent.ts` (spends real Claude usage) or the side-by-side session in `bench/side-by-side/`.
- Real Claude token counts. The token figures use the `o200k_base` tokenizer as a proxy, like the published `tool-tokens` result. They are not Claude tokenizer counts.
- The `tf.*` helper scenarios (`webpage`, `deck20`). They did not run, see above.
- Screenshot quality or the real image content. The mock returns synthetic PNGs.
- Auth and handshake cost. The fake plugin used the legacy `?token=` handshake on both builds, so the newer challenge/proof handshake of the current build is not exercised.
