#!/usr/bin/env bash
# Test bench for scripts/install-macos.sh, runnable on Linux or Git Bash:
#
#   bash scripts/tests/install-macos.test.sh          # fakes only, offline
#   bash scripts/tests/install-macos.test.sh --live   # plus one real download
#
# No Mac needed and nothing is downloaded: the macOS tools the installer
# calls (uname, sw_vers, curl, codesign, xattr, osascript, ps, open,
# ditto, defaults, shasum, sudo, chown) are replaced by fakes in a
# temporary PATH, GitHub by fixture files, and /Applications by a folder
# (WAVEFLOW_SYSTEM_APPLICATIONS). What this proves is the script's logic:
# which version it picks, where it installs, what it refuses, that it
# touches the quarantine flag of WaveFlow.app only and that its temporary
# folder is gone afterwards, failures included. What it cannot prove is how
# the real macOS tools answer; that needs a Mac.
#
# --live adds one case with the real curl: it reads the real stable
# manifest and the GitHub API, downloads the real archive (about 60 MB),
# checks its digest and that it holds WaveFlow.app alone, and "installs" it
# into a temporary folder. The macOS tools stay fakes, codesign included.

set -u

HERE=$(cd "$(dirname "$0")" && pwd)
SCRIPT="$HERE/../install-macos.sh"
ROOT=$(mktemp -d "${TMPDIR:-/tmp}/waveflow-install-test.XXXXXX")
# The installer resolves the folders it is given; on macOS TMPDIR is under
# /var, a link to /private/var, so the paths the cases expect must be
# resolved the same way.
ROOT=$(cd -P "$ROOT" && pwd)
trap 'chmod -R u+w "$ROOT" 2>/dev/null; rm -rf "$ROOT"' EXIT

FAKEBIN="$ROOT/bin"
FIX="$ROOT/fixtures"
mkdir -p "$FAKEBIN" "$FIX/assets" "$FIX/build"

fake() {
  cat >"$FAKEBIN/$1"
  chmod +x "$FAKEBIN/$1"
}

fake uname <<'EOF'
#!/usr/bin/env bash
echo "${FAKE_UNAME:-Darwin}"
EOF

fake sw_vers <<'EOF'
#!/usr/bin/env bash
echo "${FAKE_MACOS:-14.5}"
EOF

# Serves the fixtures by URL and logs every URL asked for.
fake curl <<'EOF'
#!/usr/bin/env bash
out="" url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out=$2; shift ;;
    -H | --proto | --retry | --connect-timeout) shift ;;
    https://*) url=$1 ;;
  esac
  shift
done
echo "curl $url" >>"$FAKE_LOG"
emit() { if [ -n "$out" ]; then cat "$1" >"$out"; else cat "$1"; fi; }
case "$url" in
  */latest.json | */latest-beta.json)
    [ -z "${FAKE_MANIFEST_DOWN:-}" ] || exit 22 ;;
esac
case "$url" in
  https://github.com/InstaZDLL/WaveFlow/releases/latest/download/latest.json)
    emit "$FIX/latest.json" ;;
  https://github.com/InstaZDLL/WaveFlow/releases/download/beta-channel/latest-beta.json)
    emit "$FIX/latest-beta.json" ;;
  https://github.com/InstaZDLL/WaveFlow/releases/download/v*/*)
    f="$FIX/assets/${url##*/}"
    [ -f "$f" ] || exit 22
    emit "$f" ;;
  https://api.github.com/repos/InstaZDLL/WaveFlow/releases/tags/v*)
    [ -z "${FAKE_API_DOWN:-}" ] || exit 22
    v=${url##*/v}
    a="WaveFlow_${v}_macos-universal.app.tar.gz"
    [ -f "$FIX/assets/$a" ] || exit 22
    # sha256sum on Linux, shasum on macOS, which has no sha256sum.
    if command -v sha256sum >/dev/null 2>&1; then
      d=$(sha256sum "$FIX/assets/$a" | cut -d' ' -f1)
    else
      d=$(/usr/bin/shasum -a 256 "$FIX/assets/$a" | cut -d' ' -f1)
    fi
    [ -z "${FAKE_BAD_DIGEST:-}" ] || d=0000000000000000000000000000000000000000000000000000000000000000
    digest_line="      \"digest\": \"sha256:$d\","
    [ -z "${FAKE_NO_DIGEST:-}" ] || digest_line=""
    cat <<JSON
{
  "tag_name": "v$v",
  "name": "v$v",
  "assets": [
    {
      "name": "$a",
      "uploader": {
        "login": "github-actions[bot]"
      },
      "size": 1,
$digest_line
      "browser_download_url": "https://github.com/InstaZDLL/WaveFlow/releases/download/v$v/$a"
    },
    {
      "name": "$a.sig",
      "digest": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
      "browser_download_url": "https://github.com/InstaZDLL/WaveFlow/releases/download/v$v/$a.sig"
    }
  ]
}
JSON
    ;;
  *) exit 22 ;;
