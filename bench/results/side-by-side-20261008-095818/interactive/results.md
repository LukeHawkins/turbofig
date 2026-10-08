# Interactive side-by-side: turbofig vs figma-console-mcp (2026-10-08)

**This file covers Run A** (turbofig interactive, in `luke-os`) vs console.
A second turbofig run, **Run B** (neutral headless re-run, fairer host-repo
baseline), was added afterwards: full detail in `rerun-results.md`. The
combined table below both runs against console.

## Method

Two interactive Claude Code sessions, both model `claude-sonnet-5`, each received
the same 4 prompts in the same order (Prompt 1 red square, Prompt 2a 40-slide
setup, Prompt 2b recolour, Prompt 3 hero section). turbofig ran first for every
prompt.

- **turbofig session**: `05e71403-4819-4110-a8bc-47fc80a711ac`, cwd
  `~/Development/luke-os`, file-bridge transport (`~/.turbofig/inbox` /
  `outbox`), Figma file `bench-turbofig`.
- **console session**: `5c067b95-f804-40bd-8e52-aa5e394dae1b`, cwd
  `~/Development/flow-mcp`, curl to `http://127.0.0.1:3846/mcp`
  (figma-console-mcp via flow-mcp/supergateway), Figma file `bench-console`.

Per prompt, per session: wall seconds (user message to final "DONE" reply),
tool time (sum of tool_use-to-tool_result gaps), approvals (tool calls whose
recorded `permissionDecision.source` is `user_temporary`, i.e. the owner
clicked allow), approval wait, AI working time, turns, tool calls by type,
and tokens.

**No field in the log gives pure per-call execution time** (the only
`durationMs` field present is a `turn_duration` system event for the whole
assistant turn, not a single tool call). So approval wait for console is
**estimated**: tool time minus a pure-execution estimate taken from
turbofig's own tool time for the *same* prompt (turbofig never needs
approval, so its tool time is a clean execution-only figure for that
operation). Prompt 2b has no turbofig run to match against (see below), so
its estimate falls back to the median of turbofig's other three prompt tool
times. All approval-wait and AI-working-time numbers are marked
`estimated` in `results.json`.

**Fairness correction (NET tokens).** The turbofig session ran inside the
`luke-os` repo, which loads a much larger `CLAUDE.md` + memory than
`flow-mcp`'s, so turbofig's baseline (fixed) per-turn input cost is higher
before any Figma work happens. Reporting raw tokens alone penalises
turbofig for its host repo, not for the tool. For each session:

- **baseline** = the minimum per-turn total input (input + cache-creation +
  cache-read) seen anywhere in that session before the post-benchmark
  session-id check. turbofig baseline = **50,232** tokens/turn. console
  baseline = **42,309** tokens/turn.
- **NET** = raw total input for the prompt's turns, minus (baseline x turns
  in that prompt). This is the token cost attributable to the benchmark
  work itself, with each session's fixed per-turn overhead subtracted out.

NET is reported first in the table below; RAW follows for reference.

## Per-prompt results

| Prompt | Metric | turbofig | figma-console-mcp | ratio (console/turbofig) |
|---|---|---|---|---|
| 1: red square | NET input tokens | 10,538 | 52,749 | 5.01x |
| | RAW input tokens | 462,626 | 348,912 | 0.75x |
| | Wall (s) | 25.0 | 26.9 | 1.08x |
| | AI working (s, excl. approval wait) | 25.0 | 22.2 (est.) | 0.89x |
| | Approvals | 0 | 3 | - |
| | Turns | 9 | 7 | 0.78x |
| 2a: 40-slide setup (untimed) | NET input tokens | 15,375 | 39,200 | 2.55x |
| | RAW input tokens | 266,535 | 208,436 | 0.78x |
| | Wall (s) | 15.9 | 18.0 | 1.13x |
| | AI working (s) | 15.9 | 13.0 (est.) | 0.82x |
| | Approvals | 0 | 1 | - |
| | Turns | 5 | 4 | 0.80x |
| 2b: recolour (timed) | NET input tokens | n/a (not run) | 34,692 | n/a |
| | RAW input tokens | n/a | 161,619 | n/a |
| | Wall (s) | n/a | 11.7 | n/a |
| | AI working (s) | n/a | 10.6 (est., fallback method) | n/a |
| | Approvals | n/a | 1 | n/a |
| | Turns | n/a | 3 | n/a |
| 3: hero section | NET input tokens | 29,077 | 41,876 | 1.44x |
| | RAW input tokens | 280,237 | 168,803 | 0.60x |
| | Wall (s) | 29.5 | 33.3 | 1.13x |
| | AI working (s) | 29.5 | 27.9 (est.) | 0.95x |
| | Approvals | 0 | 1 | - |
| | Turns | 5 | 3 | 0.60x |

