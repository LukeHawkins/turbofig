# How turbofig compares

Facts below are from each project's own docs, checked 2026-10-07. Sources
are listed in the Sources section at the end.

## turbofig

- 4 tools.
- No paid Figma seat, no usage quota, no Figma personal access token, no
  sign-in, no Node.js.
- Needs Figma Desktop open with the turbofig plugin running in the file.
- The file bridge needs no MCP server at all: any process that can write a
  job file to the bridge folder can drive turbofig, not only a chat agent
  (see [skills/file-bridge.md](../skills/file-bridge.md)).
- The daemon keeps running between agent sessions.
- Several Figma files and several agent sessions can run at once.
- Screenshots are saved to a file by default. The agent gets a file path,
  not image data in its context, and the image is downscaled so its
  longest edge is at most 1200 px, unless the agent asks for full
  resolution (`daemon/src/mcp.rs`: return mode default `file`, `maxDim`
  default 1200, `fullRes` default false).
- No comments, no REST API access, and no reading a file without Figma
  open.

## figma-console-mcp (npm `figma-console-mcp`, v1.40.9, 2026-10-02)

- **NPX/Local mode:** 121 tools. Reads and writes. Needs Node.js, Figma
  Desktop, and its own Desktop Bridge plugin. Needs a Figma personal
  access token.
- **Cloud mode:** 96 tools. Writes to the canvas. No Node.js needed. Still
  needs Figma Desktop and the Desktop Bridge plugin, paired once. Needs a
  Figma personal access token.
- **Remote SSE mode:** a read-only subset of tools. Cannot create or
  modify designs.
- Supports several files at once: one connection per file, with
  cross-file execute since v1.39.0.
- `figma_execute` runs any Figma Plugin API code; `figma_execute_across_files`
  runs it across several open files at once.
- Also has REST-based features, including comments and reading a file
  without Figma open.
- No published usage quota in these sources.

## Figma for Agents (Figma's MCP server)

- **Needs a paid seat to edit.** "You need a Full seat to write to Figma
  files with agents. Dev seat holders can use the Figma MCP server for
  read-only workflows." (Write to canvas)
- **Read tools are rate limited; write tools are exempt** (Rate limits and
  access):
  - Starter plan: 20 tool calls a month.
  - View or Collab seat on Organization or Enterprise: 20 tool calls a
    month.
  - Full or Dev seat on Professional or Organization: 200 a day, 10 a
    minute.
  - Enterprise: 600 a day.
