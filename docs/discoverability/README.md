# Turbofig discoverability artifacts

`mcp-config.json` is for users whose Claude client allowlists native MCP servers. Merge its `mcpServers` block into your Claude desktop or CLI MCP config.

If native MCP is not allowlisted, use the file-bridge instead. Write jobs to `~/.turbofig/inbox/` and read results from `~/.turbofig/outbox/`.

In either case, paste `CLAUDE-snippet.md` into your global `~/.claude/CLAUDE.md`. That snippet tells any Claude instance how to discover and drive turbofig without further instructions.
