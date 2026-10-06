#!/usr/bin/env bash
# Regenerates the 2 tray-icon PNGs (normal + dimmed) from one brand glyph PNG.
#
# Usage:
#   scripts/make-tray-icon.sh [path/to/menubar-glyph.png]
#
# With no argument, it uses docs/brand/menubar-glyph.png. The source must be a
# 44x44 PNG (the @2x size of a 22x22 menu-bar slot), black on a transparent
# background. The menu bar loads both outputs as macOS template images, so
# AppKit tints them for light and dark mode and only the alpha channel is
# used. That is why the dimmed state lowers alpha (to 35%) instead of using a
# lighter colour.
#
# Needs `sips` (ships with macOS) and Bun (already a repo prerequisite), which
# runs scripts/dim-png.ts to make the dimmed variant.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
assets_dir="$repo_root/daemon/assets/tray-icon"
source_png="${1:-$repo_root/docs/brand/menubar-glyph.png}"
dimmed_alpha="0.35"

if [[ ! -f "$source_png" ]]; then
    echo "make-tray-icon.sh: source not found: $source_png" >&2
    exit 1
fi

normal_png="$assets_dir/icon-tf-44.png"
dimmed_png="$assets_dir/icon-tf-44-dimmed.png"

sips -s format png -z 44 44 "$source_png" --out "$normal_png" >/dev/null

bun "$repo_root/scripts/dim-png.ts" "$normal_png" "$dimmed_png" "$dimmed_alpha"

echo "make-tray-icon.sh: wrote $normal_png and $dimmed_png"
