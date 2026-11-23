# turbofig: Figma design agent (local daemon)

Use the file-bridge: run the turbofig plugin in Figma, copy its prompt, and paste it into your agent. It works where MCP is blocked and needs no approval dialog. Add MCP only when the user asks for it.

## File-bridge (recommended)

Write a JSON job to `~/.turbofig/inbox/<id>.json` and read the result from `~/.turbofig/outbox/<id>.json` (any unique `<id>`). Ops: `execute` (async JS in `code`, with `figma` and `tf.*` in scope; use `return`, return ids), `get_selection`, `screenshot`, `status`. Add `"fileKey":"<key>"` to target one of several open files; omit it when one file is open. Full protocol and one-call shell recipe: `skills/file-bridge.md`.

Example job: `{"op":"execute","fileKey":"<key>","code":"return figma.root.name;"}`

## MCP (optional, only when the user asks)

Setup is in `docs/mcp-clients.md`. `turbofig mcp` is a stdio MCP server that starts the daemon itself if it is not already running.

If you cannot add an MCP server this way, the daemon also speaks plain local HTTP on port 18846, not HTTPS. Use curl, never a web-fetch tool (it forces HTTPS and fails). `POST http://127.0.0.1:18846/mcp`, with `Authorization: Bearer $(cat ~/.turbofig/token)`. The four tools mirror the ops: `turbofig_execute`, `turbofig_get_selection`, `turbofig_screenshot`, `turbofig_status`, each with an optional `fileKey`.
