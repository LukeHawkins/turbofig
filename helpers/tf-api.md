# tf Craft Helper API

`tf` is in scope in every `turbofig_execute` call.

## Quick start

Rules:
- The code is an async JS body. `figma` and `tf` are in scope.
- Use `return` to send data back. Return ids, not nodes. A returned node comes back as `{id,name,type}`.
- Batch many operations in one call. Each call has overhead.
- `tf.text` loads fonts only for nodes it creates. To edit EXISTING text, run `await figma.loadFontAsync(node.fontName)` before you set `characters`.
- tf helpers reject unknown option names. The error lists the valid keys.
- Do not preload fonts for a few new text nodes.
- `turbofig_execute` rounds non-integer numbers in results to 4 decimals (selection x/y/w/h to 2). Do not read rounded coordinates as an error. To keep full precision, return the number as a string (`String(n)`).
- A bad hex colour throws an Error that names the value and the option.

Most used helpers:
```
tf.frame({name?, direction?, gap?, padding?, fill?, width?, height?, primaryAlign?, counterAlign?}) -> FrameNode
tf.text({text, size?, family?, style?, color?, width?, autoResize?}) -> Promise<TextNode>
tf.rect({width, height, fill?, corner?}) -> RectangleNode
tf.append(parent, ...children) -> parent
tf.findOrCreate(parent, name, factory) -> Promise<SceneNode>
tf.clear(node)                    // remove all children
tf.findAll(node, {types:["TEXT"]}) -> SceneNode[]
tf.solid("#RRGGBB", opacity?) -> SolidPaint
tf.slide(opts?) -> FrameNode      // 1920x1080
tf.deck({parent, count, cols?, gap?, build?}) -> Promise<FrameNode[]>
tf.instanceByKey(key) -> Promise<InstanceNode>
tf.commit(label?)                 // one undo step
```

Example:
```js
const card = tf.frame({ name: "Card", direction: "VERTICAL", gap: 12, padding: 24, fill: "#FFF" });
const title = await tf.text({ text: "Title", size: 20, style: "Bold" });
const body = await tf.text({ text: "Body copy.", size: 14, width: 240 });
tf.append(card, title, body);
figma.currentPage.appendChild(card);
tf.commit("card");
return card.id;
```

Find a node by name with native Figma: `figma.currentPage.findOne(n => n.name === "X")`.

---

## Reference

Colours (surrounding spaces are trimmed): `#RGB`, `#RRGGBB`, `#RRGGBBAA`, hash optional. Bad input throws. `tf.color` returns `{r,g,b}` and adds `a` for 8 digits. `tf.solid` maps alpha to the paint opacity and multiplies it with `opacity`.

| Function | Signature | Returns | Async | Purpose |
|---|---|---|---|---|
| `tf.color` | `(hex)` | `{r,g,b,a?}` | no | Parse hex to 0..1 RGB. |
| `tf.solid` | `(hex, opacity?)` | `SolidPaint` | no | Build a solid paint. Opacity default 1. |
| `tf.loadFonts` | `(fonts: FontName[])` | `Promise<void>` | yes | Deduplicate and load fonts in parallel. Optional: `tf.text` loads its own font. |
| `tf.text` | `(opts: TextOpts)` | `Promise<TextNode>` | yes | Load the font, create the node, set characters, size, fill, resize. |
| `tf.frame` | `(opts: FrameOpts)` | `FrameNode` | no | Auto-layout frame. Transparent without `fill`. |
| `tf.rect` | `(opts: RectOpts)` | `RectangleNode` | no | Fixed-size rectangle. Transparent without `fill`. |
| `tf.clear` | `(node)` | `void` | no | Remove all children. |
| `tf.append` | `(parent, ...children)` | `parent` | no | Append children. Chainable. |
| `tf.findOrCreate` | `(parent, name, factory)` | `Promise<SceneNode>` | yes | Return the direct child `name`, or create, name and append it. |
| `tf.findAll` | `(node, criteria)` | `SceneNode[]` | no | Native `findAllWithCriteria`, for example `{types:["TEXT"]}`. |
| `tf.commit` | `(label?)` | `void` | no | `figma.commitUndo()`. |
| `tf.slide` | `(opts?: SlideOpts)` | `FrameNode` | no | Slide frame. Defaults: name "Slide", 1920x1080, direction "NONE". |
| `tf.deck` | `(opts)` | `Promise<FrameNode[]>` | yes | Create `count` slides in a grid, append to `parent`, call `build(slide, i)`. |
| `tf.instanceByKey` | `(key)` | `Promise<InstanceNode>` | yes | Import a component by key, return an instance. |

### Options

