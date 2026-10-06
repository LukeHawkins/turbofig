# Tray icon

`icon-tf-44.png` and `icon-tf-44-dimmed.png` are embedded into the menu-bar
app at build time (`include_bytes!` in `daemon/src/menu_bar/icon.rs`).

Both are 44x44 (the `@2x` size for a 22x22 menu-bar slot), black on a
transparent background, placeholders: the letters "tf" in Helvetica Bold,
rendered from a small hand-written PDF with macOS's own `sips` (no image
library, no third-party tool). `icon-tf-44.pdf` is the full-opacity source
for the "a plugin is connected" state; `icon-tf-44-dimmed.pdf` is the same
glyph behind a PDF `ExtGState` with `/ca 0.35` (constant non-stroking alpha),
for the "waiting for a plugin" or "daemon unreachable" state. Both PNGs are
loaded as macOS template images, so AppKit tints them for light and dark
mode; only the alpha channel is ever used for a template image, which is why
the dimmed variant lowers alpha rather than using a lighter RGB fill (AppKit
ignores RGB on a template image entirely). The real brand glyph replaces both
placeholders later.

## Regenerate the PNGs from new source PDFs

```sh
scripts/make-tray-icon.sh [path/to/normal.pdf] [path/to/dimmed.pdf]
```

With no arguments, it rebuilds both PNGs from the current
`icon-tf-44.pdf`/`icon-tf-44-dimmed.pdf`. Pass 1 or 2 new sources (a vector
PDF with a 44x44pt `MediaBox` and a transparent background, or a 44x44 PNG
directly) to replace either placeholder; the script copies a given PDF
source over the checked-in one too, so the source always matches the PNG.

Needs `sips`, which ships with macOS. No other dependency.
