#!/usr/bin/env bash
# Write AppImage update information into a built AppImage.
#
# Why this exists
# ---------------
# A type-2 AppImage is an ELF runtime with a squashfs appended, and the
# runtime carries an empty 1024-byte `.upd_info` section reserved for a
# single string describing where updates come from. `appimagetool -u`
# fills it at build time; Tauri's bundler builds the AppImage itself and
# offers no way to pass that string, so the section ships zeroed and
# AppImageUpdate, AppImageLauncher, AM and friends all report "no update
# information available" (#527).
#
# Filling it is what makes delta updates possible: the client reads the
# string, fetches the matching `.zsync` control file, and downloads only
# the blocks that changed instead of the whole ~95 MB image.
#
# Written with `dd conv=notrunc` at the section's own file offset rather
# than `objcopy --update-section`. objcopy rebuilds the ELF, and this
# file is not just an ELF — everything after the runtime is a squashfs
# the runtime locates by a fixed offset. Rewriting the container would
# move that boundary and produce an AppImage that no longer mounts.
# Writing in place cannot: the section already exists, at a known offset,
# with room reserved.
#
# Ordering (release.yml depends on this)
# --------------------------------------
# This mutates the image, so it must run BEFORE the updater signature is
# computed and BEFORE `zsyncmake`. It runs after scripts/fix-appimage.sh,
# which repacks the squashfs — that script preserves the runtime header
# verbatim, so the order could be reversed, except its "nothing to do"
# early exit would then skip the re-signing this rewrite requires.
# Re-signs afterwards for the same reason fix-appimage.sh does: the `.sig`
# covers the bytes we just changed.
#
# Usage: scripts/appimage-update-info.sh [--update-info <string>] [path/to/App.AppImage]
#   Without --update-info, composes the GitHub-releases form from
#   GITHUB_REPOSITORY. With no path, finds the single AppImage under
#   src-tauri/target/release/bundle/appimage/.

set -euo pipefail

BUNDLE_DIR="src-tauri/target/release/bundle/appimage"
SECTION=".upd_info"

log() { printf '[update-info] %s\n' "$*"; }
die() { printf '[update-info] error: %s\n' "$*" >&2; exit 1; }

update_info=""
img=""
while [ $# -gt 0 ]; do
  case "$1" in
    --update-info) shift; [ $# -gt 0 ] || die "--update-info needs a value"; update_info="$1" ;;
    -*) die "unknown option: $1" ;;
    *) img="$1" ;;
  esac
  shift
done

if [ -z "$update_info" ]; then
  # gh-releases-zsync|<user>|<repo>|<release>|<filename>
  #
  # `latest` is a literal the transport understands: the client asks the
  # GitHub API for the latest release each time, so the string stays
  # correct for every future version. GitHub excludes pre-releases from
  # "latest", which is why release.yml only calls this on stable builds —
  # a beta pointing here would be walked *back* to the newest stable.
  #
  # The `*` stands in for the version in our asset naming. Both the
  # `.zsync` and the image it describes are assets of the same release,
  # so the relative URL zsyncmake records resolves alongside it.
  [ -n "${GITHUB_REPOSITORY:-}" ] \
    || die "no --update-info and GITHUB_REPOSITORY is unset — nothing to compose from"
  owner="${GITHUB_REPOSITORY%%/*}"
  repo="${GITHUB_REPOSITORY##*/}"
  # The third test is the one that matters: without a slash both
  # expansions return the whole string unchanged, so they look valid.
  if [ -z "$owner" ] || [ -z "$repo" ] || [ "$owner" = "$GITHUB_REPOSITORY" ]; then
    die "GITHUB_REPOSITORY is not owner/repo: $GITHUB_REPOSITORY"
  fi
  update_info="gh-releases-zsync|${owner}|${repo}|latest|WaveFlow_*_linux-x86_64.AppImage.zsync"
fi

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

command -v readelf >/dev/null || die "readelf not found (apt install binutils)"

# Offsets come from the image itself rather than a constant: the runtime
# is fetched by the bundler and its layout is not ours to assume.
read -r sec_off sec_size <<EOF
$(readelf -S -W "$img" 2>/dev/null | awk -v s="$SECTION" '$2 == s { print $5, $6; exit }')
EOF
if [ -z "${sec_off:-}" ] || [ -z "${sec_size:-}" ]; then
  die "no $SECTION section — this runtime does not support update information"
fi
sec_off=$((16#$sec_off))
sec_size=$((16#$sec_size))
log "$SECTION at offset $sec_off, $sec_size bytes"

# One byte is reserved for the terminator: the section is read as a
# C string, and a value filling it exactly would run into whatever
# follows.
len=${#update_info}
[ "$len" -lt "$sec_size" ] \
  || die "update information is $len bytes and $SECTION holds $sec_size"

# A rewrite that changed the file size, or moved the squashfs boundary,
# would produce an image that no longer mounts. Both are checked below
# rather than assumed.
size_before=$(wc -c < "$img")
offset_before=""
if [ -x "$img" ]; then
  offset_before="$("$img" --appimage-offset 2>/dev/null || true)"
fi

log "writing: $update_info"
# Zero the section first: a shorter string over a longer one would
# otherwise leave the old tail in place, and the reader stops at the
# first NUL, not at ours.
dd if=/dev/zero of="$img" bs=1 seek="$sec_off" count="$sec_size" conv=notrunc status=none
printf '%s' "$update_info" | dd of="$img" bs=1 seek="$sec_off" conv=notrunc status=none

written="$(dd if="$img" bs=1 skip="$sec_off" count="$sec_size" status=none | tr -d '\0')"
[ "$written" = "$update_info" ] \
  || die "read back '$written' after writing '$update_info'"

size_after=$(wc -c < "$img")
[ "$size_after" -eq "$size_before" ] \
  || die "the image changed size ($size_before -> $size_after)"

if [ -n "$offset_before" ]; then
  offset_after="$("$img" --appimage-offset 2>/dev/null || true)"
  [ "$offset_after" = "$offset_before" ] \
    || die "the squashfs offset moved ($offset_before -> $offset_after)"
  log "squashfs offset unchanged ($offset_before)"
fi

log "update information embedded"

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
