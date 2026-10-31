# Connect any agent to turbofig

This file is onboarding text for an AI agent, not a human. Paste it into your agent's system prompt or project instructions (Cursor, Codex, GitHub Copilot, Claude, or any agent that can run a shell, read and write local files, or call an MCP server).

turbofig may be running on this machine. It drives the Figma file open in Figma Desktop through a local daemon and a thin Figma plugin.

## File bridge (prefer this)

Prefer the file bridge. It loads no MCP tool definitions and fires no permission dialog. The protocol, the one-call shell recipe, the ops, the result shapes and the retry rules for each error code are in `skills/file-bridge.md`.

The paths there assume `~/.turbofig`. If the daemon runs with a custom `TURBOFIG_BRIDGE_DIR`, use that folder. The copy-prompt in the plugin panel shows the real path.

## MCP (fallback)

Use MCP only when the file bridge is not available. If you can add an MCP server that runs as a child process over stdio, run `turbofig mcp`. It starts the daemon if needed and reads the pairing token itself.

If you can only speak HTTP, the daemon serves plain local HTTP on port 18846, not HTTPS. Use `curl`, never a web-fetch tool: a web-fetch tool forces HTTPS and fails. `/mcp` needs the pairing token (`~/.turbofig/token`) as a Bearer header:

```bash
curl -X POST http://127.0.0.1:18846/mcp \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $(cat ~/.turbofig/token)" \
  -d '...'
```

The 4 tools mirror the file-bridge ops: `turbofig_execute`, `turbofig_get_selection`, `turbofig_screenshot`, `turbofig_status`. Each takes an optional `fileKey`.

## Writing turbofig_execute code

The code is async JavaScript for the Figma Plugin API. `figma` and the `tf.*` helpers are in scope. See `helpers/tf-api.md` for the `tf.*` reference. Use `return`. Return ids, not nodes.

## Safety

`turbofig_execute` runs arbitrary Figma Plugin API JavaScript. Treat it like a script with full access to the open Figma file.