esac
EOF

fake codesign <<'EOF'
#!/usr/bin/env bash
echo "codesign $*" >>"$FAKE_LOG"
case "$1" in
  --verify) exit "${FAKE_CODESIGN_RC:-0}" ;;
  -dv)
    echo "Identifier=${FAKE_CODESIGN_ID:-app.waveflow}" >&2
    # What codesign prints for a linker-only signature, and for a sealed one.
    if [ -n "${FAKE_LINKER_SIGNED:-}" ]; then
      echo "Sealed Resources=none" >&2
    else
      echo "Sealed Resources version=2 rules=13 files=4" >&2
    fi
    ;;
esac
EOF

fake xattr <<'EOF'
#!/usr/bin/env bash
echo "xattr $*" >>"$FAKE_LOG"
[ "$1" = -p ] && exit 1
exit 0
EOF

fake osascript <<'EOF'
#!/usr/bin/env bash
echo "osascript $*" >>"$FAKE_LOG"
touch "$FAKE_STATE/quit"
EOF

# The process list: a WaveFlow runs from the path written in state/running
# until osascript asks it to quit.
fake ps <<'EOF'
#!/usr/bin/env bash
echo "/usr/libexec/some-daemon --flag"
if [ -e "$FAKE_STATE/running" ] && [ ! -e "$FAKE_STATE/quit" ]; then
  echo "$(cat "$FAKE_STATE/running")/WaveFlow.app/Contents/MacOS/waveflow"
fi
EOF

fake open <<'EOF'
#!/usr/bin/env bash
echo "open $*" >>"$FAKE_LOG"
EOF

fake ditto <<'EOF'
#!/usr/bin/env bash
cp -R "$1" "$2"
EOF

# defaults read <path without .plist> <key>, for the one-string keys asked.
fake defaults <<'EOF'
#!/usr/bin/env bash
[ "$1" = read ] || exit 1
awk -v key="<key>$3</key>" '
  found { if (match($0, /<string>[^<]*<\/string>/)) { print substr($0, RSTART + 8, RLENGTH - 17); ok = 1 } exit }
  index($0, key) { found = 1 }
  END { exit ok ? 0 : 1 }' "$2.plist"
EOF

fake shasum <<'EOF'
#!/usr/bin/env bash
if command -v sha256sum >/dev/null 2>&1; then sha256sum "$3"; else /usr/bin/shasum -a 256 "$3"; fi
EOF

# Lends write access to the fake /Applications for one command.
fake sudo <<'EOF'
#!/usr/bin/env bash
echo "sudo $*" >>"$FAKE_LOG"
chmod u+w "$WAVEFLOW_SYSTEM_APPLICATIONS"
"$@"
rc=$?
chmod u-w "$WAVEFLOW_SYSTEM_APPLICATIONS"
exit $rc
EOF

# The installer refuses to run as root; the bench may run as root (a
# container), so the uid is the case's, 1000 unless FAKE_UID says.
fake id <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  -u) echo "${FAKE_UID:-1000}" ;;
  -g) echo 1000 ;;
  *) /usr/bin/id "$@" ;;
esac
EOF

fake chown <<'EOF'
#!/usr/bin/env bash
echo "chown $*" >>"$FAKE_LOG"
EOF

