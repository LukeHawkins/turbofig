#!/usr/bin/env bash
# Regenerates the 2 tray-icon PNGs (normal + dimmed) from a 44x44 source PDF,
# using only sips: macOS's own tool, no image library, no third-party
# dependency. Mirrors make-app-icon.sh's approach, adapted to a menu-bar
# template image (small, black glyph, transparent background) rather than a
# 1024px app icon.
#
# Usage:
#   scripts/make-tray-icon.sh [path/to/normal.pdf] [path/to/dimmed.pdf]
#
# With no arguments, regenerates both PNGs from the checked-in
# daemon/assets/tray-icon/icon-tf-44.pdf and icon-tf-44-dimmed.pdf.
#
# To swap in the real glyph: replace one or both PDFs (any vector PDF with a
# 44x44pt MediaBox and a transparent background works; `pdftoppm`/Illustrator/
# Figma can all export one), or pass a 44x44 PNG directly as either argument
# and the script copies it through sips unchanged. menu_bar.rs loads these 2
# PNGs as `tray-icon::Icon`s and marks both as template images, so macOS
# tints them for light and dark mode; only the alpha channel matters, the
# dimmed variant must carry lower alpha (not a lighter RGB value) to actually
# look dimmer once macOS applies its own template tint.

set -euo pipefail

if [[ "$(uname)" != "Darwin" ]]; then
    echo "make-tray-icon.sh: this script needs sips, macOS-only" >&2
    exit 1
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
assets_dir="$script_dir/../daemon/assets/tray-icon"

normal_source="${1:-$assets_dir/icon-tf-44.pdf}"
dimmed_source="${2:-$assets_dir/icon-tf-44-dimmed.pdf}"

render() {
    local source="$1"
    local dest_png="$2"
    local dest_source="$3"
    if [[ ! -f "$source" ]]; then
        echo "make-tray-icon.sh: source not found: $source" >&2
        exit 1
    fi
    sips -s format png "$source" --out "$dest_png" >/dev/null
    if [[ "$source" != "$dest_source" ]]; then
        cp "$source" "$dest_source"
    fi
}

render "$normal_source" "$assets_dir/icon-tf-44.png" "$assets_dir/icon-tf-44.pdf"
render "$dimmed_source" "$assets_dir/icon-tf-44-dimmed.png" "$assets_dir/icon-tf-44-dimmed.pdf"

echo "make-tray-icon.sh: wrote $assets_dir/icon-tf-44.png and icon-tf-44-dimmed.png"
