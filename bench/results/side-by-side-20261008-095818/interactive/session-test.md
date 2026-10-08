# Session test: a 4-step landing page with screenshots (2026-10-08)

A realistic design loop: build, screenshot, check, repeat. Both chats got
the prompt at the same time (turbofig 13:01:01, figma-console-mcp
13:01:03 UTC), on claude-sonnet-5 in Claude Code:

> Session test. On a new page named "Bench 4", build a landing page in a
> frame named "Landing", 1440 wide. Build it in 4 steps, one section per
> step: a nav bar, a hero, a row of 3 feature cards, and a footer. After
> each step, take a screenshot of the "Landing" frame at your tool's
> default settings and check it before you start the next step. Do not
> resize, compress, crop or convert any image. If a screenshot comes back
> as base64 text, decode it to a PNG file at full size, then open that
> file. At the end, tell me in one sentence what you built.

Same sessions and counting method as `screenshot-test-2.md`: each API
call counted once, from the first call after the prompt.

## Clean comparison (used in the README)

One rule for both tools: leave out every stretch where the agent was
recovering from a tool failure, and keep normal design iteration (fixing
the nav width, fixing card text wrapping). Context added during a left-out
stretch is also subtracted from every later call.

Left out:

- **turbofig:** 13:01:24 to 13:03:39, the step-1 job that never returned
  and the polling around it (13 calls, 135 s).
- **figma-console-mcp:** 13:01:36 to 13:02:57 (failed screenshot decode on
  step 1 and its recovery); 13:03:31 to 13:03:58 and 13:05:01 to 13:05:49
  (stale screenshots, byte for byte the same image as the step before, and
  the cache-busting retries); 13:09:13 to 13:09:54 (a step-4 retry with the
  same cache-busting pattern; doubtful, left out in figma-console-mcp's
  favour). 17 calls in total.

| | turbofig | figma-console-mcp | Ratio |
|---|---|---|---|
| Tokens processed for the work (NET input) | **133,538** | 374,752 | **2.8× fewer** |
| Context growth | 15,540 | 35,312 | 2.3× fewer |
| Output tokens | 11,572 | 18,089 | 1.6× fewer |
| Billed cost, in input-token equivalents | **227,994** | 349,822 | **35% cheaper** |
| AI calls | 17 | 24 | |
| Screenshots | 5 | 6 | |
| Wall time | **171 s (2.9 min)** | 355 s (5.9 min), incl. approval waits | 2.1× faster |
| AI time | **~134 s** | ~199 s | 1.5× less |

## Full session, nothing left out

**Every number in this section includes both tools' problems** (listed
at the end).

| | turbofig | figma-console-mcp | Ratio |
|---|---|---|---|
| Tokens processed for the work (NET input) | **382,619** | 879,370 | **2.3× fewer** |
| Context growth over the session | 25,468 | 51,371 | 2.0× fewer |
| Output tokens | 18,789 | 25,273 | 1.3× fewer |
| Billed cost, in input-token equivalents | **406,325** | 591,874 | **31% cheaper** |
| AI calls | 30 | 41 | |
| Screenshots taken | 5 | 9 | |
| Wall time, first call to last | **306 s (5.1 min)** | 552 s (9.2 min), incl. approval waits | 1.8× faster |
| AI time (model working, excludes tool and approval time) | **~217 s** | ~321 s | 1.5× less |

NET input = sum of input over all calls, minus the first call's input ×
calls. Billed cost uses the standard ratios: cache write 1.25×, cache
read 0.1×, output 5× the base input price. It includes the fixed context
each session re-reads on every call (81.5K turbofig, 77.6K console), so
the cost gap is smaller than the token gap.

## Screenshots

Context added by each image (the jump to the next call):

| Frame state | turbofig | figma-console-mcp |
|---|---|---|
| Nav only | 1200 × 56: +397, +227 | 1998 × 111: +508, +758 |
| Nav + hero | 1200 × 355: +700 | 1999 × 708: +2,093, +2,210 |
| Nav + hero + cards | 1200 × 597: +1,087 | 1440 × 872: +1,840; 2000 × 1211: +3,391; 2000 × 1236: +3,463, +3,462 |
| Finished page | 1200 × 650: **+1,173** | 2000 × 1347: **+3,794** |

The finished page cost **3.2× fewer tokens** per screenshot with
turbofig. figma-console-mcp's images arrived as base64 inside the curl
output, so each one also needed a decode step.

## Why figma-console-mcp used more

1. **Bigger screenshots.** About 3× the tokens each, and each stays in
   the conversation, so every later call re-reads it.
2. **More screenshots and calls.** 9 screenshots and 41 calls, against 5
   and 30. It re-checked step 3 three times and step 4 twice.
3. **Extra steps per screenshot.** Save the curl output, decode the
   base64, then open the PNG.

## Problems in each run (included in the numbers above)

- **turbofig:** the first step-1 job (13:01:34) returned no result. The
  agent polled, checked `turbofig status` and the daemon log, resubmitted
  it, and inspected the page. At 13:03:49 a second version of the script
  returned. About 2 min 25 s and 13 calls. The cause was not
  investigated: a job that never returns should fail fast with an error.
  After the nav step it took 1 extra screenshot to fix the nav width.
- **figma-console-mcp:** the step-1 call returned an error (13:01:49).
  The agent read the raw output, retried, searched for a screenshot tool
  and read the flow-mcp tool docs. It opened a stale image once, and
  re-took screenshots on steps 3 and 4.

The clean comparison at the top leaves these out on both sides.

## Limits

One run. Both sessions carried earlier test history in their context,
which the NET count subtracts. figma-console-mcp ran over curl, so its
tool list was not loaded. Approval prompts were not counted for this
test.
