# Turbofig Benchmark

An exact reproduction guide for comparing turbofig against `figma-console-mcp`.
Every number this harness reports is either an exact wire byte count or a real
Claude Code token count. Nothing here is estimated or invented: see
`DECISIONS.md` #31 for why that line was redrawn.

## What this measures

Two separate, honestly-labelled layers:

- **Transport layer** (`bench/harness.ts`): exact request/response bytes and
  wall-clock time per job, for one target per run. Never a `chars / 4`
  estimate: every byte count is `Buffer.byteLength` of the real bytes sent or
  received.
- **Agent layer** (`bench/agent.ts`): the token cost of a real headless
  Claude Code session using a target's MCP tools, including the fixed
  per-session cost of loading that target's tool schemas (the transport layer
  cannot see this cost; the payload itself never carries it). Each session
  runs with `--tools ""` (no built-in tool at all, never `bypassPermissions`,
  which only skips the prompt and leaves Bash/Write loaded) plus
  `--allowedTools` naming only that target's own MCP tool, so the session can
  reach nothing but the tool under test. Report the published input-token
  figure as `totalInputTokens` (raw input + cache-creation + cache-read),
  never `inputTokens` alone: most of a tool schema's real cost sits in the
  cache fields, not the uncached remainder. The "create-frame" task verifies
  its claimed result through the target's own tool (and deletes it
  afterwards) before counting the run as a success, so a hallucinated "done"
  is never counted as one.

Only one Figma plugin runs per file at a time, so the harness benchmarks
exactly one target per invocation. `bench/compare.ts` merges the separate
reports back together.

## Targets

| Target | What it is | Default endpoint |
|---|---|---|
| `turbofig-mcp` | turbofig over MCP HTTP, tool `turbofig_execute` | `http://127.0.0.1:18846/mcp` |
| `turbofig-bridge` | turbofig over its file bridge (the primary agent path) | `~/.turbofig/inbox` -> `outbox` |
| `console-mcp` | `figma-console-mcp` over MCP HTTP, tool `figma_execute` | `http://127.0.0.1:3846/mcp` |

## Scenarios

| Scenario | Label | Targets | Description |
|---|---|---|---|
| `webpage` | transport + helpers | turbofig only | Marketing page via `tf.*` helpers: page setup, nav, hero, feature grid, footer. |
| `webpage-plain` | transport | all three | Same page, built with only the plain Figma Plugin API. |
| `deck20` | transport + helpers | turbofig only | 20-slide deck via `tf.*` helpers, in three batched execute calls. |
| `deck20-plain` | transport | all three | Same deck, built with only the plain Plugin API. |
| `read-selection` | transport | all three | Create and select one rectangle, then read the selection. |
| `read-screenshot` | transport | all three | Create and select one rectangle, then screenshot it. |

`read-screenshot` asks turbofig for an inline, undownscaled image
(`returnMode: "inline"`, `fullRes: true`), matching console-mcp's
`figma_take_screenshot`, which always fetches and returns the image inline
as base64. Without this, turbofig's default file mode returns a ~100-byte
path and console-mcp returns the whole image, which is not a like-for-like
comparison. `read-selection` asks console-mcp's `figma_get_selection` with
`verbose: true`, the closest match to turbofig's `fields: ["fills"]`
request; console-mcp's verbose mode also returns strokes, effects, and more
that the narrower turbofig request does not, so the two byte counts still
are not exactly equal (see "Known limits").

A **transport** scenario is a fair fight: the same plain-API code runs on
every target, so the comparison measures the transport, not turbofig's
helper library. A **transport + helpers** scenario shows what the helper
library adds on top, and only runs against turbofig (console-mcp has no
equivalent library, so running it there would measure the wrong thing).

Every scenario run creates its own fresh page (named `bench-<scenario>`)
and deletes it afterwards. It never renames, edits, or touches whatever
page or content the user already has open.

## Prerequisites

- Bun (`bun --version`). `bun install` once at the repo root.
- Figma Desktop, with **two** separate Figma files open for a same-session
  comparison (one plugin runs per file; see "Switching plugins" below). A
  blank file is fine; the harness creates and removes its own page.
- turbofig built: `cargo build --release` (or `cargo build` for a debug run).
- For the `console-mcp` target: a working `figma-console-mcp` install with a
  Figma personal access token, running behind `supergateway` so it serves
  streamable HTTP. `supergateway` fronts `figma-console-mcp` on port 3846 by
  default (configurable). Set this up in your own `figma-console-mcp`
  checkout; it lives outside this repo.
- For the agent layer (`bench/agent.ts`): the `claude` CLI on `PATH`. Every
  invocation spends real Claude Code usage — never loop it without intent.

## Starting each target

**turbofig-bridge and turbofig-mcp** (the daemon serves both over one process):

1. `cargo run --release` (or the installed `turbofig` binary) from the repo root.
2. In Figma Desktop, open the file you want to benchmark, then
   **Plugins -> Development -> turbofig** to connect its WebSocket.
