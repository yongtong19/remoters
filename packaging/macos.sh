#!/bin/bash
set -euo pipefail

binary="${1:-target/release/lan-remote}"
app="${2:-target/release/LanRemote.app}"
script_dir="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/lan-remote"
cp "$script_dir/Info.plist" "$app/Contents/Info.plist"
cp "$script_dir/icons/lan-remote.icns" "$app/Contents/Resources/lan-remote.icns"
codesign --force --sign - "$app"
