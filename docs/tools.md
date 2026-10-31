# The 4 tools

| Tool | What it does |
|---|---|
| `turbofig_execute` | Runs arbitrary Figma Plugin API JavaScript in the connected file and returns its result |
| `turbofig_get_selection` | Returns the current selection as a compact, shaped object |
| `turbofig_screenshot` | Exports a PNG of a given node, or the first selected node if none is given, downscaled by default |
| `turbofig_status` | Returns connection state for the daemon and the connected plugin |

Every tool accepts an optional `fileKey` to target one of several open
files. Omit it to use the paired or sole connected file.

## Reply shapes

Every reply is JSON with an `ok` key. MCP and the file bridge send the same JSON.
MCP sets `isError` when `ok` is false.

`turbofig_status` omits fields that only say "nothing is happening". It omits
null values, zero counters (`inFlight`, `pendingJobs`), `saturated:false`,
empty `plugins` lists and a `pluginVersion` that equals the daemon version.
`/health` lists files in the same per-file shape, so it omits the same null, 0 and false stats.

```json
{"ok":true,"plugin":{"connected":true,"fileKey":"fk1","name":"File 1"},"plugins":[{"fileKey":"fk1","name":"File 1"}]}
{"ok":true,"plugin":{"connected":false,"reason":"open the turbofig plugin in Figma"}}
```

`turbofig_execute` omits `result` when the result is null:

```json
{"ok":true,"result":"1:23"}
{"ok":true}
```

Errors carry a `code`:

```json
{"ok":false,"code":"not_started","error":"did not start, safe to retry"}
{"ok":false,"code":"started_unknown","error":"started, may have run: check before retry"}
{"ok":false,"code":"busy","error":"too many jobs in flight","queueDepth":3,"retryAfterMs":750}
```

A read over budget adds a `warning`:

```json
{"ok":true,"result":"...","warning":"23000 B over the 20000 B budget: use fields or depth"}
```

An inline screenshot over budget says "use file mode or maxDim".

`turbofig_execute` example, matching the helper API in `helpers/tf-api.md`:

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

A batch example, building many nodes from a data array in one
`turbofig_execute` call:

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
