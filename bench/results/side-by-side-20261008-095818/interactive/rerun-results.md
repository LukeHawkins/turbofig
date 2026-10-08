# turbofig run B: neutral headless re-run (2026-10-08)

## Why this run exists

Run A (the original interactive turbofig session) ran inside `luke-os`,
whose `CLAUDE.md` + memory are much bigger than the console session's host
repo (`flow-mcp`). Run B repeats the same 4 prompts, headless, in a neutral
scratch folder whose only project context is turbofig's own file-bridge
agent prompt, for a fairer comparison against the console session's
interactive numbers.

## Method

- cwd: `~/Development/turbofig/bench/side-by-side/turbofig-scratch`
  (its `CLAUDE.md` is turbofig's own file-bridge connect prompt, already
  scoped to Figma file `bench-turbofig`).
- One continuous session, 4 calls:
  `claude -p --model claude-sonnet-5 --output-format stream-json --verbose
  --allowedTools "Write Read Bash(cat:*) Bash(ls:*) Bash(sleep:*)"` for
  Prompt 1, then `--resume <session id>` (same flags) for Prompt 2a, 2b,
  3 in order. `--dangerously-skip-permissions` was never used.
- Session id: `1d2cf7b2-1aa9-4837-8045-5fe9ee10d372`. Model on every turn:
  `claude-sonnet-5`.
- Prompt texts: taken verbatim from the console transcript (the exact text
  the owner pasted for Prompts 1, 2a, 2b, 3), not retyped.
- Before this run, the existing "Bench 1/2/3" pages (and the stray empty
  duplicate from Run A) were renamed to "Run A - Bench 1/2/3" via the file
  bridge (untimed, no deletion), to free up the plain names for Run B.

**On tool access.** No tool call in this run shows a `permissionDecision`
at all (not even an auto-approved "config/rule" entry, let alone a manual
one) -- every Bash call simply ran. This machine's existing permission
configuration evidently already allows Bash broadly, so the
`--allowedTools` restriction passed on the CLI did not narrow anything
below that baseline. This matches Run A's posture (zero approvals, zero
approval wait) and is unlike the console session (which needed the owner's
manual click on several calls). Flagged here for transparency: the
`--allowedTools` flag was not the thing keeping this run approval-free;
the machine's standing permission settings were.

## Per-prompt results

| Prompt | Wall (s) | Turns | Tool calls | RAW input | NET input | Output tokens |
|---|---|---|---|---|---|---|
| 1: red square | 22.3 | 7 | Bash x3 | 286,473 | 13,711 | 3,419 |
| 2a: 40-slide setup (untimed) | 17.7 | 3 | Bash x1 | 129,923 | 13,025 | 2,851 |
| 2b: recolour (timed) | 15.3 | 3 | Bash x1 | 135,702 | 18,804 | 2,489 |
| 3: hero section | 39.5 | 3 | Bash x1 | 144,048 | 27,150 | 8,765 |

Approvals: **0** on every prompt (so AI working time = wall time; no
approval wait to subtract).

Baseline (minimum per-turn raw input anywhere in this session) =
**38,966** tokens/turn. NET = raw total input for the prompt's turns minus
(baseline x turns), same method as the main comparison.

By Prompt 1, the model had already learned the pattern: one Bash call that
writes the job file and polls the outbox in the same command, instead of
the separate Write-then-poll sequence seen in Run A. This is why turns and
tool-call counts drop from Prompt 2a onward.

## Verification (all three pages, against the prompt text's own spec)

Full detail: `interactive/rerun-results.json`.

- **Bench 1 (id `2:95`): PASS.** "Square" rectangle 200x200, fill
  `#FF0000`, x0/y0. Single clean page -- no stray duplicate this time.
- **Bench 2 (id `3:97`): PASS, including the recolour.** 40 frames,
  1920x1080, 100 px grid gaps, one "Speaker notes" text layer per frame
  (Inter Regular 24). All 40 fills match the colour list in Prompt 2b's own
  text exactly (`01 #D92626 ... 40 #D92641`), confirmed slide-by-slide, not
  against `bench/side-by-side/colors-40.json` (which neither live session
  used). Unlike Run A, Prompt 2b was sent and completed here.
- **Bench 3 (id `3:178`): PASS.** Hero frame 1440x800, fill `#0B1020`,
  vertical auto layout, padding 120 on all sides, gap 32, centred both
  axes. Title (Inter Bold 64, `#FFFFFF`) and subtitle (Inter Regular 24,
  `#A0A8C0`) match. "Buttons" frame horizontal, gap 16, no fill. "Get
  started" (200x56, radius 12, fill `#6C5CE7`, white Inter Medium 18
  label) and "Learn more" (200x56, radius 12, no fill, 1 px white inside
  stroke, white Inter Medium 18 label) both match exactly.

## Screenshots

- `interactive/screenshots/turbofig-runB-bench1.png`
- `interactive/screenshots/turbofig-runB-bench2.png`
- `interactive/screenshots/turbofig-runB-bench3.png`

## Evidence

The full stream-json / project-log transcript for this run is **withheld**:
`bench/side-by-side/private-evidence/turbofig-rerunB-20261008.jsonl`
(gitignored, not committed). Even in this neutral scratch cwd, Claude Code
still loads the user's global `~/.claude/CLAUDE.md`, which contains
private context, so the privacy scan matched a list of private terms.
Only the aggregate numbers above and in `rerun-results.json` are published.

See `interactive/results.md` for the combined table against Run A and the
console session.
