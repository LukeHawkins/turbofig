# App icon

`AppIcon.icns` is embedded into `Turbofig.app` at build time (`include_bytes!`
in `daemon/src/app_bundle.rs`).

`icon-1024.png` is the 1024x1024 source. It is a placeholder: a dark rounded
square with the letters "tf" in white, built with macOS's own `sips` (no
image library, no third-party tool). The real brand icon replaces it later.

## Regenerate the .icns from a new 1024px PNG

```sh
scripts/make-app-icon.sh path/to/new-icon-1024.png
```

With no argument, it rebuilds `AppIcon.icns` from the current
`icon-1024.png`. The script copies the given PNG over `icon-1024.png` too, so
the checked-in source always matches the checked-in `.icns`.

Needs `sips` and `iconutil`, both ship with macOS. No other dependency.
