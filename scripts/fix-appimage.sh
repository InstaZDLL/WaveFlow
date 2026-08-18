#!/usr/bin/env bash
# Drop the bundled `libwayland-client.so.0` from a freshly built AppImage.
#
# Why this exists
# ---------------
# Tauri's AppImage bundler copies the webview's dependency closure into
# the AppDir, and that closure includes `libwayland-client.so.0`. That
# library is on the **official AppImage excludelist**:
#
#   libwayland-client.so.0
#   # https://gitlab.freedesktop.org/mesa/mesa/-/issues/11316
#   # New version of Mesa has some dependency issues with libwayland-client
#   # if it is bundled
#
# On a host with a newer Mesa than the build runner (Fedora 44 ships Mesa
# 26 and libwayland 1.25; we build on Ubuntu), the bundled copy wins the
# lookup and WebKit's WebProcess dies before it renders a single frame:
#
#   Could not create default EGL display: EGL_BAD_PARAMETER. Aborting...
#
# The Rust side is unaffected — the app starts, opens its audio device and
# shows an empty window, which is what made this look like a WebKitGTK
# version problem for months. It isn't: removing this one file from the
# bundle is enough, verified on Fedora 44 / GNOME Wayland / Mesa 26.1.6
# against the released v1.7.0 AppImage.
#
# Repacking is done with `mksquashfs` + the AppImage's own runtime header
# rather than `appimagetool`: nothing to download, no FUSE needed (the
# runner images don't all carry libfuse2), and the result is verified by
# re-extracting it before it replaces the original.
#
# Usage: scripts/fix-appimage.sh [path/to/App.AppImage]
#   With no argument, finds the single AppImage under
#   src-tauri/target/release/bundle/appimage/.
#
# Re-signs afterwards when the build produced an updater signature — the
# `.sig` covers the bytes we just rewrote, so a stale one would make every
# updater client reject the release. Signing reads the key from
# TAURI_SIGNING_PRIVATE_KEY / TAURI_SIGNING_PRIVATE_KEY_PASSWORD (env, so
# the key never reaches the process list).

set -euo pipefail

BUNDLE_DIR="src-tauri/target/release/bundle/appimage"
EXCLUDED_LIB="libwayland-client.so.0"

log() { printf '[fix-appimage] %s\n' "$*"; }
die() { printf '[fix-appimage] error: %s\n' "$*" >&2; exit 1; }

img="${1:-}"
if [ -z "$img" ]; then
  shopt -s nullglob
  candidates=("$BUNDLE_DIR"/*.AppImage)
  shopt -u nullglob
  [ ${#candidates[@]} -eq 0 ] && die "no .AppImage under $BUNDLE_DIR"
  [ ${#candidates[@]} -gt 1 ] && die "several AppImages under $BUNDLE_DIR, pass one explicitly"
  img="${candidates[0]}"
fi
[ -f "$img" ] || die "no such file: $img"
img="$(cd "$(dirname "$img")" && pwd)/$(basename "$img")"
chmod +x "$img"

command -v mksquashfs >/dev/null || die "mksquashfs not found (apt install squashfs-tools)"
command -v unsquashfs >/dev/null || die "unsquashfs not found (apt install squashfs-tools)"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

log "unpacking $(basename "$img")"
( cd "$work" && "$img" --appimage-extract >/dev/null )
appdir="$work/squashfs-root"
[ -d "$appdir" ] || die "extraction produced no squashfs-root"

# `usr/lib/libwayland-client.so.0` is the one that matters; the glob also
# catches a versioned sibling should the bundler ever emit one.
found=0
while IFS= read -r -d '' lib; do
  log "removing ${lib#$appdir/}"
  rm -f "$lib"
  found=1
done < <(find "$appdir" -name "${EXCLUDED_LIB}*" -print0)

if [ "$found" -eq 0 ]; then
  log "$EXCLUDED_LIB is not bundled — nothing to do (did the bundler stop shipping it?)"
  exit 0
fi

# Type-2 AppImage = runtime ELF header followed by a squashfs image, so a
# repack is: keep the original runtime, rebuild the filesystem behind it.
offset="$("$img" --appimage-offset)"
[ -n "$offset" ] || die "could not read the AppImage runtime offset"
head -c "$offset" "$img" > "$work/runtime"

# Repack with the compressor and block size the original used, read back
# from its own superblock. Forcing gzip here cost 11 MB on a 95 MB
# bundle — the runtime already mounts whatever the bundler chose, so
# there's no reason to second-guess it.
comp="$(unsquashfs -o "$offset" -s "$img" 2>/dev/null | awk '/^Compression/ {print $2; exit}')"
block="$(unsquashfs -o "$offset" -s "$img" 2>/dev/null | awk '/^Block size/ {print $3; exit}')"
comp="${comp:-gzip}"
block="${block:-131072}"

log "repacking (squashfs, $comp, ${block}B blocks)"
# -root-owned / -noappend match what appimagetool itself passes.
mksquashfs "$appdir" "$work/fs.squashfs" \
  -root-owned -noappend -comp "$comp" -b "$block" -no-progress >/dev/null
cat "$work/runtime" "$work/fs.squashfs" > "$work/out.AppImage"
chmod +x "$work/out.AppImage"

# Verify before overwriting: an AppImage that can't be re-opened, or that
# still carries the library, must never reach a release.
log "verifying the repacked image"
mkdir -p "$work/verify"
( cd "$work/verify" && "$work/out.AppImage" --appimage-extract >/dev/null ) \
  || die "the repacked AppImage does not extract"
if find "$work/verify/squashfs-root" -name "${EXCLUDED_LIB}*" | grep -q .; then
  die "$EXCLUDED_LIB survived the repack"
fi
[ -x "$work/verify/squashfs-root/AppRun" ] || die "the repacked AppImage has no AppRun"

mv "$work/out.AppImage" "$img"
log "$(basename "$img") rewritten ($(du -h "$img" | cut -f1))"

# The updater signature covers the bytes we just replaced.
if [ -f "$img.sig" ]; then
  [ -n "${TAURI_SIGNING_PRIVATE_KEY:-}" ] \
    || die "an updater signature exists but TAURI_SIGNING_PRIVATE_KEY is unset — refusing to leave a stale .sig"
  log "re-signing for the updater"
  rm -f "$img.sig"
  bun run tauri signer sign "$img" >/dev/null
  [ -f "$img.sig" ] || die "signing produced no $img.sig"
  log "signature refreshed"
else
  log "no updater signature alongside the bundle — nothing to re-sign"
fi
