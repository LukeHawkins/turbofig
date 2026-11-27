# Configuration

All settings are environment variables. Each falls back to its default on
an absent, unparsable, or zero value.

| Variable | Default | Purpose |
|---|---|---|
| `TURBOFIG_MCP_PORT` | `18846` | HTTP MCP port |
| `TURBOFIG_WS_PORT` | `18847` | Plugin WebSocket port |
| `TURBOFIG_REQUEST_TIMEOUT_MS` | `30000` | Wait for a plugin reply before returning a timeout result. Clamped to `600000` |
| `TURBOFIG_BRIDGE_DIR` | `~/.turbofig` | File bridge home: the pairing token, the plugin files, and the inbox/outbox |

**Startup refusals.** The daemon refuses to start when `TURBOFIG_BRIDGE_DIR`
(or `~/.turbofig`), its `inbox/` or `outbox/`, or the `token` file is a
symlink or belongs to another user. It also refuses a `token` file that
group or world can access (fix: `chmod 600` it or delete it). A symlink
elsewhere in the path (a parent folder) is fine. The daemon also refuses a
folder when a parent folder belongs to another user (not root), or when
group or other can write to a parent folder that has no sticky bit. Fix:
`chmod go-w` the parent, or use a folder inside a folder you own. The 24 h
outbox prune runs only when `outbox/.turbofig-outbox` exists. The daemon
writes it in an outbox it creates, and in the default folder. A custom
folder with an older outbox is never pruned. The default folder is set
to mode 0700. A custom folder keeps its own mode, but the daemon refuses
a bridge folder that group or other can write to, even with the sticky bit. Some SMB
mounts report mode 0777 for every folder and are refused for that reason. If `HOME` is unset and
`TURBOFIG_BRIDGE_DIR` is not set, the daemon refuses to start. Under
autostart a refusal exits with code 0, so launchd does not restart it in a
loop. Fix the problem, then run `turbofig autostart on`.

**With autostart on** (`turbofig autostart on`), the launchd service carries
over whatever `TURBOFIG_*` variables were set at that moment. To change a
setting for the background service, set it and run `turbofig autostart on`
again.

**Without autostart**, these variables apply to the next `turbofig start`,
`turbofig serve`, or `turbofig mcp` (which starts the daemon itself if
needed).
