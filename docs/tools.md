# The 4 tools

| Tool | What it does |
|---|---|
| `turbofig_execute` | Runs arbitrary Figma Plugin API JavaScript in the connected file and returns its result |
| `turbofig_get_selection` | Returns the current selection as a compact, shaped object |
| `turbofig_screenshot` | Exports a PNG of a given node, or the first selected node if none is given, downscaled by default |
| `turbofig_status` | Returns connection state for the daemon and the connected plugin |

Every tool accepts an optional `fileKey` to target one of several open
files. Omit it to use the paired or sole connected file.

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
