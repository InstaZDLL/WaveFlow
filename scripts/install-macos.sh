#!/bin/bash
# Install or update WaveFlow on macOS in one command:
#
#   curl -fsSL https://waveflow.app/install.sh | bash
#   curl -fsSL https://waveflow.app/install.sh | bash -s -- --beta
#
# Why this exists: the macOS bundle is signed ad hoc, not with a Developer
# ID, and it is not notarized (docs/RELEASING.md, "macOS signing and the TCC
# prompt"). A copy downloaded by a browser carries the com.apple.quarantine
# flag, and Gatekeeper refuses to open a quarantined app it cannot match to
# a notarized developer. This script downloads the release itself, checks
# what it can, installs WaveFlow.app and clears that flag on WaveFlow.app
# alone. It changes no system setting: Gatekeeper stays on for every other
# app. The real fix is a Developer ID signature plus notarization, after
# which this script is only a convenience.
#
# What it installs: WaveFlow_<version>_macos-universal.app.tar.gz from the
# GitHub release, the payload the in-app updater installs, rather than the
# DMG. Both wrap the same signed WaveFlow.app, but the archive needs no disk
# image to mount (hdiutil attach can fail, prompt, or stay mounted when a
# script stops halfway) and GitHub publishes a SHA-256 digest for it.
#
# What it checks, with the tools every Mac has:
#   - the archive's SHA-256 against the digest GitHub computed when the
#     asset was uploaded (read from the GitHub API; when the API does not
#     answer, for instance past its hourly limit for anonymous calls, the
#     check is skipped with a warning and the ones below still run);
#   - that the archive reads, and holds WaveFlow.app and nothing else;
#   - that the bundle is app.waveflow at the requested version, and that
#     this Mac meets the bundle's own LSMinimumSystemVersion;
#   - codesign --verify --strict, with sealed resources and the app.waveflow
#     identifier: every file of the bundle matches the signature sealed into
#     it, so a damaged download is refused.
# What it cannot check: who built it. An ad-hoc signature carries no
# identity, and the updater's .sig file is a minisign signature, which
# needs a tool macOS does not ship. Integrity comes from GitHub over HTTPS;
# authenticity has to wait for a Developer ID.
#
# Written for the bash 3.2 that macOS ships, using only tools that come with
# the system: no jq, no Homebrew. Everything runs from main(), called on the
# last line, so a download cut short by the network runs nothing.

set -euo pipefail

REPO="InstaZDLL/WaveFlow"
BUNDLE_ID="app.waveflow"
APP_NAME="WaveFlow.app"
# tauri.conf.json sets no bundle.macOS.minimumSystemVersion, so the bundler
# writes its default, 10.13, into LSMinimumSystemVersion (the x86_64 slice
# is built for 10.13 as well; the arm64 slice for 11.0, which every Apple
# silicon Mac runs). This is the early refusal, before anything is
# downloaded; the downloaded bundle's own value is checked again below, so
# a release that raises it is refused without this line being edited.
MIN_MACOS="10.13"
# /Applications, overridable for the test bench (scripts/tests/) only.
SYSTEM_APPS="${WAVEFLOW_SYSTEM_APPLICATIONS:-/Applications}"

say() { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Install or update WaveFlow on macOS.

  curl -fsSL https://waveflow.app/install.sh | bash
  curl -fsSL https://waveflow.app/install.sh | bash -s -- [options]

Options:
  --beta             Install the newest build of the beta channel (the
                     version the in-app updater offers with the beta
                     channel on; it can be a stable release when that is
                     newer than every beta).
  --version X.Y.Z    Install that exact version (a pre-release too, for
                     example 2.0.0-beta.1).
  --user             Install into ~/Applications instead of /Applications.
  --dir PATH         Install into PATH, an existing folder, instead.
  --force            Quit WaveFlow if the copy being replaced is running,
                     instead of stopping.
  --no-unquarantine  Leave the quarantine flag alone. If macOS then blocks
                     WaveFlow, open it once with right-click > Open (macOS
                     14 and earlier) or with System Settings > Privacy &
                     Security > Open Anyway (macOS 15 and later).
  --no-launch        Do not open WaveFlow once it is installed.
  --uninstall        Remove WaveFlow.app from /Applications and
                     ~/Applications (or from the --user or --dir folder
                     alone). Your libraries, playlists and settings are
                     kept; the script prints where they are.
  -h, --help         Show this help.

WaveFlow is not notarized by Apple yet. The installer removes the quarantine
flag from WaveFlow.app only, so macOS lets it open; it changes no system
setting. sudo is asked for only to write into /Applications when your
account cannot, never for the whole script.
EOF
}

