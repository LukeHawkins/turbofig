# turbofig discoverability artifacts

`mcp-config.json` is for an MCP client with an allowlisted native MCP config that starts its own server process. Merge its `mcpServers` block into your config; it runs `turbofig mcp` over stdio, with the absolute path to the `turbofig` binary (a GUI app does not have `/opt/homebrew/bin` on its `PATH`).

Claude Code can add the same server with one command: `claude mcp add turbofig -- turbofig mcp`.

If MCP is blocked entirely, use the file-bridge instead. Write jobs to `~/.turbofig/inbox/` and read results from `~/.turbofig/outbox/`.

In either case, paste `CLAUDE-snippet.md` into your global `~/.claude/CLAUDE.md`. That snippet tells any Claude instance how to discover and drive turbofig without further instructions.

**Not using Claude?** See [`../agents.md`](../agents.md) for an agent-neutral version of this onboarding text, for Cursor, Codex, Copilot, or any other agent.
