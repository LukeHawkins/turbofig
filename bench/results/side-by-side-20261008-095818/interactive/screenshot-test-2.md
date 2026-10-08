# Screenshot test 2 (2026-10-08, 12:43 to 12:45 UTC)

A stricter repeat of `screenshot-test.md`. Both chats got the prompt at
the same time (turbofig 12:43:26, figma-console-mcp 12:43:30):

> Screenshot test 2. Do not change the Figma file. Do not resize,
> compress, crop or convert any image.
>
> 1. Take a screenshot of the frame named "Hero" on the page "Bench 3".
>    Use your tool's default size and scale. Open the image exactly as
>    your tool returns it. If it comes back as base64 text, decode it to a
>    PNG file at full size, then open that file. Tell me in one sentence
>    what you see.
> 2. Do the same for the whole page "Bench 2": all 40 slides in one
>    image. Tell me in one sentence what you see.

Same sessions and counting method as `screenshot-test.md`. Image pixel
sizes are read from the image blocks that reached the model.

## Hero frame (like for like)

| | turbofig | figma-console-mcp |
|---|---|---|
| Capture | default: shrunk to 1200 px wide | default: scale 2 (2880 × 1600) |
| Image the model saw | 1200 × 667 | 2000 × 1111 (Claude Code's own limit) |
| Context added by the image | **+1,225** | **+3,264** |
| API calls for this step | 3 (switch page, screenshot, open) | 5 (curl, decode, 2 checks, open) |

turbofig's screenshot job passed no size option (`op`, `fileKey`,
`nodeId` only), so it used its default. Its image files were the same
size in bytes as in `screenshot-test.md`.

**Result: the hero screenshot cost 2.7× fewer tokens with turbofig,
about 2,000 fewer.** The image then stays in the conversation, so every
later call carries it again (at the cached rate).

## Bench 2, whole page (not like for like)

- figma-console-mcp captured the whole page: 1999 × 722, +2,096.
- turbofig selected all 40 frames and took a screenshot, but got a
  near-blank 1200 × 675 image of one slide: +1,300. **This is a turbofig
  bug:** a screenshot of a multi-frame selection does not capture all
  frames.

## Whole prompt

| | turbofig | figma-console-mcp |
|---|---|---|
| API calls | 9 | 10 |
| Context growth | 10,199 | 9,855 |
| Output tokens | 5,634 | 4,179 |
| Wall time | 88 s | 75 s, including approval waits |

The whole-prompt totals are close because turbofig's Bench 2 image was the
broken one-slide capture, and its first call wrote 2,422 output tokens
while it planned.

## What this means

- Per screenshot of a normal frame, turbofig's default saves about 2,000
  tokens (2.7×) against figma-console-mcp's default, when the image is
  opened at full size.
- In `screenshot-test.md`, the figma-console-mcp agent shrank its images
  to 800 px by its own choice. Agents do not do that reliably; this test
  shows the cost when it does not happen.
- One run. Treat the ratio as a guide.
