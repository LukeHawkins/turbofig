# How turbofig compares

Facts below are from each project's own docs, checked 2026-10-07. Sources
are listed at the end of each row group.

## turbofig

- 4 tools.
- No Figma personal access token, no sign-in, no Node.js.
- Needs Figma Desktop open with the turbofig plugin running in the file.
- The file bridge needs no MCP server at all: an agent drives it by
  reading and writing files.
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

## Figma's official MCP server

Sources: [Figma's MCP server docs](https://developers.figma.com/docs/figma-mcp-server/)
and [Figma's help center article](https://help.figma.com/hc/en-us/articles/32132100833559).

- **Remote server (Figma's recommended option):** hosted by Figma. Sign in
  with OAuth; no Figma Desktop needed. Available on all plans and seats.
  Can write to the canvas: create and modify frames, components,
  variables, and auto layout.
- **Desktop server:** needs the Figma desktop app and a Dev or Full seat
  on a paid plan.
- Tool count: not listed in either source, so not stated here.

## Table

| | turbofig | Figma's MCP server | figma-console-mcp |
|---|---|---|---|
| Edits your canvas | Yes | Yes (remote server) | Yes (NPX/Local and Cloud modes) |
| What you need | Figma Desktop + plugin running. No token, no sign-in, no Node | OAuth sign-in. Desktop server also needs Figma Desktop + a paid seat | Node.js (NPX/Local) or none (Cloud); Figma Desktop + Desktop Bridge; a Figma personal access token |
| Tools your agent loads | 4 | Not listed | 96-121, depending on mode |
| Works without adding an MCP server | Yes, with the file bridge | No | No |
| Screenshots kept small by default | Yes: saved to a file, downscaled to 1200 px | See their docs | See their docs |
| Comments, and file access without Figma open | No | Not covered in the sources above | Yes, through its REST features |

**When to use something else.** If you need comments, or file access
without Figma open, use a REST-based server such as figma-console-mcp or
Figma's own remote MCP server.
