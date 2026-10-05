# tf Craft Helper API

`tf` is a helper namespace available in every `turbofig_execute` eval, alongside the `figma` global. The plugin runs under `documentAccess: dynamic-page`, so use async Figma APIs where required. Prefer `tf.*` over raw Figma node calls: it is shorter, handles font loading, and normalises common options.

If you drive `tf.*` code through the file-bridge, give every job file a unique id. Reusing an id while the first job is still running does not get a result: the daemon leaves the duplicate in the inbox, untouched, until the first job finishes.

---

## Function reference

| Function | Signature | Returns | Async | Purpose |
|---|---|---|---|---|
| `tf.color` | `(hex: string)` | `{r,g,b}` | no | Parse hex string to 0..1 RGB triple. Accepts `#RRGGBB`, `RRGGBB`, `#RGB`. Returns black on bad input. |
| `tf.solid` | `(hex: string, opacity?: number)` | `SolidPaint` | no | Build a `SolidPaint` from a hex string and an optional opacity (default 1). |
| `tf.loadFonts` | `(fonts: FontName[])` | `Promise<void>` | **yes** | Deduplicate and load all fonts in parallel. Call once before creating text nodes. |
| `tf.text` | `(opts: TextOpts)` | `Promise<TextNode>` | **yes** | Load the font, create a `TextNode`, and set characters, size, fill, and auto-resize behaviour. Pass `width` for body copy that wraps at a fixed column. |
| `tf.frame` | `(opts: FrameOpts)` | `FrameNode` | no | Create a `FrameNode` with auto-layout. Transparent by default; provide `fill` to colour it. Each axis sizes independently: a provided dimension is fixed on that axis; an omitted dimension hugs content on that axis. |
| `tf.rect` | `(opts: RectOpts)` | `RectangleNode` | no | Create a `RectangleNode` with a fixed size, optional fill, and optional corner radius. Transparent by default; provide `fill` to colour it. |
| `tf.clear` | `(node: BaseNode & ChildrenMixin)` | `void` | no | Remove all children from a node. Use after `findOrCreate` to rebuild children from scratch on every run. |
| `tf.append` | `(parent, ...children)` | `parent` (chainable) | no | Append one or more `SceneNode`s to a parent. Returns the parent for chaining. |
| `tf.findOrCreate` | `(parent, name, factory)` | `Promise<SceneNode>` | **yes** | Return an existing direct child named `name`, or call `factory`, name it, append it, and return it. |
| `tf.commit` | `(label?: string)` | `void` | no | Call `figma.commitUndo()` to mark the end of a batch undo step. |
| `tf.skipInvisible` | `(on?: boolean)` | `void` | no | Set `figma.skipInvisibleInstanceChildren` (default `true`). Skipping hidden instance children speeds up traversal on large documents. No-op when the property is absent. |
| `tf.findAll` | `(node, criteria)` | `SceneNode[]` | no | Wrap `node.findAllWithCriteria(criteria)`. Use for native type-based node queries, instead of a hand-written predicate. Example: `{ types: ["TEXT"] }`. |
| `tf.chunk` | `(items, size?)` | `T[][]` | no | Split an array into consecutive sub-arrays of at most `size` elements. Default `size` is 75. Throws `RangeError` when `size < 1`. |
| `tf.slide` | `(opts?: SlideOpts)` | `FrameNode` | no | Create a slide frame. Default size is 1920x1080 with name "Slide". Accepts all FrameOpts fields. |
| `tf.deck` | `(opts)` | `Promise<FrameNode[]>` | **yes** | Create `count` slide frames, position them in a grid, append each to `parent`, and call `build` per slide. Default `cols=1`, `gap=80`. |
| `tf.instance` | `(component: ComponentNode)` | `InstanceNode` | no | Return a new instance of the given component. |
| `tf.instanceByKey` | `(key: string)` | `Promise<InstanceNode>` | **yes** | Import a component by its key and return a new instance. |
| `tf.getVariable` | `(id: string)` | `Promise<Variable \| null>` | **yes** | Return the variable with the given id, or null when not found. |
| `tf.setVariableValue` | `(variable, modeId, value)` | `void` | no | Set a variable's value for the given mode id. |
| `tf.readVariableValue` | `(variable, modeId)` | `VariableValue \| undefined` | no | Read a variable's value for the given mode id. Returns undefined when the mode does not exist. |
| `tf.export` | `(node, settings?)` | `Promise<Uint8Array>` | **yes** | Export a node as an image. Defaults to PNG at 1x scale. Pass `ExportSettings` to control format and constraints. |
| `tf.slidePosition` | `(index, opts)` | `{x, y}` | no | Return the top-left grid position of a slide at `index`. Throws `RangeError` when `cols < 1` or `index < 0`. |

### SlideOpts

`SlideOpts` extends `FrameOpts`. All fields are optional. The slide factory sets these defaults before applying opts:

- `name`: `"Slide"`
- `width`: `1920`
- `height`: `1080`
- `direction`: `"NONE"` (a plain frame; slide children keep free x/y positions and are not auto-laid-out)

Pass any `FrameOpts` field to override a default. For example, `{ fill: "#1E1E1E" }` gives a dark slide at the standard 1920x1080 size.

### deck opts

```ts
{
  parent: BaseNode & ChildrenMixin; // Node to append slides to.
  count: number;                    // Number of slides to create.
  cols?: number;                    // Grid columns. Default 1.
  gap?: number;                     // Gap in pixels between slides. Default 80.
  build?: (slide: FrameNode, index: number) => void | Promise<void>;
}
```

### TextOpts

