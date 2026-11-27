# The 4 operations

Send each operation through the file bridge, the default: one short
`turbofig js` command from a shell, or a job file in the bridge inbox (see
[skills/file-bridge.md](../skills/file-bridge.md)). An MCP client sees the
same 4 operations as tools (see [MCP setup](mcp-clients.md)).

| Operation | File bridge (default) | MCP tool | What it does |
|---|---|---|---|
| execute | `turbofig js <<'EOF'` … `EOF` | `turbofig_execute` | Runs arbitrary Figma Plugin API JavaScript in the connected file and returns its result |
| get_selection | `{"op":"get_selection"}` on stdin | `turbofig_get_selection` | Returns the current selection as a compact, shaped object |
| screenshot | `turbofig js --shot` (with a job), or `{"op":"screenshot"}` | `turbofig_screenshot` | Exports a PNG of a given node, or the first selected node if none is given, 800 px by default |
| status | `{"op":"status"}` on stdin | `turbofig_status` | Returns connection state for the daemon and the connected plugins |

Every operation takes an optional file key (`--file KEY`, or `fileKey` over
MCP) to target one of several open files. Omit it to use the sole connected
file.

## Reply shapes

Every reply is JSON with an `ok` key. MCP and the file bridge send the same JSON.
MCP sets `isError` when `ok` is false.

status omits fields that only say "nothing is happening". It omits
null values, zero counters (`inFlight`, `pendingJobs`), `saturated:false`,
empty `plugins` lists and a `pluginVersion` that equals the daemon version.
`/health` lists files in the same per-file shape, so it omits the same null, 0 and false stats.

```json
{"ok":true,"plugin":{"connected":true,"fileKey":"fk1","name":"File 1"},"plugins":[{"fileKey":"fk1","name":"File 1"}]}
{"ok":true,"plugin":{"connected":false,"reason":"open the turbofig plugin in Figma"}}
```

execute omits `result` when the code returns nothing (null). `{"ok":true}` with no `result` key means the code returned nothing:

```json
{"ok":true,"result":"1:23"}
{"ok":true}
```

Errors carry a `code`:

```json
{"ok":false,"code":"not_started","error":"did not start, safe to retry"}
{"ok":false,"code":"started_unknown","error":"started, may have run: check before retry"}
{"ok":false,"code":"busy","error":"too many jobs in flight","queueDepth":3,"retryAfterMs":750}
{"ok":false,"code":"invalid_job","error":"..."}
```

`invalid_job` means the daemon could not parse the job. Fix the JSON and send it again with a new id.

A read over budget adds a `warning`:

```json
{"ok":true,"result":"...","warning":"23000 B over the 20000 B budget: use fields or depth"}
```

An inline screenshot over budget says "use file mode or maxDim".

An execute example, matching the helper API in `helpers/tf-api.md`. Send
it with `turbofig js <<'EOF'`, the code, then `EOF` on its own line. The
same code works as `turbofig_execute` code over MCP:

```js
await tf.loadFonts([{ family: "Inter", style: "Bold" }]);

const card = tf.frame({
  name: "Card",
  direction: "VERTICAL",
  gap: 12,
  padding: 24,
  fill: "#FFFFFF",
});

const heading = await tf.text({ text: "Card Title", size: 20, style: "Bold" });
tf.append(card, heading);
figma.currentPage.appendChild(card);
tf.commit("card");
return card.id;
```

A batch example, building many nodes from a data array in one job:

```js
const rows = [
  { label: "Alpha", color: "#0066FF" },
  { label: "Beta", color: "#00AA55" },
  { label: "Gamma", color: "#AA0066" },
];
await tf.loadFonts([{ family: "Inter", style: "Regular" }]);

const list = tf.frame({ name: "List", direction: "VERTICAL", gap: 8 });
for (const row of rows) {
  const chip = tf.frame({ direction: "HORIZONTAL", padding: 8, fill: row.color });
  const label = await tf.text({ text: row.label, size: 14, color: "#FFFFFF" });
  tf.append(chip, label);
  tf.append(list, chip);
}
figma.currentPage.appendChild(list);
tf.commit("list");
return list.id;
```
