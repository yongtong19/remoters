#!/usr/bin/env bash
set -euo pipefail

APT_PACKAGES=(
  build-essential
  pkg-config
  cmake
  curl
  ffmpeg
  libxkbcommon-dev
  libxkbcommon-x11-dev
  libwayland-dev
  libvulkan-dev
  libegl-dev
  libgbm-dev
  libpipewire-0.3-dev
  libdrm-dev
  libasound2-dev
  libxcb1-dev
  libxcb-shape0-dev
  libxcb-xfixes0-dev
  libxcb-render0-dev
  libxcb-randr0-dev
  libfontconfig1-dev
  libfreetype-dev
  libdbus-1-dev
  libclang-dev
  libssl-dev
  pulseaudio-utils
  xclip
)

usage() {
  cat <<EOF
Usage: $(basename "$0") [--install-deps] [--check-only]

Checks Ubuntu build/runtime dependencies and builds Remoters in release mode.

Options:
  --install-deps   Install missing apt packages automatically with sudo apt-get
  --check-only     Verify dependencies without running cargo build
  -h, --help       Show this help message
EOF
}

INSTALL_DEPS=0
CHECK_ONLY=0

for arg in "$@"; do
  case "$arg" in
    --install-deps) INSTALL_DEPS=1 ;;
    --check-only) CHECK_ONLY=1 ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $arg" >&2
      usage >&2
      exit 2
      ;;
  esac
done

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"

if [[ "$INSTALL_DEPS" -eq 1 ]]; then
  echo "Installing Ubuntu build and runtime dependencies..."
  sudo apt-get update
  sudo apt-get install -y "${APT_PACKAGES[@]}"
fi

if command -v dpkg-query >/dev/null 2>&1; then
  missing=()
  for pkg in "${APT_PACKAGES[@]}"; do
    if ! dpkg-query -W -f='${Status}' "$pkg" 2>/dev/null | grep -q "install ok installed"; then
      missing+=("$pkg")
    fi
  done
  if [[ ${#missing[@]} -gt 0 ]]; then
    echo "Missing Ubuntu packages: ${missing[*]}" >&2
    echo "Install them with:" >&2
    echo "  sudo apt-get update && sudo apt-get install -y ${missing[*]}" >&2
    echo "Or re-run with: $0 --install-deps" >&2
    exit 1
  fi
fi

for cmd in cargo rustc ffmpeg; do
  if ! command -v "$cmd" >/dev/null 2>&1; then
    echo "Required command not found in PATH: $cmd" >&2
    exit 1
  fi
done

if [[ "$CHECK_ONLY" -eq 1 ]]; then
  echo "All Ubuntu build and runtime dependencies are present."
  exit 0
fi

cd "$repo_root"
echo "Building remoters (--release --locked)..."
cargo build --release --locked -p remoters
echo "Built: $repo_root/target/release/remoters"