Model (all turns, both sessions): `claude-sonnet-5`.

**Prompt 2b gap.** The turbofig session never received Prompt 2b. Its
transcript goes straight from Prompt 2a's "DONE" to Prompt 3's user message;
no recolour job was ever written to `~/.turbofig/inbox`. Read-back confirms
all 40 "Speaker notes" text layers on turbofig's Bench 2 are still black
(`#000000`), the colour set by Prompt 2a. No turbofig figure exists for
Prompt 2b; only console's numbers are reported for that row.

**Reading the two token columns together.** turbofig's RAW input is higher
per prompt (bigger host-repo baseline), but once that fixed baseline is
subtracted, turbofig's NET (work-attributable) token cost is consistently
*lower* than console's, by 1.4x to 5x depending on the prompt. Console's
per-turn incremental cost is higher even after removing its own (smaller)
baseline, consistent with its curl responses carrying full JSON-RPC/SSE
envelopes plus verbose "housekeeping" warning payloads on every call.

**Reading AI working time.** Once estimated approval wait is removed,
console's working time is close to or faster than turbofig's wall time in
this single run (0.82x-0.95x, one point at 0.89x). Approval wait is the
main driver of console's slower *wall* time, not model or execution speed.
Treat this as a single-run signal, not a settled result (see Caveats).

## Combined: Run A vs Run B vs console

Run B (`1d2cf7b2-1aa9-4837-8045-5fe9ee10d372`) repeats the same 4 prompts
headless, in the neutral `turbofig-scratch` cwd, as one continuous
`claude -p` / `--resume` chain. It has a much lower baseline (38,966
tokens/turn vs Run A's 50,232), confirming the fairness concern: `luke-os`'s
CLAUDE.md/memory really was inflating Run A's fixed per-turn cost. Full
detail and verification: `rerun-results.md` / `rerun-results.json`.

Ratio column = console divided by Run B (console's AI working time has no
reconnect-dependent estimate issue here; Run B had 0 approvals throughout,
same as Run A, so its AI working time equals its wall time).

| Prompt | Metric | Run A (luke-os, interactive) | Run B (neutral, headless) | console (interactive) | ratio (console/B) |
|---|---|---|---|---|---|
| 1: red square | NET tokens | 10,538 | 13,711 | 52,749 | 3.85x |
| | Wall (s) | 25.0 | 22.3 | 26.9 | 1.21x |
| | Turns | 9 | 7 | 7 | 1.00x |
| | Approvals | 0 | 0 | 3 | - |
| 2a: setup (untimed) | NET tokens | 15,375 | 13,025 | 39,200 | 3.01x |
| | Wall (s) | 15.9 | 17.7 | 18.0 | 1.02x |
| | Turns | 5 | 3 | 4 | 1.33x |
| | Approvals | 0 | 0 | 1 | - |
| 2b: recolour (timed) | NET tokens | n/a (not run) | 18,804 | 34,692 | 1.85x |
| | Wall (s) | n/a | 15.3 | 11.7 | 0.76x |
| | Turns | n/a | 3 | 3 | 1.00x |
| | Approvals | n/a | 0 | 1 | - |
| 3: hero section | NET tokens | 29,077 | 27,150 | 41,876 | 1.54x |
| | Wall (s) | 29.5 | 39.5 | 33.3 | 0.84x |
| | Turns | 5 | 3 | 3 | 1.00x |
| | Approvals | 0 | 0 | 1 | - |

Reading this together: on NET tokens, Run B confirms Run A's finding --
turbofig's work-attributable token cost is consistently lower than
console's (1.5x to 3.9x), now from a session with a *smaller*, not larger,
fixed baseline than console's own. On wall time, the picture is mixed in
this single run: Run B is faster than console on Prompt 1 and 2a, but
slower on Prompt 2b and 3 (39.5s vs console's 33.3s on the hero section --
Run B had a long 23.8s time-to-first-token on that call, not reflected in
tool time). Both turbofig runs had zero approvals throughout; all of
console's approval waits are concentrated in its curl-based tool calls,
confirming that approval friction, not raw model or execution speed, is
console's main wall-time cost in this test.

Console's own pages could not be re-verified after the run: its Desktop
Bridge plugin disconnected sometime after the session ended, and (per
instructions) was not reconnected to force a check. Its Prompt-1/2b output
correctness rests on the transcript's own tool results (each call returned
`success:true`), not an independent read-back.

## Pass/fail (output verification)

Verification used `bench/side-by-side/drivers.sh` against the live Figma
files. Full detail: `interactive/verification.json`.