# make_release VERSION [MIN_MACOS] [STRAY_FILE]: a fake WaveFlow.app archive.
make_release() {
  local v=$1 min=${2:-10.13} stray=${3:-} dir="$FIX/build/$1"
  mkdir -p "$dir/WaveFlow.app/Contents/MacOS" "$dir/WaveFlow.app/Contents/_CodeSignature"
  printf '#!/bin/sh\n' >"$dir/WaveFlow.app/Contents/MacOS/waveflow"
  printf 'sealed\n' >"$dir/WaveFlow.app/Contents/_CodeSignature/CodeResources"
  cat >"$dir/WaveFlow.app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>app.waveflow</string>
	<key>CFBundleShortVersionString</key>
	<string>$v</string>
	<key>LSMinimumSystemVersion</key>
	<string>$min</string>
</dict>
</plist>
PLIST
  if [ -n "$stray" ]; then
    printf 'x\n' >"$dir/$stray"
    (cd "$dir" && tar -czf "$FIX/assets/WaveFlow_${v}_macos-universal.app.tar.gz" WaveFlow.app "$stray")
  else
    (cd "$dir" && tar -czf "$FIX/assets/WaveFlow_${v}_macos-universal.app.tar.gz" WaveFlow.app)
  fi
}

make_release 1.9.8
make_release 1.9.5
make_release 2.0.0-beta.1
make_release 6.6.6 10.13 evil.sh
make_release 7.0.0 15.0
# An entry that climbs out of WaveFlow.app through "..": only GNU tar can
# write one from the command line, so the case is skipped elsewhere.
DOTDOT=0
if tar --version 2>/dev/null | grep GNU >/dev/null; then
  make_release 6.6.7
  dir="$FIX/build/6.6.7"
  printf 'x\n' >"$dir/evil.sh"
  (cd "$dir" && tar -czf "$FIX/assets/WaveFlow_6.6.7_macos-universal.app.tar.gz" \
    --transform 's|^evil\.sh$|WaveFlow.app/../evil.sh|' WaveFlow.app evil.sh) && DOTDOT=1
fi
# A hard link inside WaveFlow.app: tar extracts it as a link to its
# target, so the installer must refuse it by its kind, not its name.
HARDLINK=0
make_release 6.6.8
dir="$FIX/build/6.6.8"
if ln "$dir/WaveFlow.app/Contents/Info.plist" "$dir/WaveFlow.app/Contents/MacOS/linked" 2>/dev/null; then
  (cd "$dir" && tar -czf "$FIX/assets/WaveFlow_6.6.8_macos-universal.app.tar.gz" WaveFlow.app) &&
    tar -tvzf "$FIX/assets/WaveFlow_6.6.8_macos-universal.app.tar.gz" | cut -c1 | grep '^h$' >/dev/null &&
    HARDLINK=1
fi
printf '{\n  "version": "1.9.8",\n  "notes": "",\n  "platforms": {}\n}\n' >"$FIX/latest.json"
printf '{\n  "version": "2.0.0-beta.1",\n  "notes": "",\n  "platforms": {}\n}\n' >"$FIX/latest-beta.json"

PASS=0
FAIL=0
CASE=""

# new_case NAME: a fresh home, /Applications, temp folder and log.
new_case() {
  CASE="$ROOT/cases/$1"
  mkdir -p "$CASE/home" "$CASE/tmp" "$CASE/Applications" "$CASE/state"
  : >"$CASE/log"
  CASE_NAME=$1
}

