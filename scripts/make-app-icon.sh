#!/usr/bin/env bash
# Regenerates daemon/assets/app-icon/AppIcon.icns from a 1024x1024 PNG.
#
# Uses only tools that ship with macOS: sips (resize) and iconutil (build
# the .icns from an .iconset). No ImageMagick, no third-party dependency.
#
# Usage:
#   scripts/make-app-icon.sh [path/to/source-1024.png]
#
# With no argument, regenerates from the checked-in
# daemon/assets/app-icon/icon-1024.png. Pass a new 1024px PNG (the real
# brand icon) to replace the placeholder and rebuild the .icns from it; the
# script also copies that PNG over icon-1024.png so the source stays current.

set -euo pipefail

if [[ "$(uname)" != "Darwin" ]]; then
    echo "make-app-icon.sh: this script needs sips and iconutil, both macOS-only" >&2
    exit 1
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
assets_dir="$script_dir/../daemon/assets/app-icon"
source_png="${1:-$assets_dir/icon-1024.png}"

if [[ ! -f "$source_png" ]]; then
    echo "make-app-icon.sh: source PNG not found: $source_png" >&2
    exit 1
fi

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

iconset_dir="$work_dir/AppIcon.iconset"
mkdir -p "$iconset_dir"

# Every size and scale macOS expects inside a .iconset.
sips -z 16 16 "$source_png" --out "$iconset_dir/icon_16x16.png" >/dev/null
sips -z 32 32 "$source_png" --out "$iconset_dir/icon_16x16@2x.png" >/dev/null
sips -z 32 32 "$source_png" --out "$iconset_dir/icon_32x32.png" >/dev/null
sips -z 64 64 "$source_png" --out "$iconset_dir/icon_32x32@2x.png" >/dev/null
sips -z 128 128 "$source_png" --out "$iconset_dir/icon_128x128.png" >/dev/null
sips -z 256 256 "$source_png" --out "$iconset_dir/icon_128x128@2x.png" >/dev/null
sips -z 256 256 "$source_png" --out "$iconset_dir/icon_256x256.png" >/dev/null
sips -z 512 512 "$source_png" --out "$iconset_dir/icon_256x256@2x.png" >/dev/null
sips -z 512 512 "$source_png" --out "$iconset_dir/icon_512x512.png" >/dev/null
cp "$source_png" "$iconset_dir/icon_512x512@2x.png"

iconutil -c icns "$iconset_dir" -o "$work_dir/AppIcon.icns"

cp "$work_dir/AppIcon.icns" "$assets_dir/AppIcon.icns"
if [[ "$source_png" != "$assets_dir/icon-1024.png" ]]; then
    cp "$source_png" "$assets_dir/icon-1024.png"
fi

echo "make-app-icon.sh: wrote $assets_dir/AppIcon.icns"
