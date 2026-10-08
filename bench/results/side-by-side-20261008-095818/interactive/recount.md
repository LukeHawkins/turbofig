# Recount: tokens counted from the first test turn (2026-10-08)

This file corrects the token comparison in `results.md` and
`rerun-results.md`. Those files are left as they were recorded.

## The problem

`results.md` sets each session's baseline to the smallest per-turn input
seen anywhere in the session. For figma-console-mcp, that minimum
(42,309) came before the test, ahead of the connection back-and-forth.
At the first test turn its context was already 48,714. So about 6,400
tokens of pre-test connection chat were counted again on every test
turn. For turbofig Run B, the minimum (38,966) is its first test turn,
so it carried no such extra.

That extra, multiplied by turns, made the original 2.3× ratio. Prompt 1
had the most turns, so it took the biggest share.

## Recount

Baseline = total input at each session's first test turn. Same raw
totals and turn counts as `results.md` and `rerun-results.md`.

| Prompt | turbofig (Run B) NET | figma-console-mcp NET | Output tokens (turbofig / console) |
|---|---|---|---|
| 1: red square | 13,711 | 7,914 | 3,419 / 1,810 |
| 2a: 40-slide setup | 13,025 | 13,580 | 2,851 / 1,185 |
| 2b: recolour | 18,804 | 15,477 | 2,489 / 1,155 |
| 3: hero section | 27,150 | 22,661 | 8,765 / 3,000 |
| **Total** | **72,690** | **59,632** | **17,524 / 7,150** |

Context growth over the whole test: turbofig 38,966 to 50,984 (about
12.0K), figma-console-mcp 48,714 to 58,455 (about 9.7K).

Console output tokens are summed per API call (deduplicated by message
id) from `runs/console.jsonl`.

## What this means

- Per job, the two cost about the same. figma-console-mcp was about 1.2×
  lower here, mostly because turbofig's agent wrote more output.
- figma-console-mcp ran over curl, as in the tester's setup, so its 121
  tool definitions were never loaded. The test could not show the cost
  of a loaded tool list.
- No job took a screenshot, so turbofig's screenshot downscaling was not
  measured.
- The approval counts (0 against 6) and the setup notes are unaffected.

The README no longer states a per-job token ratio.