3. The daemon now serves `turbofig-bridge` via `~/.turbofig/` and
   `turbofig-mcp` via `http://127.0.0.1:18846/mcp`, for whichever file has the
   plugin connected. Use `--file-key <key>` on the harness if more than one
   file has the plugin connected at once (copy the key from the plugin panel).

**console-mcp:**

1. In your `figma-console-mcp` setup: start the `supergateway` wrapper
   (needs `.env` with `FIGMA_ACCESS_TOKEN` set; see that setup's own
   instructions if `.env` does not exist yet).
2. In Figma Desktop, open the file you want to benchmark, then
   **Plugins -> Development -> Figma Console MCP Bridge**.
3. `console-mcp` now serves `http://127.0.0.1:3846/mcp` for that file.

### Switching plugins between targets

Only one development plugin can hold the WebSocket connection for a given
Figma file at a time. To compare turbofig against console-mcp:

- Use two separate files, one per plugin, running both targets at once, **or**
- Use one file and switch: close the running plugin's panel (or stop the
  daemon/gateway), start the other plugin, then run the harness again with
  the new `--target`.

Two files is faster for a side-by-side comparison session; one file is
closer to a real user's single-file workflow.

## Commands, in order

```sh
# Once per machine
bun install

# 1. turbofig over the file bridge (daemon + plugin running, see above)
bun bench/harness.ts --target turbofig-bridge --scenario all --runs 10 \
  --machine "MacBook Pro M3" --macos "15.1" --figma-version "<fill in>" \
  --daemon-version "<fill in>" --file "<fill in>" \
  --out out/turbofig-bridge.json

# 2. turbofig over MCP HTTP (same daemon, same or a second file)
bun bench/harness.ts --target turbofig-mcp --scenario all --runs 10 \
  --out out/turbofig-mcp.json

# 3. console-mcp over MCP HTTP (figma-console-mcp running, its plugin connected)
bun bench/harness.ts --target console-mcp --scenario all --runs 10 \
  --out out/console-mcp.json

# 4. Merge all three into one comparison table
bun bench/compare.ts \
  --report out/turbofig-bridge.json \
  --report out/turbofig-mcp.json \
  --report out/console-mcp.json \
  --out out/compare.json

# 5. Optional: the agent layer (spends real Claude Code usage)
bun bench/agent.ts --runs 5 --out out/agent-report.json
```

`--scenario all` skips any scenario the current `--target` does not support
(for example, `webpage` is skipped for `console-mcp`) rather than failing.
Run `--scenario <name>` for one scenario at a time instead.

### Dry run (no daemon, no Figma, no console-mcp)

```sh
bun bench/harness.ts --dry-run --scenario webpage-plain --runs 1
bun bench/agent.ts --dry-run --runs 1
```

`--dry-run` uses a stub transport that never contacts a daemon or Figma file.
It reports real byte counts for the canned job payloads, so it is useful for
sanity-checking a scenario's request size, but every timing field is
meaningless under the stub (`wallMs` is always 0). `bun bench/agent.ts
--dry-run` only prints the exact `claude` commands it would run; it never
spawns `claude`.

## Reading a report

Each `--out` file is a `BenchReport[]`, one element per scenario run:

- `valid` / `invalidReason`: `false` with a reason the moment any job in any
  iteration fails. An invalid run must never be read as a fast, cheap result.
- `failedIterations`: count of iterations where any job, setup, or teardown
  failed, out of `runs`. Always 0 on a valid run.
- `jobStats[i].coldMs`: the first iteration's wall time for job `i` (JIT,
  cache warm-up, cold connection). `null` when that iteration's job itself
  failed: a failed call's wall time is not a real timing.
- `jobStats[i].warmMedianMs` / `warmP95Ms`: timing across every iteration
  after the first that succeeded. A failed iteration's wall time is excluded
  from both, so it can never look like a fast, cheap run.
- `jobStats[i].failures`: count of iterations where job `i` itself failed.
- `totalRequestBytes` / `totalResponseBytes`: exact wire bytes across every
  job in the scenario (summed from `jobStats`, which take their byte counts
  from the first iteration that recorded that job; payload size does not
  vary run to run).
- `machine`: whatever you passed via `--machine`/`--macos`/`--figma-version`/
  `--daemon-version`/`--file`. Fill these in on every real run; a bare number
  with no machine context is not reproducible.

## Baseline comparison

```sh
bun bench/harness.ts --target turbofig-bridge --scenario webpage-plain --runs 1 \
  --baseline bench/baseline.json --max-ratio 1.2
```

