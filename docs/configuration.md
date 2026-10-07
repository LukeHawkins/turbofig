# Configuration

All settings are environment variables. Each falls back to its default on
an absent, unparsable, or zero value.

| Variable | Default | Purpose |
|---|---|---|
| `TURBOFIG_MCP_PORT` | `18846` | HTTP MCP port |
| `TURBOFIG_WS_PORT` | `18847` | Plugin WebSocket port |
| `TURBOFIG_REQUEST_TIMEOUT_MS` | `30000` | Wait for a plugin reply before returning a timeout result. Clamped to `600000` |
| `TURBOFIG_BRIDGE_DIR` | `~/.turbofig` | File-bridge home: the pairing token, the plugin files, and the inbox/outbox |

**With autostart on** (`turbofig autostart on`), the launchd service carries
over whatever `TURBOFIG_*` variables were set at that moment. To change a
setting for the background service, set it and run `turbofig autostart on`
again.

**Without autostart**, these variables apply to the next `turbofig start`,
`turbofig serve`, or `turbofig mcp` (which starts the daemon itself if
needed).