- **Remote server (Figma's recommended option):** hosted by Figma. Sign in
  with OAuth; no Figma Desktop needed. Can write to the canvas: create and
  modify frames, components, variables, and auto layout (on a Full seat).
- **Desktop server:** needs the Figma desktop app and a Dev or Full seat
  on a paid plan.
- **Write-to-canvas limits:** 20 KB output per call, no image or asset
  support, no custom fonts, and components must be published manually.
  Write tools are exempt from the rate limits above.
- **Pricing status, from Figma's own FAQ:** "This will eventually be a
  usage-based paid feature, but is currently available for free during the
  beta period."
- Tool count: 36, across 7 groups (design to code, code to design, image
  generation, design systems and Code Connect, generative plugins and
  shaders, account, Weave tools), per the published tool-and-prompts docs.
- Several files at once, and comments: not stated in these sources.

## Table

| | turbofig | figma-console-mcp (NPX) | figma-console-mcp (Cloud Mode) | Figma for Agents (Figma's MCP server) |
|---|---|---|---|---|
| Tool list size, as a share of AI memory (if loaded up front) | **0.3%** (about 680 tokens) | 18% (about 36,600 tokens) | 14.5% est. (96 tools, scaled from the measured NPX tokens) | 36 tools; size not published (a docs-based lower bound is 0.7%, see below) |
| Ready when your agent starts | **Under 0.1 s** (always running) | About 2 s (about 30 s on a first run) | Hosted, no local start | Hosted, no local start |
| Runs any Figma plugin script (advanced work) | **Yes** | Yes | Yes | Partly: no images or custom fonts, 20 KB output per call |
| Cost | **Free** | Free | Free (no Cloud-specific pricing or limits stated) | Editing needs a paid Full seat; write tools free in beta, later usage-based |
| Usage limits | **None** | None stated | None stated | Read tools: 20 calls a month (Starter) up to 600 a day (Enterprise) |
| Setup | Homebrew + one plugin import | Node.js, a Figma access token, the Desktop Bridge plugin | No Node.js; still needs Figma Desktop, the Desktop Bridge plugin paired once, and a Figma access token | Sign in with Figma |
| Several files at once | Yes | Yes | Yes | Not stated |
| Comments | No | Yes | Yes | Not stated |
| Works without Figma Desktop open | No | Partly | Partly | Yes |
| Works without adding an MCP server | Yes, file bridge | No | No | No |
| Screenshots kept small by default | Yes, file + 1200 px | Not stated in their docs | Not stated in their docs | Not stated in their docs |

Tool size and startup time are measured: see
[bench/results](../bench/results/session-cost-20261007-144824.md) for the
method and raw data.

Fewer tools matters because many agents load every tool's name,
description, and settings into context at the start of a session, before
any work starts: Anthropic measured about 72K tokens to load 50+ MCP tools
up front ([Anthropic, "Advanced tool
use"](https://www.anthropic.com/engineering/advanced-tool-use)). Some
newer clients defer tools behind a search step instead, which adds a
lookup on every use. Either way, more tools means more for the agent to
choose between. turbofig gives the agent 4 tools and lets one script do
the work.

turbofig has no quota and needs no paid seat. If you need comments,
figma-console-mcp's REST tools cover them. If you must work without Figma
Desktop open, Figma's remote server can.

Measured speed and token-per-edit numbers will be added after the
benchmark run.

## Sources

Dated 2026-10-07.

- **Tool tokens.** turbofig about 680 tokens, figma-console-mcp (NPX) about
  36,600 tokens, counted with `o200k_base` (`cl100k_base`: 668 and 35,848).
  That is 0.34% against 18.3% of a 200K-token context. Method and raw
  data: [bench/results/tool-tokens-20261007-221725.md](../bench/results/tool-tokens-20261007-221725.md).
- **figma-console-mcp Cloud Mode: live `tools/list` not measured, size
  estimated.** The Cloud Mode `/mcp` endpoint
  (`https://figma-console-mcp.southleft.com/mcp`) rejects a placeholder
  `figd_dummy` token with `401 invalid_token` before it will answer
  `tools/list`, so a live tool count, byte size, token count, and timing
  could not be measured without a real, valid Figma personal access
  token. Method and raw data:
  [bench/results/cloud-mode-20261007-203749.md](../bench/results/cloud-mode-20261007-203749.md).
  The README names which tool groups Cloud Mode excludes, but the tags do
  not reconcile to an exact 96-tool list, so the 14.5% figure above is a
  proportional estimate (96 of the 121 measured NPX tools), not a filtered
  tool-name list. Method and raw data:
  [bench/results/hosted-tool-size-20261007-210348.md](../bench/results/hosted-tool-size-20261007-210348.md).
- **Figma for Agents tool size: estimated from published docs.** Figma
  publishes a name, description, group and (for most tools) a parameters
  list for its 36 tools at
  [developers.figma.com/docs/figma-mcp-server/tools-and-prompts](https://developers.figma.com/docs/figma-mcp-server/tools-and-prompts/),
  but not a `tools/list` JSON-RPC response or full JSON Schema types, so
  the 0.7% figure above counts an approximate JSON built from that page,
  not a real server response. It is a lower bound: the docs give short
  descriptions and no full schemas, so the real definitions are larger
  (figma-console-mcp's real definitions average about 1,350 bytes per tool;
  this docs-based JSON averages about 170). Even this lower bound, 6.3 KB,
  is more than twice turbofig's 2.8 KB. Method and raw data:
  [bench/results/hosted-tool-size-20261007-210348.md](../bench/results/hosted-tool-size-20261007-210348.md).
- **Figma for Agents write-to-canvas limits:** 20 KB output per call, no
  image or asset support, no custom fonts, components must be published
  manually, write tools exempt from rate limits.
  [developers.figma.com/docs/figma-mcp-server/write-to-canvas](https://developers.figma.com/docs/figma-mcp-server/write-to-canvas)
- **Figma for Agents rate limits and access:**
  [developers.figma.com/docs/figma-mcp-server/rate-limits-access](https://developers.figma.com/docs/figma-mcp-server/rate-limits-access)
- **Figma's MCP server FAQ**, on pricing: "This will eventually be a
  usage-based paid feature, but is currently available for free during the
  beta period."
  [help.figma.com/hc/en-us/articles/39252411778583-Figma-MCP-server-FAQs](https://help.figma.com/hc/en-us/articles/39252411778583-Figma-MCP-server-FAQs)
- **Figma for Agents, general docs and seat requirement:**
  [developers.figma.com/docs/figma-mcp-server](https://developers.figma.com/docs/figma-mcp-server/)
  and [help.figma.com/hc/en-us/articles/32132100833559](https://help.figma.com/hc/en-us/articles/32132100833559).
- **Figma for Agents tool list**, source for the 36-tool count, groups,
  descriptions and parameters:
  [developers.figma.com/docs/figma-mcp-server/tools-and-prompts](https://developers.figma.com/docs/figma-mcp-server/tools-and-prompts/),
  cross-checked against
  [github.com/figma/mcp-server-guide](https://github.com/figma/mcp-server-guide).
- **Hosted-option tool size (both Cloud Mode and Figma for Agents)**,
  method and raw data:
  [bench/results/hosted-tool-size-20261007-210348.md](../bench/results/hosted-tool-size-20261007-210348.md).
- **figma-console-mcp README**, source for tool modes, tool counts,
  `figma_execute` ("Run any Figma Plugin API code"), and
  `figma_execute_across_files`:
  [github.com/southleft/figma-console-mcp](https://github.com/southleft/figma-console-mcp)
- **turbofig's own file-bridge protocol:** [skills/file-bridge.md](../skills/file-bridge.md).
- **turbofig's own screenshot defaults:** `daemon/src/mcp.rs`.
- **Anthropic, "Advanced tool use"** (72K-token figure for loading 50+ MCP
  tools): [anthropic.com/engineering/advanced-tool-use](https://www.anthropic.com/engineering/advanced-tool-use)
