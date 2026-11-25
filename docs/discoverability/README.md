# turbofig discoverability artifacts

`mcp-config.json` is for an MCP client with an allowlisted native MCP config that starts its own server process. Merge its `mcpServers` block into your config; it runs `turbofig mcp` over stdio, with the absolute path to the `turbofig` binary (a GUI app does not have `/opt/homebrew/bin` on its `PATH`).

The default connection is the file bridge: run the turbofig plugin in Figma, press the link icon next to the file name (Copy connect prompt), and paste the prompt into your agent. Agents write jobs to `~/.turbofig/inbox/` and read results from `~/.turbofig/outbox/`.

MCP is optional. Set it up only when the user asks for it. See `docs/mcp-clients.md`.

In either case, paste `CLAUDE-snippet.md` into your global `~/.claude/CLAUDE.md`. That snippet tells any Claude instance how to discover and drive turbofig without further instructions.

**Not using Claude?** See [`../agents.md`](../agents.md) for an agent-neutral version of this onboarding text, for Cursor, Codex, Copilot, or any other agent.