# A release version: three numbers, then an optional pre-release suffix.
is_version() {
  printf '%s\n' "$1" | grep -E '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?$' >/dev/null
}

# version_ge A B: true when the dotted numbers of A are >= those of B.
# Compares up to three components and ignores any pre-release suffix, which
# is all that macOS versions and the downgrade warning need.
version_ge() {
  local a1 a2 a3 b1 b2 b3
  IFS=. read -r a1 a2 a3 <<EOF
${1%%-*}
EOF
  IFS=. read -r b1 b2 b3 <<EOF
${2%%-*}
EOF
  a1=${a1//[!0-9]/}
  a2=${a2//[!0-9]/}
  a3=${a3//[!0-9]/}
  b1=${b1//[!0-9]/}
  b2=${b2//[!0-9]/}
  b3=${b3//[!0-9]/}
  [ "${a1:-0}" -gt "${b1:-0}" ] && return 0
  [ "${a1:-0}" -lt "${b1:-0}" ] && return 1
  [ "${a2:-0}" -gt "${b2:-0}" ] && return 0
  [ "${a2:-0}" -lt "${b2:-0}" ] && return 1
  [ "${a3:-0}" -ge "${b3:-0}" ]
}

# HTTPS only, redirects included, with a couple of retries for a flaky link.
fetch() {
  curl -fsSL --proto '=https' --tlsv1.2 --retry 2 --connect-timeout 20 "$@"
}

# download FILE URL: the same, with a progress bar when someone is watching
# (-s, which keeps fetch quiet, would hide it).
download() {
  if [ -t 2 ]; then
    curl -fSL --progress-bar --proto '=https' --tlsv1.2 --retry 2 --connect-timeout 20 -o "$1" "$2"
  else
    fetch -o "$1" "$2"
  fi
}

# The version a channel's updater manifest names: the same files the in-app
# updater reads, so "stable" and "--beta" install what it would offer.
channel_version() {
  local url manifest version
  if [ "$1" = beta ]; then
    url="https://github.com/$REPO/releases/download/beta-channel/latest-beta.json"
  else
    url="https://github.com/$REPO/releases/latest/download/latest.json"
  fi
  manifest=$(fetch "$url") || die "Could not reach GitHub to find the latest $1 version."
  # The first "version" key is the manifest's own: it comes before the
  # platform entries, which carry none.
  version=$(printf '%s\n' "$manifest" |
    grep -Eo '"version"[[:space:]]*:[[:space:]]*"[^"]*"' |
    head -n 1 | sed -E 's/.*"([^"]*)"$/\1/') || true
  is_version "$version" || die "The $1 update manifest names no version."
  printf '%s\n' "$version"
}

# The SHA-256 GitHub recorded for one asset of a release, or nothing. The
# API's JSON is read line by line: the asset's "digest" comes after its
# "name" and before its "browser_download_url", so stopping at the latter
# never borrows the next asset's digest.
published_digest() {
  local version=$1 asset=$2 json
  json=$(fetch -H 'Accept: application/vnd.github+json' \
    "https://api.github.com/repos/$REPO/releases/tags/v$version" 2>/dev/null) || return 0
  printf '%s\n' "$json" | awk -v name="\"$asset\"" '
    index($0, "\"name\"") && index($0, name) { found = 1; next }
    found && index($0, "\"browser_download_url\"") { exit }
    found && index($0, "\"digest\"") {
      if (match($0, /sha256:[0-9a-f]+/)) print substr($0, RSTART + 7, RLENGTH - 7)
      exit
    }'
}

# is_running [DIR]: true while WaveFlow runs from DIR/WaveFlow.app, or from
# any copy without DIR. The executable path is compared as a plain string
# (DIR comes from pwd -P, the form the process list shows), never as a
# pattern, so a folder name with regex characters in it cannot misfire.
is_running() {
  local prefix="/$APP_NAME/Contents/MacOS/" anywhere=1
  if [ -n "${1:-}" ]; then
    prefix="$1/$APP_NAME/Contents/MacOS/"
    anywhere=0
  fi
  ps -axo command= 2>/dev/null | PREFIX=$prefix ANYWHERE=$anywhere awk '
    ENVIRON["ANYWHERE"] == 1 && index($0, ENVIRON["PREFIX"]) { found = 1 }
    ENVIRON["ANYWHERE"] == 0 && index($0, ENVIRON["PREFIX"]) == 1 { found = 1 }
    END { exit found ? 0 : 1 }'
}

# quit_running DIR: ask WaveFlow to quit the way the Dock does, so it saves
# its state, and wait for the copy in DIR to be gone. Never kills it.
quit_running() {
  local waited=0
  say "Quitting WaveFlow..."
  osascript -e "tell application id \"$BUNDLE_ID\" to quit" >/dev/null 2>&1 || true
  while is_running "$1"; do
    [ "$waited" -ge 20 ] && die "WaveFlow did not quit. Close it, then run the installer again."
    sleep 1
    waited=$((waited + 1))
  done
}

# Runs a command as the current user, or through sudo when the target
# folder is not writable for this account. Only the copy goes through here.
NEED_SUDO=0
as_owner() {
  if [ "$NEED_SUDO" = 1 ]; then
    sudo "$@"
  else
    "$@"
  fi
}

check_platform() {
  local os
  [ "$(uname -s)" = Darwin ] ||
    die "This installer is for macOS. For Windows and Linux, see https://github.com/$REPO#install"
  os=$(sw_vers -productVersion) || die "Could not read the macOS version."
  version_ge "$os" "$MIN_MACOS" || die "WaveFlow needs macOS $MIN_MACOS or later; this Mac runs $os."
  [ "$(id -u)" != 0 ] ||
    die "Run the installer as yourself, not with sudo: it asks for sudo only for the copy, when needed."
  printf '%s\n' "$os"
}

# uninstall DIR...: remove WaveFlow.app from each folder that has one.
uninstall() {
  local dir removed=0
  for dir in "$@"; do
    [ -e "$dir/$APP_NAME" ] || continue
    dir=$(cd "$dir" && pwd -P)
    if is_running "$dir"; then
      [ "$FORCE" = 1 ] ||
        die "WaveFlow is running from $dir. Quit it and run the uninstaller again, or pass --force."
      quit_running "$dir"
    fi
    NEED_SUDO=0
    [ -w "$dir" ] || NEED_SUDO=1
    [ "$NEED_SUDO" = 1 ] && say "Administrator rights are needed to remove $dir/$APP_NAME."
    as_owner rm -rf "$dir/$APP_NAME" || die "Could not remove $dir/$APP_NAME."
    say "Removed $dir/$APP_NAME."
    removed=1
  done
  [ "$removed" = 1 ] || say "WaveFlow.app was not found in $*."
  cat <<EOF
Your libraries, playlists and settings were kept. To remove them as well,
delete these folders:
  ~/Library/Application Support/$BUNDLE_ID   (libraries, settings, caches)
  ~/Library/Logs/$BUNDLE_ID                  (logs)
A cache folder you moved in Settings > Storage stays where you moved it.
EOF
}

main() {
  local channel=stable version="" user=0 dir="" unquarantine=1 launch=1 mode=install
  FORCE=0
  while [ $# -gt 0 ]; do
    case "$1" in
      --beta) channel=beta ;;
      --version)
        [ $# -ge 2 ] || die "--version needs a version, for example --version 1.9.8."
        version=${2#v}
        shift
        ;;
      --version=*)
        version=${1#--version=}
        version=${version#v}
        ;;
      --user) user=1 ;;
      --dir)
        [ $# -ge 2 ] || die "--dir needs a folder."
        dir=$2
        shift
        ;;
      --dir=*) dir=${1#--dir=} ;;
      --force) FORCE=1 ;;
      --no-unquarantine) unquarantine=0 ;;
      --no-launch) launch=0 ;;
      --uninstall) mode=uninstall ;;
      -h | --help)
        usage
        exit 0
        ;;
      *) die "Unknown option: $1 (see --help)" ;;
    esac
    shift
  done
  if [ -n "$version" ] && [ "$channel" = beta ]; then
    die "--beta and --version cannot be used together."
  fi
  if [ "$user" = 1 ] && [ -n "$dir" ]; then
    die "--user and --dir cannot be used together."
  fi
  if [ -n "$version" ] && ! is_version "$version"; then
    die "Not a version: $version (expected X.Y.Z or X.Y.Z-beta.N)."
  fi

  local os
  os=$(check_platform)

  # The folder in its physical form (/tmp is /private/tmp on macOS), as
  # the process list names a running copy.
  local dest
  if [ -n "$dir" ]; then
    [ -d "$dir" ] || die "$dir is not a folder."
    dest=$(cd "$dir" && pwd -P)
  elif [ "$user" = 1 ]; then
    [ "$mode" = uninstall ] || mkdir -p "$HOME/Applications" || die "Could not create ~/Applications."
    dest=$(cd "$HOME/Applications" 2>/dev/null && pwd -P) || dest="$HOME/Applications"
  else
    [ -d "$SYSTEM_APPS" ] || die "$SYSTEM_APPS does not exist."
    dest=$(cd "$SYSTEM_APPS" && pwd -P)
  fi

  if [ "$mode" = uninstall ]; then
    if [ -n "$dir" ] || [ "$user" = 1 ]; then
      uninstall "$dest"
    else
      uninstall "$dest" "$HOME/Applications"
    fi
    exit 0
  fi

  # Refuse before downloading anything rather than after.
  if is_running "$dest" && [ "$FORCE" != 1 ]; then
    die "WaveFlow is running. Quit it and run the installer again, or pass --force to quit it."
  fi

  if [ -z "$version" ]; then
    version=$(channel_version "$channel")
  fi

  # Everything downloaded or unpacked lives here and goes, whatever happens.
  WORK=$(mktemp -d "${TMPDIR:-/tmp}/waveflow-install.XXXXXX") || die "Could not create a temporary folder."
  trap 'rm -rf "$WORK"' EXIT
  trap 'exit 130' INT TERM

  local asset url archive
  asset="WaveFlow_${version}_macos-universal.app.tar.gz"
  url="https://github.com/$REPO/releases/download/v$version/$asset"
  archive="$WORK/$asset"

  say "Downloading WaveFlow $version..."
  download "$archive" "$url" || die "Could not download WaveFlow $version for macOS ($url)."

  local expected actual
  expected=$(published_digest "$version" "$asset") || true
  if [ -n "$expected" ]; then
    actual=$(shasum -a 256 "$archive" | awk '{ print $1 }')
    [ "$actual" = "$expected" ] ||
      die "The download does not match the checksum GitHub published for it. Nothing was installed."
    say "Checksum matches the one GitHub published."
  else
    warn "Could not read the checksum GitHub published (the API did not answer); relying on the signature check."
  fi

  # The archive must hold WaveFlow.app and nothing beside it. Each test
  # collects grep's whole output rather than asking grep -q, which stops
  # reading early: under pipefail the writer's SIGPIPE would then turn a
  # match into a failure, and a refusal into a pass.
  local listing outside
  listing=$(tar -tzf "$archive") || die "The download is not a readable archive. Nothing was installed."
  outside=$(printf '%s\n' "$listing" | grep -Ev '^WaveFlow\.app(/|$)' || true)
  outside=$outside$(printf '%s\n' "$listing" | grep -E '(^|/)\.\.(/|$)' || true)
  [ -z "$outside" ] || die "The archive holds files outside $APP_NAME. Nothing was installed."
  mkdir "$WORK/unpacked"
  tar -xzf "$archive" -C "$WORK/unpacked" || die "Could not unpack the archive. Nothing was installed."
  local app="$WORK/unpacked/$APP_NAME"
  [ -d "$app/Contents/MacOS" ] || die "The archive holds no $APP_NAME. Nothing was installed."

  local plist="$app/Contents/Info" found_id found_version needs
  found_id=$(defaults read "$plist" CFBundleIdentifier 2>/dev/null) || true
  found_version=$(defaults read "$plist" CFBundleShortVersionString 2>/dev/null) || true
  [ "$found_id" = "$BUNDLE_ID" ] || die "The archive is not WaveFlow ($found_id). Nothing was installed."
  [ "$found_version" = "$version" ] ||
    die "The archive holds version $found_version, not $version. Nothing was installed."
  needs=$(defaults read "$plist" LSMinimumSystemVersion 2>/dev/null) || true
  if [ -n "$needs" ] && ! version_ge "$os" "$needs"; then
    die "WaveFlow $version needs macOS $needs or later; this Mac runs $os."
  fi

  # The same three facts release.yml asserts before publishing: the seal
  # holds, it names app.waveflow, and it covers the bundle's resources (a
  # linker-only signature covers the executable alone).
  codesign --verify --strict "$app" >/dev/null 2>&1 ||
    die "$APP_NAME fails its code signature check: the download is damaged. Nothing was installed."
  local details
  details=$(codesign -dv "$app" 2>&1) || die "Could not read the signature of $APP_NAME. Nothing was installed."
  case "$details" in
    *"Identifier=$BUNDLE_ID"*) ;;
    *) die "$APP_NAME is not signed as $BUNDLE_ID. Nothing was installed." ;;
  esac
  case "$details" in
    *"Sealed Resources"*) ;;
    *) die "$APP_NAME has no sealed resources: its files are not covered by the signature. Nothing was installed." ;;
  esac
  say "Signature verified (ad hoc, $BUNDLE_ID)."

  if [ "$unquarantine" = 1 ]; then
    say "WaveFlow is not notarized by Apple yet, so macOS would refuse to open a quarantined copy."
    say "Removing the quarantine flag from WaveFlow.app only; no system setting is changed."
    xattr -dr com.apple.quarantine "$app" 2>/dev/null || true
    if xattr -p com.apple.quarantine "$app" >/dev/null 2>&1; then
      warn "The quarantine flag could not be removed; macOS may ask before opening WaveFlow."
    fi
  else
    say "Leaving the quarantine flag alone (--no-unquarantine)."
  fi

  # A version older than the installed one: a library a newer version
  # wrote is refused by an older one, so say so before it happens.
  local target="$dest/$APP_NAME" installed=""
  if [ -d "$target" ]; then
    installed=$(defaults read "$target/Contents/Info" CFBundleShortVersionString 2>/dev/null) || true
    if [ -n "$installed" ] && ! version_ge "$version" "$installed"; then
      warn "Installing $version over the newer $installed: an older version refuses a library a newer one has opened."
    fi
  fi

  if [ ! -w "$dest" ] || { [ -e "$target" ] && [ ! -w "$target" ]; }; then
    command -v sudo >/dev/null 2>&1 || die "$dest is not writable. Run again with --user to install into ~/Applications."
    NEED_SUDO=1
    say "Administrator rights are needed to write to $dest; sudo asks for your password for the copy only."
  fi

  if is_running "$dest"; then
    quit_running "$dest"
  fi

  # Copy beside the old app, swap, then drop the old one: a failure at any
  # step leaves a whole WaveFlow.app in place, old or new.
  local staging="$dest/.WaveFlow.app.installing" previous="$dest/.WaveFlow.app.previous"
  as_owner rm -rf "$staging" "$previous" || die "Could not clear an earlier attempt in $dest."
  as_owner ditto "$app" "$staging" || die "Could not copy $APP_NAME into $dest."
  if [ "$NEED_SUDO" = 1 ]; then
    # Owned by the user, as a copy dragged from a disk image would be, so
    # the in-app updater can replace it later without a password.
    as_owner chown -R "$(id -u):$(id -g)" "$staging" || warn "Could not hand $APP_NAME over to your account."
  fi
  if [ -e "$target" ]; then
    as_owner mv "$target" "$previous" || die "Could not move the installed $APP_NAME aside."
  fi
  if ! as_owner mv "$staging" "$target"; then
    [ -e "$previous" ] && as_owner mv "$previous" "$target"
    die "Could not put the new $APP_NAME in place; the previous one was kept."
  fi
  as_owner rm -rf "$previous" || warn "Could not remove the previous copy at $previous."

  if [ "$installed" = "$version" ]; then
    say "WaveFlow $version was reinstalled in $dest."
  elif [ -n "$installed" ]; then
    say "WaveFlow $installed was replaced by $version in $dest."
  else
    say "WaveFlow $version is installed in $dest."
  fi
  if [ -z "$dir" ] && [ "$user" = 1 ] && [ -d "$SYSTEM_APPS/$APP_NAME" ]; then
    warn "Another copy is still in $SYSTEM_APPS; macOS may open that one instead."
  elif [ -z "$dir" ] && [ "$user" = 0 ] && [ -d "$HOME/Applications/$APP_NAME" ]; then
    warn "Another copy is still in ~/Applications; macOS may open that one instead."
  fi

  if [ "$launch" = 1 ]; then
    # macOS brings a running WaveFlow forward rather than start a second
    # one, so opening the new copy then would show the old one.
    if is_running; then
      say "WaveFlow is already running from another copy; open the new one from $dest once it is closed."
    else
      open "$target" || warn "Could not open WaveFlow; find it in $dest."
    fi
  fi
}

main "$@"