- **turbofig, bridge connected.** All three pages read back successfully.
  - **Bench 1: PASS, with a flag.** The "Square" rectangle is correct
    (200x200, `#FF0000`, x0 y0). But the file also holds a second, empty
    page also named "Bench 1" (id `2:2`), left over from a `script_error`
    retry and never cleaned up. The prompt said "do nothing else."
  - **Bench 2: PASS (structure only).** 40 frames, 1920x1080, 100 px grid
    gaps, each holding one "Speaker notes" text layer, Inter Regular 24,
    at x80/y900. Colours were never changed (see the Prompt 2b gap above),
    so this row scores the setup prompt only, not the recolour.
  - **Bench 3: PASS.** Hero frame 1440x800, fill `#0B1020`, vertical auto
    layout, padding 120 all sides, gap 32, centred both axes; title (Inter
    Bold 64, white) and subtitle (Inter Regular 24, `#A0A8C0`) text match;
    "Buttons" frame horizontal, gap 16, no fill; "Get started" (200x56,
    radius 12, fill `#6C5CE7`, white Inter Medium 18 label) and "Learn
    more" (200x56, radius 12, no fill, 1 px white inside stroke, white
    Inter Medium 18 label) both match.
- **console, bridge NOT connected.** `figma-console-mcp`'s Desktop Bridge
  plugin was disconnected at verification time (`Cannot connect to Figma
  Desktop`). Per instructions this was not reconnected. Bench 1/2/3 on
  `bench-console` were not read back; no screenshots were taken for this
  stack.

## Screenshots

- `interactive/screenshots/turbofig-runA-bench1.png`
- `interactive/screenshots/turbofig-runA-bench2.png`
- `interactive/screenshots/turbofig-runA-bench3.png`
- console: none (bridge disconnected, see above)

## Setup note

Per the owner's report: turbofig connected on the first try. flow-mcp
needed 3 to 4 attempts to connect initially, and its Desktop Bridge
connection dropped once before this test. This matches the console
session's own early transcript (07:53-07:56 UTC): repeated polling,
one reconnect, and a second session id before the plugin reported
connected, roughly 12 minutes before Prompt 1 was sent.

## Caveats

- Single run per stack, interactive (not the scripted/timed harness).
  Results are a signal, not a statistically solid comparison.
- turbofig always ran first for every prompt, so any shared state (Figma
  desktop app warm-up, OS/network caching) could favour it slightly.
- **Approval wait is estimated**, not measured directly: the log has no
  field separating a tool call's human-approval latency from its actual
  execution time. The estimate borrows turbofig's own (approval-free) tool
  time for the matched prompt as a stand-in for console's "pure execution"
  floor; Prompt 2b falls back to a median since turbofig has no matched run.
- turbofig's tool calls were pre-approved via a permission rule
  (`permissionDecision.source: "config"`/`"rule"`), not reviewed by the
  owner call-by-call; console's calls needed the owner's manual click
  (`source: "user_temporary"`) every time. This is a setup difference
  between the two sessions, not a proven protocol property.
- The colour list actually sent in both sessions' Prompt 2b text (a single
  hue-rotation ramp, `01 #D92626 ... 40 #D92641`) does not match
  `bench/side-by-side/colors-40.json` (a different 40-colour set).
  `colors-40.json` was not used by either live session; verification
  followed the colours actually in the prompt text.
- Console verification and screenshots were skipped because its Desktop
  Bridge plugin was disconnected at analysis time; this was not fixed by
  reconnecting it, per instructions.

## Evidence

- turbofig's scoped transcript slice (the 4 benchmark prompts, home path
  replaced with `~`) is **withheld**: it ran inside the `luke-os` repo,
  which loads a large personal `CLAUDE.md` and memory, so the raw log is
  kept at `bench/side-by-side/private-evidence/turbofig-interactive-20261008.jsonl`
  (gitignored, not committed). Only the aggregate numbers above are
  published.
- console's scoped transcript slice is published at
  `interactive/runs/console.jsonl`.
- Both scoped slices were scanned (case-insensitive) for a list of private terms before this decision: no
  matches in either slice. turbofig's is withheld anyway, by policy, given
  its host repo.
- Full verification read-back: `interactive/verification.json`.
- Full machine-readable results: `interactive/results.json`.
- Run B (neutral headless re-run): `interactive/rerun-results.md` /
  `interactive/rerun-results.json`. Its transcript is withheld too
  (`bench/side-by-side/private-evidence/turbofig-rerunB-20261008.jsonl`,
  gitignored) because this machine's global `~/.claude/CLAUDE.md` contains private context regardless of cwd; only its aggregate numbers are published.
