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
  click the copy-prompt button in the plugin panel, or read
  `skills/file-bridge.md` directly.
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
- **Uninstalling.** Run `turbofig uninstall` before `brew uninstall
  turbofig`. Homebrew alone leaves `turbofig.app` and the login item in
  place.
