# Screenshot test (2026-10-08, 12:30 to 12:35 UTC)

Same prompt in both open test chats (claude-sonnet-5, Claude Code):

> Screenshot test. Take a screenshot of the page "Bench 3" and tell me in
> one sentence what you see. Then take a screenshot of the page "Bench 2"
> and tell me in one sentence what you see. Use your normal screenshot
> method with its default settings. Do not change any file.

Sessions: turbofig `127911f3-f113-4c13-aa89-c26ec64baaa4` (log kept
private: it contains unrelated personal context), figma-console-mcp
`5c067b95-f804-40bd-8e52-aa5e394dae1b` (`runs/` has the earlier part).
Each API call is counted once (deduplicated by message id). "Context" =
total input on a call (input + cache creation + cache read).

## Image cost: context added by each screenshot

Measured as the jump in context from the call that opened the image to
the next call.

| Screenshot | turbofig | figma-console-mcp |
|---|---|---|
| Bench 3 (hero frame) | +1,173 (PNG, 1200 px cap, 89 KB) | +643 (PNG downscaled by the AI to 800 px, 46 KB) |
| Bench 2 | +1,216 (one slide, 21 KB) | +498 (whole page of 40 frames, 800 px, 12 KB) |

Bench 2 is not like for like: turbofig captured the selected slide,
figma-console-mcp captured the whole page.

## How each tool delivered the image

- **turbofig:** the screenshot job wrote a PNG to `~/.turbofig/outbox`
  (shrunk to at most 1200 px). The job result was about 100 characters.
  The AI opened the file with Read.
- **figma-console-mcp (over curl):** the screenshot (scale 2, 66,258-byte
  PNG) came back inside the curl output: 86.6 KB of JSON with a text
  block and an image block. Claude Code saved it to a file and showed a
  2 KB preview of base64. That preview added **+2,179 tokens** of context.
  The AI then extracted the image from the saved file with Python,
  downscaled it to 800 px with `sips`, and opened it. For Bench 2 it saved
  the curl output straight to a file, so no preview was added.
  Nothing in the flow-mcp repo asks for the 800 px downscale. The AI
  chose it.

## Whole prompt

| | turbofig | figma-console-mcp |
|---|---|---|
| API calls | 15 (11 after it reached the right file) | 10 |
| Context growth | 11,240 (8,555 after the right file) | 6,914 |
| Output tokens | 6,046 | 3,356 |
| Wall time | 68 s after the right file | 73 s, including approval waits |

turbofig's run had problems that cost it calls:

1. At 12:31:04 it was connected to a different file (key starting
   `sOqY`, only "Page 1"). It asked the tester which file to use.
2. The tester connected it to the bench file in 2 follow-up messages
   (12:32:08 and 12:32:31). The agent first treated the redirect as a
   possible prompt injection.
3. On Bench 3 it switched page twice (the first switch failed), took a
   screenshot that returned no image file because nothing was selected,
   selected the page's frames, and took the screenshot again.

## Result

- In this test, screenshots cost about the same with both tools, at a few
  hundred to about 1,200 tokens each. figma-console-mcp's images were
  smaller because its AI downscaled them to 800 px by itself.
- turbofig avoided the base64 preview (+2,179 tokens) that the curl route
  added on the first figma-console-mcp screenshot.
- This test does not support a claim that turbofig saves many tokens on
  screenshots.
