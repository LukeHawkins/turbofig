# How it works

![How turbofig works](how-it-works.png)
*The daemon bridges your AI agent to the Figma plugin over a local
WebSocket.*

- The daemon is one Rust process. It runs the MCP HTTP endpoint, the
  plugin WebSocket server, and the file bridge as three tasks sharing one
  state.
- The Figma plugin UI holds a persistent WebSocket to the daemon, with
  infinite-backoff reconnect, so a plugin reopen re-pairs with no
  handshake.
- MCP HTTP (`POST /mcp`, port 18846) is the native-client path: a request
  arrives over HTTP, carrying the pairing token as a Bearer auth header, and
  the daemon forwards the call to the plugin.
- The file bridge (`~/.turbofig/inbox` and `outbox`) is the default path
  for a locked-down client: it writes a job file, the daemon's watcher
  picks it up, and it reads the result file back.
- The plugin WebSocket upgrade requires a per-install pairing token
  (`~/.turbofig/token`), so a malicious web page cannot open the socket
  even though it shares the plugin iframe's null Origin.

See [reliability.md](reliability.md) for restart and retry behaviour, and
[mcp-clients.md](mcp-clients.md) for the MCP connection paths.
