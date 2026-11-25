#!/bin/bash
set -euo pipefail

binary="${1:-target/release/remoters}"
app="${2:-target/release/Remoters.app}"
script_dir="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/remoters"
cp "$script_dir/Info.plist" "$app/Contents/Info.plist"
cp "$script_dir/icons/remoters.icns" "$app/Contents/Resources/remoters.icns"
codesign --force --sign - "$app"
