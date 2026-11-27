# turbofig discoverability artifacts

The default and recommended connection is the file bridge: run the turbofig plugin in Figma, press the link icon next to the file name (Copy connect prompt), and paste the prompt into your agent. An agent with a shell runs `turbofig js` (start with `turbofig js --guide`); one without a shell writes jobs to `~/.turbofig/inbox/` and reads results from `~/.turbofig/outbox/`. A fresh install starts in file bridge mode. AI agents that install turbofig keep it that way.

MCP is optional, there for anyone who wants it. Set it up only when the user asks for it. See `docs/mcp-clients.md`. `mcp-config.json` is for an MCP client with a native MCP config that starts its own server process. Merge its `mcpServers` block into your config; it runs `turbofig mcp` over stdio, with the absolute path to the `turbofig` binary (a GUI app does not have `/opt/homebrew/bin` on its `PATH`).

In either case, paste `CLAUDE-snippet.md` into your global `~/.claude/CLAUDE.md`. That snippet tells any Claude instance how to discover and drive turbofig without further instructions.

**Not using Claude?** See [`../agents.md`](../agents.md) for an agent-neutral version of this onboarding text, for Cursor, Codex, Copilot, or any other agent.