# run [ENV=VALUE...] -- [installer options...]
run() {
  local envs=()
  while [ $# -gt 0 ] && [ "$1" != -- ]; do
    envs+=("$1")
    shift
  done
  shift
  env HOME="$CASE/home" TMPDIR="$CASE/tmp" \
    WAVEFLOW_SYSTEM_APPLICATIONS="$CASE/Applications" \
    FAKE_STATE="$CASE/state" FAKE_LOG="$CASE/log" FIX="$FIX" \
    PATH="$FAKEBIN:$PATH" ${envs[@]+"${envs[@]}"} \
    bash "$SCRIPT" "$@" >"$CASE/out" 2>"$CASE/err" </dev/null
  RC=$?
}

check() {
  local what=$1
  shift
  if "$@"; then
    PASS=$((PASS + 1))
  else
    FAIL=$((FAIL + 1))
    echo "FAIL [$CASE_NAME] $what"
    echo "  stdout: $(tr '\n' '|' <"$CASE/out")"
    echo "  stderr: $(tr '\n' '|' <"$CASE/err")"
  fi
}

installed_version() {
  awk '/CFBundleShortVersionString/ { getline; gsub(/.*<string>|<\/string>.*/, ""); print }' \
    "$1/WaveFlow.app/Contents/Info.plist" 2>/dev/null
}
tmp_empty() { [ -z "$(ls -A "$CASE/tmp")" ]; }
logged() { grep -q -- "$1" "$CASE/log"; }
not_logged() { ! grep -q -- "$1" "$CASE/log"; }
err_has() { grep -q -- "$1" "$CASE/err"; }
out_has() { grep -q -- "$1" "$CASE/out"; }
# Every xattr call names WaveFlow.app as its target, nothing else.
xattr_only_on_app() {
  grep '^xattr ' "$CASE/log" | awk '{ print $NF }' | grep -v '/WaveFlow\.app$' | grep -q . && return 1
  return 0
}
# Nothing in the folder but WaveFlow.app: no staging or previous copy.
no_leftovers() {
  local entry
  for entry in "$1"/* "$1"/.[!.]*; do
    [ -e "$entry" ] || continue
    [ "${entry##*/}" = WaveFlow.app ] || return 1
  done
  return 0
}

# --- the stable channel, into /Applications ---------------------------------
new_case stable
run -- --no-launch
check "exit 0" [ "$RC" = 0 ]
check "installs the stable version" [ "$(installed_version "$CASE/Applications")" = 1.9.8 ]
check "reads the stable manifest" logged "curl https://github.com/InstaZDLL/WaveFlow/releases/latest/download/latest.json"
check "verifies the checksum" out_has "Checksum matches"
check "verifies the signature" logged "codesign --verify --strict"
check "removes the quarantine flag" logged "xattr -dr com.apple.quarantine"
check "quarantine only on WaveFlow.app" xattr_only_on_app
check "explains the quarantine step" out_has "not notarized"
check "no sudo when writable" not_logged "^sudo"
check "does not launch with --no-launch" not_logged "^open"
check "temporary folder removed" tmp_empty
check "no staging copy left" no_leftovers "$CASE/Applications"

# --- launch, and the user's own Applications folder --------------------------
new_case user
run -- --user
check "exit 0" [ "$RC" = 0 ]
check "installs into ~/Applications" [ "$(installed_version "$CASE/home/Applications")" = 1.9.8 ]
check "leaves /Applications alone" [ ! -e "$CASE/Applications/WaveFlow.app" ]
check "launches the installed app" logged "open $CASE/home/Applications/WaveFlow.app"
check "temporary folder removed" tmp_empty

# --- --beta and --version ----------------------------------------------------
new_case beta
run -- --beta --no-launch
check "exit 0" [ "$RC" = 0 ]
check "installs the beta channel's version" [ "$(installed_version "$CASE/Applications")" = 2.0.0-beta.1 ]
check "reads the beta manifest" logged "latest-beta.json"

new_case version
run -- --version v1.9.5 --no-launch
check "exit 0" [ "$RC" = 0 ]
check "installs the version asked" [ "$(installed_version "$CASE/Applications")" = 1.9.5 ]
check "reads no manifest" not_logged "latest"

new_case version-equals
run -- --version=2.0.0-beta.1 --no-launch
check "exit 0" [ "$RC" = 0 ]
check "accepts --version=X" [ "$(installed_version "$CASE/Applications")" = 2.0.0-beta.1 ]

new_case manifest-down
run FAKE_MANIFEST_DOWN=1 -- --no-launch
check "refused" [ "$RC" != 0 ]
check "says why" err_has "Could not reach GitHub to find the latest stable version"
check "installs nothing" [ ! -e "$CASE/Applications/WaveFlow.app" ]

