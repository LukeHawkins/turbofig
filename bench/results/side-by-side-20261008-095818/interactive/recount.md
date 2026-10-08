# Recount: tokens counted from the first test turn (2026-10-08)

This file corrects the token comparison in `results.md` and
`rerun-results.md`. Those files are left as they were recorded.

## Two problems in the original count

1. **Baseline.** `results.md` sets each session's baseline to the
   smallest per-turn input seen anywhere in the session. For
   figma-console-mcp, that minimum (42,309) came before the test, ahead
   of the connection back-and-forth. At the first test call its context
   was already 48,714. So about 6,400 tokens of pre-test connection chat
   were counted again on every test turn. For turbofig Run B, the
   minimum (38,966) is its first test call, so it carried no such extra.
   That extra made the original 2.3× ratio.
2. **Turns.** The original "turns" are transcript rows. One API call
   writes one row per content block (thinking, text, tool call), each
   with the same usage. Summing rows counts some calls 2 or 3 times.

## Recount

Each API call is counted once (deduplicated by message id). Baseline =
total input at each session's first test call. NET = sum of input over
the prompt's calls, minus baseline × calls.

| Prompt | Calls (tf / console) | turbofig NET | figma-console-mcp NET | Output (tf / console) |
|---|---|---|---|---|
| 1: red square | 4 / 4 | 8,675 | 4,786 | 1,712 / 1,810 |
| 2a: 40-slide setup | 2 / 2 | 9,183 | 6,790 | 1,428 / 1,185 |
| 2b: recolour | 2 / 2 | 12,975 | 10,746 | 1,247 / 1,155 |
| 3: hero section | 2 / 2 | 19,584 | 16,201 | 4,385 / 3,000 |
| **Total** | **10 / 10** | **50,417** | **38,523** | **8,772 / 7,150** |

Context at the first and last test call: turbofig 38,966 to 50,984,
figma-console-mcp 48,714 to 58,455.

The output gap is mostly extended thinking on the hero section (about
2,850 against 1,330 thinking tokens). The scripts were similar in size,
with no comments, helpers or retries in either.

## Estimate: all tools loaded up front

figma-console-mcp ran over curl here, so its 121 tool descriptions
(about 36,600 tokens, see `../../tool-tokens-20261007-221725.md`) were
never sent. As a normal MCP server in a client that loads every tool,
each call would also carry them. turbofig's 4 tools are about 680.

| Across the 4 jobs, 10 calls each | turbofig | figma-console-mcp |
|---|---|---|
| Measured (NET input) | 50,417 | 38,523 |
| Estimate: + tool list × 10 calls | ~57,200 | ~404,500 |

This is an estimate, not a measurement. Prompt caching bills repeated
tool lists at a fraction of the input price, so the cost gap is smaller
than the token gap. Claude Code switches to on-demand tool search when
tool descriptions pass 10% of the context, which also avoids most of
this load.

## What this means

- Measured per job, the two cost about the same. figma-console-mcp was
  slightly lower here.
- No job took a screenshot, so turbofig's screenshot downscaling was not
  measured.
- The approval counts (0 against 6) and the setup notes are unaffected.
