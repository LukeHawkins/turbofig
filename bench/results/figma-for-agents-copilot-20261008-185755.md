# Figma for Agents: measured tool size and read timings

Date: 2026-10-08T18:43:04Z to 18:57:55Z

Server: Figma's remote MCP server (`https://mcp.figma.com/mcp`), server
version 1.0.0, protocol 2026-07-28. Client: GitHub Copilot CLI 1.0.93.
Account: a View seat on an Enterprise plan. Raw data:
[figma-for-agents-copilot-20261008-185755.json](figma-for-agents-copilot-20261008-185755.json).

## Why this run went through Copilot

A direct client (`@modelcontextprotocol/sdk` 1.32.1) could not connect.
Figma's dynamic client registration endpoint
(`POST https://api.figma.com/v1/oauth/mcp/register`) returned
`403 Forbidden`, so the script never reached `tools/list`. Every number
below comes through Copilot CLI, which is signed in to the server.

A View seat gets 20 rate-limited read calls a month and cannot write to
the canvas. The run used 10 of them, plus 1 call to `whoami`, which is
exempt. No rate-limit, quota or seat error came back.

## Tool list

| | Figma for Agents | turbofig |
|---|---|---|
| Tools | 41 | 4 |
| Bytes (`JSON.stringify(tools)`) | 85,025 | 2,824 |
| Tokens, `o200k_base` | 20,626 | 680 |
| Tokens, `cl100k_base` | 20,358 | 668 |
| Share of a 200K-token context | 10.31% | 0.34% |

turbofig figures: [tool-tokens-20261007-221725.md](tool-tokens-20261007-221725.md).

**Method.** The tool definitions were copied from what Copilot CLI showed
the model, then serialized with `JSON.stringify` and counted with
`js-tiktoken` 1.0.21, the same as the turbofig count. This is not the raw
`tools/list` response. Copilot prefixes each name with `Figma-` and folds
some JSON Schema keywords into the description text. The 41 names match
Copilot's own record of the tool list. Treat the size as accurate to a
few percent, not to the byte.

The earlier docs-based lower bound (36 tools, 6,262 bytes, 0.68%, from
[hosted-tool-size-20261007-210348.md](hosted-tool-size-20261007-210348.md))
is about 13 times smaller than this measured size. This file replaces it.

Write tools, from the description text (the schemas Copilot showed carry
no MCP read/write annotations):

- Canvas or file: `use_figma`, `generate_figma_design`, `upload_assets`,
  `generate_diagram`, `create_new_file`.
- Code Connect: `add_code_connect_map`, `send_code_connect_mappings`.
- Account library: `create_generative_plugin`, `update_generative_plugin`,
  `create_shader`, `update_shader`.

## Read timings (not like-for-like)

Node: an empty frame, 203×232, 0 children. 5 calls each (1 cold, 4 warm).

| Read | Tool | Cold | Warm median | Warm p95 |
|---|---|---|---|---|
| Selection | `get_metadata` | 1,429 ms | 832 ms | 1,023 ms |
| Screenshot | `get_screenshot` | 1,912 ms | 2,022 ms | 2,230 ms |

Do not compare these with turbofig's transport timings:

- They include Copilot CLI's own MCP client overhead.
- Wire bytes are unknown: Copilot does not log the raw JSON-RPC messages.
- `get_metadata` returns id, type, name, position and size only, no fills.
- `get_screenshot` ran in its default mode: it returns an image link, not
  inline base64, with a 1024 px default cap. The PNG was 744 bytes.

## Not measured

- Write timings and tokens per edit: a View seat cannot write.
- Agent session cost: the Claude Code CLI was not on the test device.
- The desktop server: it needs a Dev or Full seat.
