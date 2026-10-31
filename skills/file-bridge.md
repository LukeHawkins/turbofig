# turbofig file-bridge

Drive the turbofig daemon with local files. No network call, no confirmation dialog, no MCP tool definitions. The daemon must run: run `turbofig start` once if no result appears.

## One-call recipe (shell)

Write the job, wait with a bounded loop, read the result. The daemon ignores names that do not end in `.json`, so write `<id>.json.tmp` and `mv` it: the write is atomic. The loop stops after 35 s, so a dead daemon cannot hang you.

```bash
I=job$RANDOM$RANDOM; B=~/.turbofig
cat > $B/inbox/$I.json.tmp <<'EOF2'
{"op":"execute","code":"return figma.root.name;"}
EOF2
mv $B/inbox/$I.json.tmp $B/inbox/$I.json; O=$B/outbox/$I.json
for i in $(seq 350); do [ -s $O ] && break; sleep 0.1; done; cat $O
```

With file tools only: write `inbox/<id>.json` in one operation, then read `outbox/<id>.json` once. The daemon retries a half-written file for a short grace time, so one complete write is the contract.

The bridge folder is `~/.turbofig`. `TURBOFIG_BRIDGE_DIR` overrides it.

**The id must be unique per job.** Never reuse an id while a job with that id may still run. The daemon holds a duplicate in the inbox until the first job ends.

## Rules

- Batch many operations in one job. One write plus one read beats ten.
- Read the result once. Never loop reads in separate tool calls.
- Shape results: return ids or compact rows, never whole nodes. The result enters your context verbatim.

## Ops

- `status`: daemon and plugin liveness.
- `execute`: `code` is async JS with `figma` and `tf.*` in scope. Use `return`.
- `get_selection`: compact `{id,name,type,x,y,w,h}` list.
- `screenshot`: optional `scale` (default 1), `nodeId` (default the selection), `return` (`file` default, or `inline`). File mode writes a PNG and returns its `path`.

Add `"fileKey":"<key>"` to target one of several open files. Omit it when one file is open.

## Results

```json
{ "ok": true, "plugin": { "connected": true, "fileKey": "abc123", "name": "My Design File" } }
```

`status` omits fields whose value is null, 0 or false. When no plugin is connected, `plugin.connected` is false and `plugin.reason` says why. Ask the user to open the plugin by hand (Plugins > Development > turbofig).

Errors: `{ "ok": false, "code": "...", "error": "..." }`. Act on `code`:

- `not_started`: the plugin never began the job. Safe to retry.
- `started_unknown`: the job may have run. Check the file first, then retry. Never duplicate a mutation.
- `busy`: wait `retryAfterMs`, then retry unchanged.
- `result_too_large`: the reply exceeded 16 MiB. Shape the request or use screenshot file mode.
