Target file: bench-turbofig (fileKey ZdDvYYjKvLJEjXde6acpWE). This is the
sole Figma file connected to the turbofig plugin right now, so you may omit
fileKey from jobs, or pass it explicitly as shown above.

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

## Retrying a failed job

Every `ok:false` result carries a machine-readable `code`. Do not retry
blindly on every failure: that is how a handful of concurrent callers turns
into a thundering herd against one plugin connection.

- **`busy`**: the connection is already running the daemon's admission cap
  of jobs (default 4). Wait the given `retryAfterMs`, then retry unchanged.
- **`not_started`**: the plugin never began the job. Always safe to retry.
- **`started_unknown`**: the plugin began the job, then the call timed out
  or disconnected before a reply came back. The job may have run. For
  `execute` or `screenshot`, check the file first (e.g. a `get_selection`
  call) before retrying, so a node creation or a write is never silently
  duplicated.
- **`timeout`**: a `status` call got no reply. Harmless to retry; `status`
  never mutates.
- **`plugin_disconnected`**: no plugin was reachable at all. Safe to retry
  once one connects.
- **`file_not_connected`**: the `fileKey` you named is not open right now.
- **`script_error`**: your own code threw. Fix it; retrying as-is will not help.
- **`result_too_large`**: the reply exceeded the 16 MiB cap. Shape the
  request (fields/depth, or `screenshot`'s file mode) instead of retrying.

A `status` call's `plugin` object also reports queue health
(`pendingJobs`, `inFlight`, `saturated`) and, when no plugin is connected at
all, a `reason` and `lastDisconnectAgoMs`. turbofig cannot launch the Figma
plugin itself (Figma does not allow it): if `status` reports no plugin
connected after a Figma restart, tell the user to reopen it by hand
(Plugins > Development > turbofig in Figma).

## MCP (fallback)

Use this only when the file bridge is not available. If you can add an MCP
server that runs as a child process over stdio, run `turbofig mcp` that
way (this starts the daemon itself, if it is not already running, and
reads the pairing token itself). If you can only speak HTTP, the daemon
serves plain local HTTP on port 18846, not HTTPS. Use `curl`, never a
web-fetch tool: a web-fetch tool forces HTTPS and fails against a plain
HTTP port. `/mcp` requires the pairing token (`~/.turbofig/token`) as a
Bearer auth header:

```bash
curl -X POST http://127.0.0.1:18846/mcp \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $(cat ~/.turbofig/token)" \
  -d '...'
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
