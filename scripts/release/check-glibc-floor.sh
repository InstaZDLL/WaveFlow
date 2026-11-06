#!/usr/bin/env bash
# Fail when the Linux release needs a newer glibc than GLIBC_FLOOR.
#
#   GLIBC_FLOOR=2.39 check-glibc-floor.sh
#
# Run from the repository root after the AppImage is final (after
# fix-appimage.sh and appimage-update-info.sh), so it reads the image
# that ships. The binary is the one the .deb and .rpm carry; the
# AppImage adds the build machine's own libraries (WebKitGTK,
# GStreamer, …), each of which can raise the floor on its own.
#
# The floor is the build machine's glibc: a binary needs at least the
# one it was linked against. release.yml builds on ubuntu-24.04 and
# .gitlab-ci.yml in an ubuntu:24.04 image; a newer one means the image
# moved under us (docs/RELEASING.md, "Builds Linux on ubuntu-24.04").
set -euo pipefail
: "${GLIBC_FLOOR:?GLIBC_FLOOR is required, e.g. 2.39}"

img=$(ls src-tauri/target/release/bundle/appimage/*.AppImage | head -n 1)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
( cd "$work" && "$OLDPWD/$img" --appimage-extract >/dev/null )
# Empty for an ELF with no glibc symbols (a static one).
newest_of() {
  { objdump -T "$1" 2>/dev/null || true; } \
    | { grep -o 'GLIBC_[0-9.]*' || true; } | sed 's/GLIBC_//' | sort -uV | tail -n 1
}
above() { [ "$(printf '%s\n%s\n' "$1" "$GLIBC_FLOOR" | sort -V | tail -n 1)" != "$GLIBC_FLOOR" ]; }
binary=$(newest_of src-tauri/target/release/waveflow)
echo "waveflow needs glibc $binary (floor $GLIBC_FLOOR)."
if [ -z "$binary" ] || above "$binary"; then
  echo "::error::waveflow needs glibc ${binary:-?}, newer than $GLIBC_FLOOR: it would not start on older distributions."
  exit 1
fi
failed=0
checked=0
while IFS= read -r -d '' f; do
  [ "$(head -c 4 "$f" | od -An -c | tr -d ' ')" = '177ELF' ] || continue
  checked=$((checked + 1))
  v=$(newest_of "$f")
  if [ -n "$v" ] && above "$v"; then
    echo "::error::${f#"$work"/} needs glibc $v, newer than $GLIBC_FLOOR"
    failed=1
  fi
done < <(find "$work/squashfs-root" -type f -print0)
echo "Checked $checked ELF files in the AppImage."
exit "$failed"