```ts
{
  text: string;
  size?: number;
  family?: string;
  style?: string;
  color?: string;
  autoResize?: "WIDTH_AND_HEIGHT" | "HEIGHT" | "NONE" | "TRUNCATE";
  width?: number;
}
```

Defaults: `size=16`, `family="Inter"`, `style="Regular"`, `color="#000000"`, `autoResize="WIDTH_AND_HEIGHT"`.

When you pass `width`, the node is resized to that pixel width and `autoResize` defaults to `"HEIGHT"`. The text wraps within the fixed width and the node grows vertically. This is the right setting for body copy inside a fixed-width container. When you do not pass `width`, the node grows on both axes to fit the text (standard single-line behaviour).

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

`fill` is optional. When omitted the rect is transparent (an empty fills array). Provide a hex string to fill it.

---

## Decks and slides

Use `tf.deck` to build a set of presentation slides in one call. Each slide is a 1920x1080 `FrameNode`. The optional `build` callback receives each slide and its index so you can populate content per slide.

```js
// Build a 3-slide deck in a 2-column grid.
// Preload the fonts once before tf.deck, not inside the build callback.
await tf.loadFonts([{ family: "Inter", style: "Bold" }]);
const slides = await tf.deck({
  parent: figma.currentPage,
  count: 3,
  cols: 2,
  gap: 80,
  build: async (slide, i) => {
    const title = await tf.text({ text: `Slide ${i + 1}`, size: 48, style: "Bold", color: "#FFFFFF" });
    tf.append(slide, title);
  },
});
tf.commit("deck");
return slides.map((s) => s.id);
```

Use `tf.slidePosition` when you need to compute grid positions without creating nodes:

```js
const pos = tf.slidePosition(4, { cols: 3, width: 1920, height: 1080, gap: 80 });
// pos -> { x: 2000, y: 1160 }
```

---

## Performance and batching

- Preload all fonts once with `await tf.loadFonts([...])` before creating any text nodes. `tf.loadFonts` deduplicates the list and calls `figma.loadFontAsync` for each font once, in parallel; `tf.text` then creates each node without loading its font again.
- Do as many node operations as possible in one eval call. Each `turbofig_execute` call has network overhead.
- Call `tf.commit(label)` once at the end of a batch to create a single undo step.
- Call `tf.skipInvisible()` at the start of any eval that scans the document. It sets `figma.skipInvisibleInstanceChildren = true`, which skips hidden instance children during traversal.
- Use `tf.findAll(node, { types: ["TEXT"] })` instead of `node.findAll(predicate)`. It calls the native `findAllWithCriteria`, rather than running a JS predicate over every node.
- Use `tf.chunk(nodes, size)` to process large node arrays in batches of at most `size` (default 75). Process each batch in sequence to avoid blocking the UI thread for long periods.

---

## Cross-page access

The plugin runs under `documentAccess: dynamic-page`. A node outside
`figma.currentPage` is not available until its page loads. Load the page
first with `await page.loadAsync()`, or use an async node lookup such as
`figma.getNodeByIdAsync` before you touch a node on another page.

```js
const pages = figma.root.children;
const otherPage = pages.find((p) => p.name === "Components");
await otherPage.loadAsync();
const nodes = tf.findAll(otherPage, { types: ["COMPONENT"] });
```

## Export and result limits

- Every queued reply (including `tf.export` output returned from
  `turbofig_execute`) is capped at 16 MiB. An oversized reply becomes an
  `ok:false` error naming the size, instead of reaching the daemon.
- `turbofig_screenshot`'s scale is clamped to `[0.1, 4]`, matching the range
  Figma itself accepts for an export constraint. `tf.export` passes its
  `settings` through to `exportAsync` unchanged, with no clamp.
- `turbofig_screenshot` defaults to file mode: the PNG is written to the
  file-bridge outbox and the call returns its path, instead of returning
  the image inline.

## instanceByKey and unpublished components

`tf.instanceByKey(key)` calls `figma.importComponentByKeyAsync(key)`. The
component must be published in a library the current file can use. When it
is not published, or the key is wrong, `importComponentByKeyAsync` throws
and the eval call fails with that error. `tf.instanceByKey` does not catch
or soften this error.

## Multiple open files

Each open Figma file runs its own plugin instance and its own WebSocket
connection to the daemon. When more than one file is open, pass `fileKey`
on every tool call (or file-bridge job) to target a specific file. Omit it
only when a single file is connected.

## Idempotency

`tf.findOrCreate` protects only the named node itself. Children appended inside the factory, or after it returns, are NOT protected. If the eval runs a second time, `findOrCreate` returns the existing node but then appends new children on top of the existing ones, accumulating duplicates.

To make the full section idempotent, call `tf.clear` immediately after `findOrCreate`, then rebuild the children:

```js
const section = await tf.findOrCreate(figma.currentPage, "HeroSection", () =>
  tf.frame({ direction: "VERTICAL", gap: 16, fill: "#F5F5F5" })
);
tf.clear(section);          // Remove any children from a previous run.
// Rebuild from scratch. Safe to run as many times as needed.
const heading = await tf.text({ text: "Hero Title", size: 24, style: "Bold" });
tf.append(section, heading);
```

Use a named `findOrCreate` call for each child if you need to preserve an individual child across re-runs without clearing its siblings.

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
// findOrCreate protects only the named section node.
// Clear its children so a re-run does not accumulate duplicates.
const section = await tf.findOrCreate(figma.currentPage, "HeroSection", () =>
  tf.frame({ name: "HeroSection", direction: "HORIZONTAL", gap: 16, padding: 32, fill: "#F5F5F5" })
);
tf.clear(section);

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
