#!/usr/bin/env bash
# Native GTK has a separate Cargo workspace because GTK3 and GTK4 cannot
# resolve different glib-sys "links" versions in one Cargo dependency graph.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
CACHE_DIR="$SCRIPT_DIR/.cache"
GEN_REF="96e2fe8bf7d2e5791ca1bdce2dba373f1e27c425"
GENERATOR="$CACHE_DIR/flatpak-cargo-generator.py"
mkdir -p "$CACHE_DIR" "$SCRIPT_DIR/generated"
if [ ! -f "$GENERATOR" ]; then
  curl -fsSL "https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/$GEN_REF/cargo/flatpak-cargo-generator.py" -o "$GENERATOR"
fi
if [ ! -d "$CACHE_DIR/venv" ]; then
  python3 -m venv "$CACHE_DIR/venv"
fi
"$CACHE_DIR/venv/bin/pip" install --quiet aiohttp toml tomlkit
"$CACHE_DIR/venv/bin/python" "$GENERATOR" \
  "$REPO_ROOT/src-tauri/crates/gtk-app/Cargo.lock" \
  -o "$SCRIPT_DIR/generated/gtk-cargo-sources.json"
