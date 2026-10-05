# Connect any agent to turbofig

This file is onboarding text for an AI agent, not a human. Paste it into
your agent's system prompt or project instructions (Cursor, Codex, GitHub
Copilot, Claude, or any other agent that can read and write local files, or
call an MCP server over HTTP).

## What turbofig is

turbofig may be running on this machine. It drives the Figma file open in
Figma Desktop through a local daemon and a thin Figma plugin. There are two
ways to reach it.

## File bridge (prefer this)

Prefer the file bridge. It loads no MCP tool definitions and adds no
JSON-RPC or SSE envelope, and it fires no permission dialog.

These paths assume the default home, `~/.turbofig`. If the daemon runs with
a custom `TURBOFIG_BRIDGE_DIR`, use that folder instead; the copy-prompt in
the plugin panel shows the real path.

1. Pick a unique job id for every job. Never reuse an id while an earlier
   job with that id may still be running.
2. Write a JSON job file to `~/.turbofig/inbox/<id>.json`.
3. Read the result from `~/.turbofig/outbox/<id>.json`. If it is not there
   yet, wait a moment and read again, once.

Ops: `execute` (run JS, carried in a `code` field), `get_selection`,
`screenshot`, `status`. Add `"fileKey":"<key>"` to target one of several
open files; omit it for the sole open file.

Example job:

```json
{ "op": "execute", "code": "return figma.root.name;" }
```

Full protocol and job and result shapes: `skills/file-bridge.md`.

## MCP (fallback)

Use this only when the file bridge is not available. The daemon speaks
plain local HTTP on port 18846, not HTTPS. Use `curl`, never a web-fetch
tool: a web-fetch tool forces HTTPS and fails against a plain HTTP port.

```bash
curl -X POST http://127.0.0.1:18846/mcp -H "Content-Type: application/json" -d '...'
```

The 4 tools mirror the file-bridge ops: `turbofig_execute`,
`turbofig_get_selection`, `turbofig_screenshot`, `turbofig_status`. Each
takes an optional `fileKey`.

## Writing turbofig_execute code

`turbofig_execute` runs JavaScript against the Figma Plugin API, with a
`tf` helper namespace already in scope. See `helpers/tf-api.md` for the
full `tf.*` reference. Batch many node operations into one call: each call
has network overhead, so one call that builds ten nodes beats ten calls
that build one each.

## Safety

`turbofig_execute` runs arbitrary Figma Plugin API JavaScript. Treat it
like running a script with full access to the open Figma file.
