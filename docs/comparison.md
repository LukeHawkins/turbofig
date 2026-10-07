# How turbofig compares

Facts below are from each project's own docs, checked 2026-10-07. Sources
are listed at the end of each section and inline for the Figma facts.

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

## Figma's official MCP server

Sources: [Write to canvas](https://developers.figma.com/docs/figma-mcp-server/write-to-canvas/)
and [Rate limits and access](https://developers.figma.com/docs/figma-mcp-server/rate-limits-access/),
and the [MCP server docs](https://developers.figma.com/docs/figma-mcp-server/)
and [Figma's help center article](https://help.figma.com/hc/en-us/articles/32132100833559).

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
- Tool count: not stated in these sources, so not given here.
- Several files at once, and comments: not stated in these sources.

## figma-console-mcp (npm `figma-console-mcp`, v1.40.9, 2026-10-02)

Source: [its README](https://github.com/southleft/figma-console-mcp).

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
- Also has REST-based features, including comments and reading a file
  without Figma open.
- No published usage quota in these sources.

## Table

| | turbofig | Figma's MCP server | figma-console-mcp |
|---|---|---|---|
| Needs a paid Figma seat to edit | No | Yes, a Full seat (Dev seats are read-only) | No |
| Usage limits | None | Read tools: 20 calls/month (Starter, View/Collab), 200/day (Professional, Organization), 600/day (Enterprise) | See their docs |
| What you need | Figma Desktop + plugin. No token, no sign-in, no Node | OAuth sign-in | Figma Desktop + Desktop Bridge, a Figma personal access token, Node.js (not needed in Cloud Mode) |
| Tools your agent loads | 4 | Not stated in their docs | 96 to 121, by mode |
| Tool definitions your agent loads every session | 4 tools, 2.8 KB | Not published | 121 tools, 163 KB (NPX mode; 96 tools in Cloud Mode) |
| Ready to use after the agent starts | About 66 ms (already running in the background) | Hosted remote server, not measured | About 1.9 s: npx starts a fresh process each session (29 s on a first run) |
| Several files at once | Yes | Not stated in their docs | Yes |
| Works without adding an MCP server | Yes, file bridge | No | No |
| Screenshots kept small by default | Yes, file + 1200 px | Not stated in their docs | Not stated in their docs |
| Comments, or work without Figma open | No | Without Figma open: yes (remote server) | Comments: yes (REST tools) |

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
