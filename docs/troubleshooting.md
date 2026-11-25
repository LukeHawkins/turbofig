# Troubleshooting

- **No tf icon in the menu bar?** Run `turbofig status`, then `turbofig`
  again.
- **The plugin panel shows disconnected, or says it is waiting.** Run
  `turbofig start`. Run `turbofig status` first to check the daemon is
  actually down.
- **"Import plugin from manifest" is missing from the Plugins menu.** Use
  Figma Desktop, not the Figma web app. The menu item does not exist there.
- **A port is already in use.** Run `turbofig stop` first. Then set
  `TURBOFIG_MCP_PORT` or `TURBOFIG_WS_PORT` to a free port and run
  `turbofig start` (or, with autostart on, `turbofig autostart on` again so
  the launchd plist picks up the new value). If an MCP client starts its
  own `turbofig mcp` process, pass the same port to it:
  `claude mcp add -e TURBOFIG_MCP_PORT=<port> turbofig -- turbofig mcp`. If
  you change the WebSocket port, also set the same port in the plugin
  panel's Advanced screen.
- **MCP is blocked on a managed machine.** Use the file bridge instead:
  press the link icon next to the file name in the plugin panel (Copy
  connect prompt), or read `skills/file-bridge.md` directly.
- **The panel or `turbofig status` warns of a version mismatch.** The
  daemon upgraded but the plugin in Figma has not reloaded. Reopen the
  turbofig plugin in Figma.
- **My agent uses Figma's own tools instead of turbofig.** In Claude Code
  you see lines such as "Called Figma 4 times · 2 failed". The server is
  Figma's own connector, listed as "claude.ai Figma".
  - Fix now: paste the turbofig connect prompt again. Say "use turbofig
    for Figma, not the Figma connector".
  - Prevent it for one project: in Claude Code, type `/mcp`, pick
    "claude.ai Figma", and disable it. Claude Code saves this per project.
  - Prevent it for every project: add this to `~/.claude/settings.json`.
    `{"deniedMcpServers": [{"serverName": "claude.ai Figma"}]}`
  - Check the active servers with `/mcp` or `claude mcp list`. Connector
    names can change, so use the name that these commands show.
- **Managed or corporate Mac.**
  - No admin rights: turbofig installs `turbofig.app` in `~/Applications`
    instead of `/Applications`. This is normal.
  - Homebrew is blocked: download the release from
    [GitHub Releases](https://github.com/LukeHawkins/turbofig/releases).
  - Antivirus software can slow the first start. Wait a minute, then run
    `turbofig status`.
  - turbofig uses 2 local ports, 18846 (HTTP) and 18847 (plugin
    WebSocket). Both stay on your Mac. If a security tool blocks local
    ports, ask your IT team to allow them.
- **A second macOS user on the same Mac.** The 2 ports are shared, so the
  second user's turbofig finds them taken. Set `TURBOFIG_MCP_PORT` and
  `TURBOFIG_WS_PORT` to free ports for that user, and set the same WebSocket
  port in the plugin panel's Advanced screen. See
  [configuration.md](configuration.md).
- **The plugin says "turbofig was reset. Close and reopen this plugin."**
  The pairing token changed, so the open plugin can no longer verify. Close
  the plugin and open it again (Plugins > Development > turbofig).
- **The plugin says "Another app may be using turbofig's port, or turbofig
  is older than this plugin."** Check these in order:
  1. Run `turbofig status`. If it names a running daemon, run `brew upgrade
     turbofig` and reopen the plugin.
  2. If it reports an error about the port, quit that app, or move
     turbofig to other ports (see "A port is already in use").
  3. Run `lsof -i :18847` to see which process holds the plugin port.
- **Uninstalling.** Run `turbofig uninstall` before `brew uninstall
  turbofig`. Homebrew alone leaves `turbofig.app` and the login item in
  place.