```ts
TextOpts  { text: string; size?=16; family?="Inter"; style?="Regular"; color?="#000000";
            autoResize?: "WIDTH_AND_HEIGHT"|"HEIGHT"|"NONE"|"TRUNCATE"; width?: number }
FrameOpts { name?; direction?: "NONE"|"HORIZONTAL"|"VERTICAL" (default "VERTICAL"); gap?;
            padding?: number | {top?,right?,bottom?,left?}; fill?: hex; width?; height?;
            primaryAlign?: "MIN"|"CENTER"|"MAX"|"SPACE_BETWEEN"; counterAlign?: "MIN"|"CENTER"|"MAX" }
RectOpts  { width: number; height: number; fill?: hex; corner?: number }
SlideOpts = FrameOpts
deck      { parent; count; cols?=1; gap?=80; build?: (slide, index) => void | Promise<void> }
```

- `text` with `width`: the node gets that width and `autoResize` defaults to `"HEIGHT"` (wraps). Without `width` it grows on both axes.
- `frame` `width` and `height` are independent. A given value fixes that axis. An omitted value hugs content.
- `direction "NONE"` makes a plain frame. `gap`, `padding`, `primaryAlign`, `counterAlign` and sizing modes are ignored.

### tf opts vs raw Figma props

| tf opt | Figma prop |
|---|---|
| `direction` | `layoutMode` |
| `gap` | `itemSpacing` |
| `padding` | `paddingTop`, `paddingRight`, `paddingBottom`, `paddingLeft` |
| `primaryAlign` | `primaryAxisAlignItems` |
| `counterAlign` | `counterAxisAlignItems` |
| `fill`, `color` (hex) | `fills` (`Paint[]`) |
| `corner` | `cornerRadius` |
| `size` | `fontSize` |
| `family`, `style` | `fontName` |
| `autoResize` | `textAutoResize` |
| `text` | `characters` |
| `width`, `height` | `resize(w, h)` |

---

## Rarely needed

Thin wrappers:
- `tf.slidePosition(index, {cols, width, height, gap})` returns `{x,y}`. Throws `RangeError` when `cols < 1` or `index < 0`. Example: `tf.slidePosition(4, {cols:3, width:1920, height:1080, gap:80})` gives `{x:2000, y:1160}`.
- `tf.chunk(items, size = 75)` splits an array. Throws `RangeError` when `size < 1`.
- `tf.skipInvisible(on = true)` sets `figma.skipInvisibleInstanceChildren`. Call it before a large scan. No-op when the property is absent.
- `tf.readVariableValue(variable, modeId)` returns the value, or `undefined` when the mode is missing.
- `tf.setVariableValue(variable, modeId, value)` sets a variable value.
- `tf.getVariable(id)` returns `Promise<Variable | null>`.
- `tf.instance(component)` returns a new `InstanceNode`.
- `tf.export(node, settings?)` returns `Promise<Uint8Array>`. Default is PNG at 1x. Settings pass to `exportAsync` with no clamp.

Decks: the `deck` parent must not use auto-layout (it throws). Each slide is 1920x1080.
```js
const slides = await tf.deck({ parent: figma.currentPage, count: 3, cols: 2, gap: 80,
  build: async (slide, i) => {
    tf.append(slide, await tf.text({ text: `Slide ${i + 1}`, size: 48, style: "Bold", color: "#FFFFFF" }));
  } });
return slides.map((s) => s.id);
```

Cross-page: the plugin runs under `documentAccess: dynamic-page`. Load a page (`page.loadAsync()`) or use `figma.getNodeByIdAsync` before you touch its nodes.
```js
const page = figma.root.children.find((p) => p.name === "Components");
await page.loadAsync();
return tf.findAll(page, { types: ["COMPONENT"] }).map((n) => n.id);
```

Export and result limits:
- A queued reply is capped at 16 MiB. An oversized reply becomes an `ok:false` error that names the size.
- `turbofig_screenshot` scale is clamped to `[0.1, 4]`.
- `turbofig_screenshot` defaults to file mode. It writes the PNG to the file-bridge outbox and returns the path.
- `turbofig_screenshot` caps the longest edge at 800 px by default. Pass `maxDim` for another cap or `fullRes: true` for full size.

`instanceByKey` needs a published component. An unpublished component or a wrong key makes `importComponentByKeyAsync` throw. The error is not softened.

Multiple files: each open file has its own plugin and connection. Pass `fileKey` on every tool call or job when more than one file is open.

Idempotency: `findOrCreate` protects only the named node. A re-run appends children again. Call `tf.clear(section)` right after it, then rebuild. To keep one child, use a named `findOrCreate` per child.
```js
const section = await tf.findOrCreate(figma.currentPage, "Hero", () =>
  tf.frame({ direction: "HORIZONTAL", gap: 16, padding: 32, fill: "#F5F5F5" }));
tf.clear(section);
tf.append(section, tf.rect({ width: 120, height: 80, fill: "#CCC", corner: 8 }));
return section.id;
```

File-bridge: give every job file a unique id. While the first job runs, the daemon leaves a duplicate id untouched in the inbox until the first job finishes.
