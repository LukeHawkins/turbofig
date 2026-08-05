# tf Craft Helper API

`tf` is a helper namespace available in every `turbofig_execute` eval, alongside the `figma` global. The plugin runs under `documentAccess: dynamic-page`, so use async Figma APIs where required. Prefer `tf.*` over raw Figma node calls: it is shorter, handles font loading, and normalises common options.

---

## Function reference

| Function | Signature | Returns | Async | Purpose |
|---|---|---|---|---|
| `tf.color` | `(hex: string)` | `{r,g,b}` | no | Parse hex string to 0..1 RGB triple. Accepts `#RRGGBB`, `RRGGBB`, `#RGB`. Returns black on bad input. |
| `tf.solid` | `(hex: string, opacity?: number)` | `SolidPaint` | no | Build a `SolidPaint` from a hex string and an optional opacity (default 1). |
| `tf.loadFonts` | `(fonts: FontName[])` | `Promise<void>` | **yes** | Deduplicate and load all fonts in parallel. Call once before creating text nodes. |
| `tf.text` | `(opts: TextOpts)` | `Promise<TextNode>` | **yes** | Load the font, create a `TextNode`, and set characters, size, and fill. |
| `tf.frame` | `(opts: FrameOpts)` | `FrameNode` | no | Create a `FrameNode` with auto-layout. Transparent by default; provide `fill` to colour it. Each axis sizes independently: a provided dimension is fixed on that axis; an omitted dimension hugs content on that axis. |
| `tf.rect` | `(opts: RectOpts)` | `RectangleNode` | no | Create a `RectangleNode` with a fixed size, optional fill, and optional corner radius. |
| `tf.append` | `(parent, ...children)` | `parent` (chainable) | no | Append one or more `SceneNode`s to a parent. Returns the parent for chaining. |
| `tf.findOrCreate` | `(parent, name, factory)` | `Promise<SceneNode>` | **yes** | Return an existing direct child named `name`, or call `factory`, name it, append it, and return it. |
| `tf.commit` | `(label?: string)` | `void` | no | Call `figma.commitUndo()` to mark the end of a batch undo step. |

### TextOpts

```ts
{ text: string; size?: number; family?: string; style?: string; color?: string }
```

Defaults: `size=16`, `family="Inter"`, `style="Regular"`, `color="#000000"`.

### FrameOpts

```ts
{
  name?: string;
  direction?: "NONE" | "HORIZONTAL" | "VERTICAL"; // default "VERTICAL"
  gap?: number;
  padding?: number | { top?:number; right?:number; bottom?:number; left?:number };
  fill?: string;   // omit for a transparent frame; provide a hex string to fill
  width?: number;  // fixed on the width axis; omit to hug content on that axis
  height?: number; // fixed on the height axis; omit to hug content on that axis
  primaryAlign?: "MIN" | "CENTER" | "MAX" | "SPACE_BETWEEN";
  counterAlign?: "MIN" | "CENTER" | "MAX";
}
```

`width` and `height` are independent. Provide one, both, or neither. Each provided dimension fixes that axis; each omitted dimension hugs content on that axis. This lets you create "fixed width, hug height" frames without clipping.

`direction "NONE"` makes a plain frame with no auto-layout. On a plain frame, `gap`, `padding`, `primaryAlign`, `counterAlign`, and per-axis sizing modes are ignored. Only `width`, `height`, `fill`, and `name` apply.

### RectOpts

```ts
{ width: number; height: number; fill?: string; corner?: number }
```

---

## Performance and batching

- Preload all fonts once with `await tf.loadFonts([...])` before creating any text nodes. This avoids one network round-trip per node.
- Do as many node operations as possible in one eval call. Each `turbofig_execute` call has network overhead.
- Call `tf.commit(label)` once at the end of a batch to create a single undo step.

---

## Idempotency

Use `tf.findOrCreate(parent, name, factory)` for named sections. If a re-run or resume calls the same eval again, it finds the existing child instead of duplicating it.

---

## Examples

### 1. Auto-layout card with heading, body, and button

```js
await tf.loadFonts([
  { family: "Inter", style: "Bold" },
  { family: "Inter", style: "Regular" },
]);

const card = tf.frame({ name: "Card", direction: "VERTICAL", gap: 12, padding: 24, fill: "#FFFFFF" });

const heading = await tf.text({ text: "Card Title", size: 20, style: "Bold" });
const body    = await tf.text({ text: "Supporting text goes here.", size: 14 });
const btn     = tf.frame({ name: "Button", direction: "HORIZONTAL", padding: { top:10, right:20, bottom:10, left:20 }, fill: "#0066FF" });
const btnLabel = await tf.text({ text: "Action", size: 14, style: "Bold", color: "#FFFFFF" });

tf.append(btn, btnLabel);
tf.append(card, heading, body, btn);
figma.currentPage.appendChild(card);
tf.commit("card");
return card.id;
```

### 2. Idempotent section with a coloured rect

```js
const section = await tf.findOrCreate(figma.currentPage, "HeroSection", () =>
  tf.frame({ name: "HeroSection", direction: "HORIZONTAL", gap: 16, padding: 32, fill: "#F5F5F5" })
);

const thumb = tf.rect({ width: 120, height: 80, fill: "#CCCCCC", corner: 8 });
tf.append(section, thumb);
tf.commit("hero");
return section.id;
```

### 3. Batch of text nodes with one font preload

```js
const labels = ["Alpha", "Beta", "Gamma"];
await tf.loadFonts([{ family: "Inter", style: "Regular" }]);

const row = tf.frame({ name: "LabelRow", direction: "HORIZONTAL", gap: 8 });
for (const label of labels) {
  const t = await tf.text({ text: label, size: 12 });
  tf.append(row, t);
}
figma.currentPage.appendChild(row);
tf.commit("labels");
return row.id;
```
