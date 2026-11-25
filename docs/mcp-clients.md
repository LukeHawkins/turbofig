# Claude Code and other MCP clients

This page is optional. The default connection is the file bridge: run the
plugin, press the link icon next to the file name (Copy connect prompt),
and paste the prompt into your agent. AI agents add an MCP
server only when the user asks for MCP.

On a machine where MCP is not blocked, this is a lighter-weight connection
than the file bridge, with no clipboard step.

**First step: switch Connection to MCP.** Open turbofig Settings (menu bar
icon, Settings…) and pick MCP in the Connection control, or pick MCP in the
Figma plugin (gear). Both use the same setting. The default is File bridge.
The mode matters only for the HTTP transport (in File bridge mode the daemon
refuses `/mcp` over HTTP and returns 404) and for which prompt the plugin
copies. Stdio setups such as the stdio server below (`turbofig mcp`) work in
either mode, including Claude Desktop.

**Claude Code:**

```bash
claude mcp add turbofig -- turbofig mcp
```

This runs `turbofig mcp`, a stdio MCP server, the same way Claude Code
starts an `npx`-based MCP server. If the daemon is not running, `turbofig
mcp` starts it as a separate background process first. The daemon keeps
running after the agent session ends.

**Claude Desktop:**

1. Open Settings > Developer > Edit Config. This opens
   `~/Library/Application Support/Claude/claude_desktop_config.json`.
2. Add the block below. Use the absolute path to `turbofig`
   (`/usr/local/bin/turbofig` on an Intel Mac).
3. Quit Claude Desktop fully, then open it again.

```json
{"mcpServers":{"turbofig":{"command":"/opt/homebrew/bin/turbofig","args":["mcp"]}}}
```

- This works in either Connection mode (it uses stdio).
- For screenshots, ask for `inline`. Claude Desktop cannot open a local file
  path. `inline` returns the image itself.
- The Code tab of Claude Desktop runs Claude Code, so the file bridge prompt
  works there too.
- Claude's custom connectors need a public HTTPS URL. The localhost HTTP
  transport is not for Claude Desktop.
- Company admins can turn local extensions off.

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
directly at `http://127.0.0.1:18846/mcp`, only when Connection is MCP. Point any MCP client that
supports streamable HTTP at that URL if it cannot start its own stdio
server process. Run `turbofig status` first: it checks that the process on the port
is your own daemon before you send the token. This path requires the pairing token as a Bearer auth
header:

```bash
claude mcp add --transport http turbofig http://127.0.0.1:18846/mcp \
  --header "Authorization: Bearer $(cat ~/.turbofig/token)"
```

The recommended path (`claude mcp add turbofig -- turbofig mcp`, above)
needs no header: the stdio proxy reads the token itself.
