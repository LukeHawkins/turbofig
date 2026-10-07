# Claude Code and other MCP clients

On a machine where MCP is not blocked, this is a lighter-weight connection
than the file bridge, with no clipboard step.

**Claude Code:**

```bash
claude mcp add turbofig -- turbofig mcp
```

This runs `turbofig mcp`, a stdio MCP server, the same way Claude Code
starts an `npx`-based MCP server. If the daemon is not running, `turbofig
mcp` starts it as a separate background process first. The daemon keeps
running after the agent session ends.

**Any other MCP client that starts its own server process:** add this
block, with the absolute path to the `turbofig` binary (a GUI app does not
have `/opt/homebrew/bin` on its `PATH`):

```json
{
  "mcpServers": {
    "turbofig": {
      "command": "/opt/homebrew/bin/turbofig",
      "args": ["mcp"]
    }
  }
}
```

See [discoverability/mcp-config.json](discoverability/mcp-config.json) for
this block, and
[discoverability/CLAUDE-snippet.md](discoverability/CLAUDE-snippet.md) for
a snippet to paste into a global `CLAUDE.md`.

**Advanced: MCP over HTTP.** The daemon also serves streamable HTTP MCP
directly at `http://127.0.0.1:18846/mcp`. Point any MCP client that
supports streamable HTTP at that URL if it cannot start its own stdio
server process. This path requires the pairing token as a Bearer auth
header:

```bash
claude mcp add --transport http turbofig http://127.0.0.1:18846/mcp \
  --header "Authorization: Bearer $(cat ~/.turbofig/token)"
```

The recommended path (`claude mcp add turbofig -- turbofig mcp`, above)
needs no header: the stdio proxy reads the token itself.
