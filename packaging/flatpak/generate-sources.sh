#!/usr/bin/env bash
# Regenerate the Flatpak offline source manifests.
#
# Flathub disables network during `flatpak-builder`, so every Cargo
# crate and every npm tarball the build pulls in must be declared
# upfront as a `source` entry with a sha256. This script fetches the
# two upstream generators from flatpak/flatpak-builder-tools and runs
# them against the project's lockfiles.
#
# Outputs (commit these to git so Flathub's build sandbox can read
# them without network):
#   packaging/flatpak/generated/cargo-sources.json
#   packaging/flatpak/generated/node-sources.json
#
# Run from anywhere — the script resolves paths relative to its own
# location, so `bash packaging/flatpak/generate-sources.sh` works from
# the repo root, and the same command works from the packaging dir.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
GEN_DIR="$SCRIPT_DIR/generated"
CACHE_DIR="$SCRIPT_DIR/.cache"

mkdir -p "$GEN_DIR" "$CACHE_DIR"

# ----------------------------------------------------------------------------
# Pin the generators to a known-good commit on flatpak/flatpak-builder-tools.
# Bump deliberately, not opportunistically — generator changes can shift
# how vendored crates / tarballs are laid out and accidentally invalidate
# a previously-reviewed Flathub manifest.
GEN_REPO="https://raw.githubusercontent.com/flatpak/flatpak-builder-tools"
GEN_REF="96e2fe8bf7d2e5791ca1bdce2dba373f1e27c425"

CARGO_GEN="$CACHE_DIR/flatpak-cargo-generator.py"

if [ ! -f "$CARGO_GEN" ]; then
  echo "→ Fetching flatpak-cargo-generator.py @ ${GEN_REF:0:7}"
  curl -fsSL "$GEN_REPO/$GEN_REF/cargo/flatpak-cargo-generator.py" -o "$CARGO_GEN"
fi

# ----------------------------------------------------------------------------
# Python venv that holds both generator runtimes.
#   - flatpak-cargo-generator.py is a single script and only needs
#     `aiohttp` + `toml` at runtime.
#   - flatpak-node-generator has become a proper Python package
#     (poetry-managed) and is installed straight from the upstream
#     pinned commit. Once installed in the venv it exposes a
#     `flatpak-node-generator` console entrypoint.
VENV="$CACHE_DIR/venv"
NODE_GEN_PKG="git+https://github.com/flatpak/flatpak-builder-tools.git@${GEN_REF}#subdirectory=node"
if [ ! -d "$VENV" ]; then
  echo "→ Creating Python venv at $VENV"
  python3 -m venv "$VENV"
  "$VENV/bin/pip" install --quiet --upgrade pip
fi
"$VENV/bin/pip" install --quiet aiohttp toml tomlkit
"$VENV/bin/pip" install --quiet "$NODE_GEN_PKG"

PY="$VENV/bin/python"
NODE_GEN="$VENV/bin/flatpak-node-generator"

# ----------------------------------------------------------------------------
# Cargo sources — straight read of src-tauri/Cargo.lock.
# The local [patch.crates-io] entry for vendor/glib-0.18.5 is a path
# dep, which the generator skips automatically (only registry +
# git crates are emitted). The vendor dir reaches the sandbox via the
# `type: dir` source in the manifest.
echo "→ Generating cargo-sources.json"
"$PY" "$CARGO_GEN" \
  "$REPO_ROOT/src-tauri/Cargo.lock" \
  -o "$GEN_DIR/cargo-sources.json"

# ----------------------------------------------------------------------------
# Node sources — driven by a parallel package-lock.json regenerated
# from package.json. We don't actually run npm install here; we just
# materialize the lockfile so flatpak-node-generator can map each
# tarball to its registry URL + integrity hash.
#
# Why npm and not bun: flatpak-node-generator supports npm / yarn
# lockfiles, not bun.lock. Resolution drift between bun and npm is
# negligible for this project (the same package.json + same registry =
# same tarball hashes), but if it ever matters, the Flatpak build
# becomes the source of truth for what ships, not bun.lock.
echo "→ Generating package-lock.json (npm --package-lock-only)"
NPM_TMP="$CACHE_DIR/npm-workdir"
rm -rf "$NPM_TMP"
mkdir -p "$NPM_TMP"
cp "$REPO_ROOT/package.json" "$NPM_TMP/"
(cd "$NPM_TMP" && npm install --package-lock-only --ignore-scripts --no-audit --no-fund >/dev/null)

echo "→ Generating node-sources.json"
# --xdg-layout is now the default; not Electron, so no node headers.
"$NODE_GEN" \
  -o "$GEN_DIR/node-sources.json" \
  npm \
  "$NPM_TMP/package-lock.json"

# Tuck the package-lock.json alongside the sources file. The Flatpak
# manifest references it as a literal source so the in-sandbox
# `npm ci --offline` has something to read against.
cp "$NPM_TMP/package-lock.json" "$GEN_DIR/package-lock.json"

echo
echo "✓ Done. Outputs in $GEN_DIR:"
ls -lh "$GEN_DIR" | awk 'NR>1 {printf "    %s  %s\n", $5, $NF}'
echo
echo "Commit these alongside any package.json / Cargo.lock changes so"
echo "Flathub builds stay reproducible."