new_case beta-and-version
run -- --beta --version 1.9.5
check "refused" [ "$RC" != 0 ]
check "says why" err_has "cannot be used together"

new_case bad-version
run -- --version 1.9
check "refused" [ "$RC" != 0 ]
check "says why" err_has "Not a version"
check "downloads nothing" not_logged "^curl"

new_case missing-version
run -- --version 9.9.9 --no-launch
check "refused" [ "$RC" != 0 ]
check "says why" err_has "Could not download WaveFlow 9.9.9"
check "temporary folder removed after a failure" tmp_empty

# --- updates -----------------------------------------------------------------
new_case update
run -- --version 1.9.5 --no-launch
run -- --no-launch
check "exit 0" [ "$RC" = 0 ]
check "replaces the old version" [ "$(installed_version "$CASE/Applications")" = 1.9.8 ]
check "says what it replaced" out_has "WaveFlow 1.9.5 was replaced by 1.9.8"
check "no staging or previous copy left" no_leftovers "$CASE/Applications"

new_case downgrade
run -- --no-launch
run -- --version 1.9.5 --no-launch
check "exit 0" [ "$RC" = 0 ]
check "installs the older version" [ "$(installed_version "$CASE/Applications")" = 1.9.5 ]
check "warns about the downgrade" err_has "over the newer 1.9.8"

# --- platform refusals -------------------------------------------------------
new_case linux
run FAKE_UNAME=Linux -- --no-launch
check "refused" [ "$RC" != 0 ]
check "says it is for macOS" err_has "This installer is for macOS"
check "downloads nothing" not_logged "^curl"

new_case root
run FAKE_UID=0 -- --no-launch
check "refused" [ "$RC" != 0 ]
check "says why" err_has "Run the installer as yourself"
check "downloads nothing" not_logged "^curl"

new_case old-macos
run FAKE_MACOS=10.12.6 -- --no-launch
check "refused" [ "$RC" != 0 ]
check "names the minimum" err_has "needs macOS 10.13 or later; this Mac runs 10.12.6"
check "downloads nothing" not_logged "^curl"

new_case min-macos-ok
run FAKE_MACOS=10.13 -- --no-launch
check "10.13 itself is accepted" [ "$RC" = 0 ]

new_case bundle-needs-newer-macos
run FAKE_MACOS=14.5 -- --version 7.0.0 --no-launch
check "refused" [ "$RC" != 0 ]
check "names the bundle's minimum" err_has "WaveFlow 7.0.0 needs macOS 15.0 or later"
check "installs nothing" [ ! -e "$CASE/Applications/WaveFlow.app" ]
check "temporary folder removed after a failure" tmp_empty

# --- integrity -----------------------------------------------------------------
new_case bad-signature
run FAKE_CODESIGN_RC=1 -- --no-launch
check "refused" [ "$RC" != 0 ]
check "says why" err_has "code signature check"
check "installs nothing" [ ! -e "$CASE/Applications/WaveFlow.app" ]
check "temporary folder removed after a failure" tmp_empty

new_case wrong-identifier
run FAKE_CODESIGN_ID=com.example.other -- --no-launch
check "refused" [ "$RC" != 0 ]
check "says why" err_has "is not signed as app.waveflow"

new_case longer-identifier
run FAKE_CODESIGN_ID=app.waveflow.other -- --no-launch
check "refused" [ "$RC" != 0 ]
check "says why" err_has "is not signed as app.waveflow"

new_case linker-signed
run FAKE_LINKER_SIGNED=1 -- --no-launch
check "refused" [ "$RC" != 0 ]
check "says why" err_has "has no sealed resources"

new_case bad-digest
run FAKE_BAD_DIGEST=1 -- --no-launch
check "refused" [ "$RC" != 0 ]
check "says why" err_has "does not match the checksum"
check "installs nothing" [ ! -e "$CASE/Applications/WaveFlow.app" ]
check "temporary folder removed after a failure" tmp_empty

