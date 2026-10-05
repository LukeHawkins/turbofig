# turbofig: Figma design agent (local daemon)

turbofig may be running on this machine. It drives the open Figma file.

**Prefer the file-bridge.** It loads no MCP tool definitions and adds no JSON-RPC or SSE envelope, and it fires no permission dialog. Use MCP only when the file-bridge is not available.

## File-bridge (recommended)

Write a JSON job to `~/.turbofig/inbox/<id>.json` and read the result from `~/.turbofig/outbox/<id>.json` (any unique `<id>`). Ops: `execute` (JS in `code`), `get_selection`, `screenshot`, `status`. Add `"fileKey":"<key>"` to target one of several open files; omit it for the sole open file.

Example job: `{"op":"execute","fileKey":"<key>","code":"return figma.root.name;"}`

## MCP (fallback)

Claude Code: `claude mcp add turbofig -- turbofig mcp`. This starts a stdio MCP server that starts the daemon itself if it is not already running.

If you cannot add an MCP server this way, the daemon also speaks plain local HTTP on port 18846, not HTTPS. Use curl, never a web-fetch tool (it forces HTTPS and fails). `POST http://127.0.0.1:18846/mcp`. The four tools mirror the ops: `turbofig_execute`, `turbofig_get_selection`, `turbofig_screenshot`, `turbofig_status`, each with an optional `fileKey`.
