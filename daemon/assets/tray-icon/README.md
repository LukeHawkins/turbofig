# Tray icon

`icon-tf-44.png` and `icon-tf-44-dimmed.png` are embedded into the menu-bar
app at build time (`include_bytes!` in `daemon/src/menu_bar/icon.rs`).

Both are 44x44 (the `@2x` size for a 22x22 menu-bar slot), black on a
transparent background, made from the brand source `docs/brand/menubar-glyph.png`.
`icon-tf-44.png` is the full-opacity glyph for the "a plugin is connected"
state. `icon-tf-44-dimmed.png` is the same glyph at 35% alpha, for the
"waiting for a plugin" or "daemon unreachable" state. Both PNGs are loaded as
macOS template images, so AppKit tints them for light and dark mode. Only the
alpha channel is used for a template image, which is why the dimmed variant
lowers alpha rather than using a lighter RGB fill.

## Regenerate the PNGs from the brand glyph

```sh
scripts/make-tray-icon.sh [path/to/menubar-glyph.png]
```

With no argument, it uses `docs/brand/menubar-glyph.png` (44x44, black on
transparent). It writes `icon-tf-44.png` with `sips`, and the 35% dimmed copy
`icon-tf-44-dimmed.png` with `scripts/dim-png.ts`.

Needs `sips` (ships with macOS) and Bun (already a repo prerequisite).