new_case api-down
run FAKE_API_DOWN=1 -- --no-launch
check "still installs" [ "$RC" = 0 ]
check "warns that the checksum was not checked" err_has "Could not read the checksum"
check "still verifies the signature" logged "codesign --verify"

new_case no-digest-for-asset
run FAKE_NO_DIGEST=1 -- --no-launch
check "installs" [ "$RC" = 0 ]
check "does not borrow the next asset's digest" err_has "Could not read the checksum"

new_case stray-file
run -- --version 6.6.6 --no-launch
check "refused" [ "$RC" != 0 ]
check "says why" err_has "holds files outside WaveFlow.app"
check "installs nothing" [ ! -e "$CASE/Applications/WaveFlow.app" ]
check "temporary folder removed after a failure" tmp_empty

new_case dotdot
if [ "$DOTDOT" = 1 ]; then
  run -- --version 6.6.7 --no-launch
  check "refused" [ "$RC" != 0 ]
  check "says why" err_has "holds files outside WaveFlow.app"
  check "nothing escaped the temporary folder" [ ! -e "$CASE/evil.sh" ]
  check "temporary folder removed after a failure" tmp_empty
else
  echo "skip [dotdot]: this tar cannot write a .. entry"
fi

new_case hardlink
if [ "$HARDLINK" = 1 ]; then
  run -- --version 6.6.8 --no-launch
  check "refused" [ "$RC" != 0 ]
  check "says why" err_has "holds links or special files"
  check "nothing installed" [ ! -e "$CASE/Applications/WaveFlow.app" ]
  check "temporary folder removed after a failure" tmp_empty
else
  echo "skip [hardlink]: this file system or tar cannot write a hard link entry"
fi

# --- quarantine opt-out --------------------------------------------------------
new_case no-unquarantine
run -- --no-unquarantine --no-launch
check "exit 0" [ "$RC" = 0 ]
check "does not touch the quarantine flag" not_logged "^xattr"

# --- WaveFlow running ------------------------------------------------------------
# runs_from DIR: the fake process list shows WaveFlow started from DIR.
runs_from() { (cd "$1" && pwd -P) >"$CASE/state/running"; }

new_case running
runs_from "$CASE/Applications"
run -- --no-launch
check "refused" [ "$RC" != 0 ]
check "asks to quit it" err_has "WaveFlow is running"
check "downloads nothing" not_logged "^curl"
check "does not quit it" not_logged "^osascript"

new_case running-force
runs_from "$CASE/Applications"
run -- --force --no-launch
check "exit 0" [ "$RC" = 0 ]
check "asks WaveFlow to quit" logged "osascript -e tell application id \"app.waveflow\" to quit"
check "installs" [ "$(installed_version "$CASE/Applications")" = 1.9.8 ]

new_case running-elsewhere
mkdir -p "$CASE/elsewhere"
runs_from "$CASE/elsewhere"
run -- --dir "$CASE/Applications"
check "a copy running elsewhere does not block" [ "$RC" = 0 ]
check "does not quit it" not_logged "^osascript"
check "installs" [ "$(installed_version "$CASE/Applications")" = 1.9.8 ]
check "does not open the new copy over the running one" not_logged "^open"
check "says why" out_has "already running from another copy"

# --- an explicit folder ----------------------------------------------------------
new_case dir
mkdir -p "$CASE/elsewhere"
run -- --dir "$CASE/elsewhere/" --no-launch
check "exit 0" [ "$RC" = 0 ]
check "installs into the folder" [ "$(installed_version "$CASE/elsewhere")" = 1.9.8 ]
check "leaves /Applications alone" [ ! -e "$CASE/Applications/WaveFlow.app" ]
check "leaves ~/Applications alone" [ ! -e "$CASE/home/Applications" ]
run -- --version 1.9.5 --no-launch
: >"$CASE/log"
run -- --dir="$CASE/elsewhere" --uninstall
check "uninstall exit 0" [ "$RC" = 0 ]
check "removes the folder's copy" [ ! -e "$CASE/elsewhere/WaveFlow.app" ]
check "keeps the copy in /Applications" [ "$(installed_version "$CASE/Applications")" = 1.9.5 ]