`bench/baseline.json` ships as a **static payload-size snapshot** (see its
`note` field): a `--dry-run` record of `webpage-plain`'s request/response
bytes, with every timing field `null`. It is a size reference, not a timing
claim. `--max-ratio <n>` turns the comparison into an actual gate: the
process exits non-zero when the measured ratio of
`(requestBytes + responseBytes)` exceeds `n`, when the run itself is
invalid, or when no ratio could be computed at all (missing/malformed
baseline file, a scenario/target mismatch, or an invalid baseline total) —
`--max-ratio` never passes silently just because a comparison could not be
made. `--baseline` accepts either shape: a single `BenchReport` object (the
committed `bench/baseline.json` shape) or a `BenchReport[]` (whatever
`--out` writes, including a live multi-scenario report). When the file is an
array, the entry matching the current run's scenario and target is used.
Replace `bench/baseline.json` with a real `--out` report once a live
baseline exists, so the CI gate compares against a real number instead of a
static snapshot.

## Comparing across targets

`bench/compare.ts` groups the `--out` files from separate target runs by
scenario and prints one row per target: validity, total bytes, summed cold
time, summed warm-median time. An invalid run prints as `INVALID: <reason>`
and is excluded from the byte ratio, so a failed job can never leak a
flattering number into a published comparison.

## Known limits

- **Only one target benchmarked per invocation.** This is a hard Figma
  constraint (one plugin per file), not a shortcut; it is why `compare.ts`
  exists as a separate merge step.
- **console-mcp has no multi-file routing and no status-equivalent tool.**
  Scenarios that need either are scoped to turbofig only (see the Scenarios
  table). `--file-key` is a no-op for `console-mcp`.
- **MCP HTTP and the file bridge measure different kinds of round trip** (a
  network request with SSE parsing vs. a local filesystem write and watch).
  That difference is exactly what `turbofig-mcp` vs. `turbofig-bridge` is
  measuring; do not read it as a bug.
- **Real Figma variance.** Cold/warm/p95 numbers depend on the local
  machine, the Figma Desktop version, and current Figma app load. Always
  record `--machine`/`--macos`/`--figma-version`/`--daemon-version`/`--file`
  on a run meant for publication.
- **`bench/agent.ts` spends real Claude Code usage.** Never run it in a loop
  without deliberate intent; use `--dry-run` to check the commands first.
- **`read-selection` is not exactly equal across targets.** console-mcp's
  `verbose: true` returns more fields (strokes, effects, and more) than
  turbofig's narrower `fields: ["fills"]` request. The byte counts favour
  whichever side returns less, not necessarily the faster transport; read
  the per-job byte counts, not only the total, before publishing this one.
- **`--runs` must be a positive integer.** `--runs 0` or a non-numeric value
  is rejected at startup rather than silently producing an empty, trivially
  passing report.
- **CI gap.** `.github/workflows/*.yml` still runs
  `bun bench/harness.ts --dry-run --scenario webpage --baseline bench/baseline.json`
  (old scenario name, no `--max-ratio`, and `--dry-run` only ever checks
  payload size, never a live run). The scenario name needs to become
  `webpage-plain` (matching `bench/baseline.json`) and the step needs a
  `--max-ratio` to actually fail on a regression; `.github/` is outside this
  change's scope, so this is a flagged follow-up, not a silent gap.

## Running the tests

```sh
bun test bench/
```

## Manual run checklist (about 30 minutes, Figma Desktop open)

1. `bun install` at the repo root. (1 min)
2. Build the daemon: `cargo build --release`. (2-5 min, first build)
3. Open Figma Desktop with a blank or scratch file. Run
   **Plugins -> Development -> turbofig**. Start the daemon:
   `cargo run --release`. Confirm the plugin panel shows "Connected". (2 min)
4. Run the two turbofig targets:
   `bun bench/harness.ts --target turbofig-bridge --scenario all --runs 10 --out out/turbofig-bridge.json`
   then
   `bun bench/harness.ts --target turbofig-mcp --scenario all --runs 10 --out out/turbofig-mcp.json`.
   (8-10 min; `read-screenshot` and the two `-plain` builds are the slowest jobs)
5. Stop the turbofig plugin panel (or close that file). Start your
   `figma-console-mcp` setup's `supergateway` wrapper.
   Open the same or a second Figma file and run
   **Plugins -> Development -> Figma Console MCP Bridge**. (3 min)
6. Run the console-mcp target:
   `bun bench/harness.ts --target console-mcp --scenario all --runs 10 --out out/console-mcp.json`.
   (5-7 min; only the `-plain` and `read-*` scenarios apply)
7. Merge: `bun bench/compare.ts --report out/turbofig-bridge.json --report out/turbofig-mcp.json --report out/console-mcp.json --out out/compare.json`. (1 min)
8. Read `out/compare.json` (or the printed table) for the byte and timing
   comparison. Every number that goes into the README or a LinkedIn post
   must trace back to a file under `out/`, with the machine/Figma/daemon
   version fields filled in. (2 min)
9. Optional, separate budget: `bun bench/agent.ts --runs 5 --out out/agent-report.json`
   for the real token-cost comparison. This step spends Claude Code usage;
   decide deliberately whether to run it. (5-10 min)