new_case dir-missing
run -- --dir "$CASE/nowhere" --no-launch
check "refused" [ "$RC" != 0 ]
check "says why" err_has "is not a folder"
check "creates nothing" [ ! -e "$CASE/nowhere" ]
check "downloads nothing" not_logged "^curl"

new_case dir-and-user
run -- --dir "$CASE/home" --user
check "refused" [ "$RC" != 0 ]
check "says why" err_has "cannot be used together"

# --- /Applications not writable: sudo for the copy only --------------------------
new_case sudo
chmod u-w "$CASE/Applications"
if [ -w "$CASE/Applications" ]; then
  echo "skip [sudo]: this file system ignores chmod (or the bench runs as root)"
  chmod u+w "$CASE/Applications"
else
  run -- --no-launch
  chmod u+w "$CASE/Applications"
  check "exit 0" [ "$RC" = 0 ]
  check "installs" [ "$(installed_version "$CASE/Applications")" = 1.9.8 ]
  check "copies through sudo" logged "^sudo ditto"
  check "hands the copy to the user" logged "^sudo chown -R"
  check "downloads without sudo" not_logged "^sudo curl"
  check "verifies without sudo" not_logged "^sudo codesign"
  check "clears quarantine without sudo" not_logged "^sudo xattr"
  check "says why it needs sudo" out_has "Administrator rights are needed"
fi

# --- uninstall -------------------------------------------------------------------
new_case uninstall
run -- --no-launch
mkdir -p "$CASE/home/Library/Application Support/app.waveflow"
: >"$CASE/log"
run -- --uninstall
check "exit 0" [ "$RC" = 0 ]
check "removes the app" [ ! -e "$CASE/Applications/WaveFlow.app" ]
check "keeps the data" [ -d "$CASE/home/Library/Application Support/app.waveflow" ]
check "says where the data is" out_has "Library/Application Support/app.waveflow"
check "downloads nothing" not_logged "^curl"

new_case uninstall-running
run -- --no-launch
runs_from "$CASE/Applications"
run -- --uninstall
check "refused while running" [ "$RC" != 0 ]
check "keeps the app" [ -e "$CASE/Applications/WaveFlow.app" ]

new_case uninstall-user
run -- --no-launch
run -- --user --no-launch
run -- --user --uninstall
check "exit 0" [ "$RC" = 0 ]
check "removes ~/Applications' copy" [ ! -e "$CASE/home/Applications/WaveFlow.app" ]
check "keeps /Applications' copy" [ -e "$CASE/Applications/WaveFlow.app" ]

# --- help and unknown options ----------------------------------------------------
new_case help
run -- --help
check "exit 0" [ "$RC" = 0 ]
check "documents --uninstall" out_has "--uninstall"

new_case unknown
run -- --frobnicate
check "refused" [ "$RC" != 0 ]
check "says why" err_has "Unknown option"

# --- one real download (--live) ----------------------------------------------------
if [ "${1:-}" = --live ]; then
  mkdir -p "$ROOT/livebin"
  for tool in "$FAKEBIN"/*; do
    [ "${tool##*/}" = curl ] || cp "$tool" "$ROOT/livebin/"
  done
  new_case live
  saved=$FAKEBIN
  FAKEBIN="$ROOT/livebin"
  run -- --no-launch
  FAKEBIN=$saved
  live_version=$(installed_version "$CASE/Applications")
  check "exit 0" [ "$RC" = 0 ]
  check "installs a release" [ -n "$live_version" ]
  check "the digest GitHub published matches" out_has "Checksum matches"
  check "the bundle has its executable" [ -f "$CASE/Applications/WaveFlow.app/Contents/MacOS/waveflow" ]
  check "the bundle has its seal" [ -f "$CASE/Applications/WaveFlow.app/Contents/_CodeSignature/CodeResources" ]
  check "temporary folder removed" tmp_empty
  echo "live: installed WaveFlow $live_version from GitHub"
fi

echo "install-macos: $PASS passed, $FAIL failed"
[ "$FAIL" = 0 ]
