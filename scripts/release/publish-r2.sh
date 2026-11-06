#!/usr/bin/env bash
# Mirror a release onto Cloudflare R2, the updater's second source.
#
#   publish-r2.sh --check                    prints "configured" or "absent"
#   publish-r2.sh <assets dir> <latest.json> uploads, verifies, publishes
#
# <assets dir> holds the release's files under their published names
# (what prepare-assets.py put in dist-release/, or a `gh release
# download` of the tag); <latest.json> is the merged manifest, wherever
# its URLs point. The bucket ends up as:
#
#   releases/<tag>/<every asset>     the installers and updater payloads
#   stable/latest.json               a stable tag's manifest, or
#   beta/latest.json                 a pre-release tag's
#
# with the manifest's URLs rewritten to the copies under releases/<tag>/
# (mirror-manifest.py). The signatures stay the bytes the build made, so
# the app verifies an update from either host under the one public key.
#
# The manifest goes up last, and only once every payload it names
# answers on the public domain with the size of the local file: a
# manifest is what makes a client download, so it never points at
# something that is not there yet.
#
# No beta copy of a stable manifest, unlike release.yml on GitHub: every
# build that reads R2 reads stable/latest.json on the beta channel too
# (commands/updater.rs), and builds that predate R2 never read it.
#
# Environment:
#   R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY  an R2 API token's S3 keys
#   R2_ACCOUNT_ID                          the Cloudflare account id
#   R2_BUCKET                              the bucket's name
#   R2_PUBLIC_URL     the bucket's public domain, as compiled into the
#                     app (default https://download.waveflow.app)
#   TAG_NAME, IS_PRERELEASE                as the release workflows set
#   R2_DRY_RUN=1      touch no network: print the uploads, write the
#                     manifest to R2_DRY_RUN_OUT (default r2-latest.json)
#   R2_FORCE=1        replace the channel's manifest even when the one
#                     published is newer (a deliberate rollback)
#
# None of the four R2_* secrets set means no mirror: the script says so
# and succeeds, so a fork or a repository without them still releases.
# Some but not all of them set is an error — half a configuration is a
# mistake, not a choice.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

r2_state() {
  local set=0
  for v in R2_ACCESS_KEY_ID R2_SECRET_ACCESS_KEY R2_ACCOUNT_ID R2_BUCKET; do
    [ -n "${!v:-}" ] && set=$((set + 1))
  done
  case "$set" in
    0) echo absent ;;
    4) echo configured ;;
    *) echo partial ;;
  esac
}

state="$(r2_state)"
if [ "${1:-}" = "--check" ]; then
  if [ "$state" = "partial" ]; then
    echo "::error::R2 mirror half configured: set all four of R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY, R2_ACCOUNT_ID, R2_BUCKET, or none" >&2
    exit 1
  fi
  echo "$state"
  exit 0
fi

assets="${1:?usage: publish-r2.sh <assets dir> <latest.json>}"
manifest="${2:?usage: publish-r2.sh <assets dir> <latest.json>}"
dry_run="${R2_DRY_RUN:-0}"
: "${TAG_NAME:?TAG_NAME is required}"
: "${IS_PRERELEASE:?IS_PRERELEASE is required}"

case "$state" in
  partial)
    echo "::error::R2 mirror half configured: set all four of R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY, R2_ACCOUNT_ID, R2_BUCKET, or none" >&2
    exit 1
    ;;
  absent)
    if [ "$dry_run" != "1" ]; then
      echo "No R2_* secret set: the R2 mirror is not configured, nothing to publish."
      exit 0
    fi
    # A dry run needs names to print, not credentials.
    R2_ACCOUNT_ID="<account-id>"
    R2_BUCKET="<bucket>"
    ;;
esac

public="${R2_PUBLIC_URL:-https://download.waveflow.app}"
public="${public%/}"
prefix="releases/$TAG_NAME"
if [ "$IS_PRERELEASE" = "true" ]; then channel=beta; else channel=stable; fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

python3 "$here/mirror-manifest.py" \
  --manifest "$manifest" --assets "$assets" \
  --base-url "$public/$prefix" --out "$work/latest.json"
version="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["version"])' "$work/latest.json")"
if [ "v$version" != "$TAG_NAME" ]; then
  echo "::error::the manifest is for $version, the tag is $TAG_NAME" >&2
  exit 1
fi

export AWS_ACCESS_KEY_ID="${R2_ACCESS_KEY_ID:-}"
export AWS_SECRET_ACCESS_KEY="${R2_SECRET_ACCESS_KEY:-}"
export AWS_DEFAULT_REGION=auto
# AWS CLI 2.23 started sending CRC checksums R2 does not take on every
# request; Cloudflare's guidance is to send them only where required.
export AWS_REQUEST_CHECKSUM_CALCULATION=when_required
export AWS_RESPONSE_CHECKSUM_VALIDATION=when_required
endpoint="https://$R2_ACCOUNT_ID.r2.cloudflarestorage.com"

run() {
  if [ "$dry_run" = "1" ]; then
    printf '[dry-run]'
    printf ' %q' "$@"
    printf '\n'
  else
    "$@"
  fi
}

# Five minutes at the edge, not a year: a re-run of the release workflow
# rebuilds the payloads under the same names, and a cached old copy
# would no longer match the new manifest's signature.
uploaded=0
for f in "$assets"/*; do
  [ -f "$f" ] || continue
  name="$(basename "$f")"
  case "$name" in
    latest*.json) continue ;; # the manifests go up rewritten, below
  esac
  run aws s3 cp "$f" "s3://$R2_BUCKET/$prefix/$name" \
    --endpoint-url "$endpoint" \
    --cache-control "public, max-age=300" \
    --only-show-errors
  uploaded=$((uploaded + 1))
done
if [ "$uploaded" -eq 0 ]; then
  echo "::error::no asset in $assets" >&2
  exit 1
fi
echo "Uploaded $uploaded files to $prefix/."

if [ "$dry_run" != "1" ]; then
  # Through the public domain, as a client will ask: the S3 API
  # answering says nothing about the custom domain serving the file.
  # The query string is a cache key the edge has not seen. The list is
  # read into a variable first, so a failure to build it stops the
  # script instead of leaving the loop with nothing to check.
  targets="$(python3 -c '
import json, sys
seen = set()
for entry in json.load(open(sys.argv[1]))["platforms"].values():
    url = entry["url"]
    if url not in seen:
        seen.add(url)
        print(url, url.rsplit("/", 1)[1], sep="\t")
' "$work/latest.json" | tr -d '\r')" # a Windows Python ends its lines with CRLF
  if [ -z "$targets" ]; then
    echo "::error::the manifest names no payload to verify" >&2
    exit 1
  fi
  while IFS=$'\t' read -r url name; do
    expected="$(wc -c <"$assets/$name" | tr -d ' ')"
    got="$(curl -fsSIL "$url?v=$RANDOM$RANDOM" | tr -d '\r' |
      awk -F': ' 'tolower($1) == "content-length" { n = $2 } END { print n }')"
    if [ "$got" != "$expected" ]; then
      echo "::error::$url answers with ${got:-no} bytes, the payload has $expected; the manifest stays unpublished" >&2
      exit 1
    fi
    echo "verified $url ($got bytes)"
  done <<<"$targets"
fi

# Never move a channel backwards. A patch release from a maintenance
# branch (1.9.9 after 2.0.1), or the workflow re-run on an old tag, would
# otherwise put an older manifest in place, and the app offers only what
# is newer than itself: every 2.0.1 installation would stop seeing
# updates from this host. The same version again is a re-run, and goes
# through. R2_FORCE=1 overrides, for a deliberate rollback.
newer() {
  python3 - "$1" "$2" <<'PY'
import sys

def key(version):
    core, _, pre = version.partition("-")
    numbers = tuple(int(part) for part in core.split("."))
    # A release sorts after every pre-release of the same core;
    # pre-release identifiers compare numerically where they can.
    parts = tuple(
        (0, int(p), "") if p.isdigit() else (1, 0, p)
        for p in pre.split(".")
    ) if pre else ()
    return numbers, (1,) if not pre else (0,) + parts

sys.exit(0 if key(sys.argv[1]) > key(sys.argv[2]) else 1)
PY
}
if [ "$dry_run" != "1" ] && [ "${R2_FORCE:-0}" != "1" ]; then
  code="$(curl -sSL -o "$work/current.json" -w '%{http_code}' \
    "$public/$channel/latest.json?v=$RANDOM$RANDOM" || true)"
  case "$code" in
    200)
      if ! published="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["version"])' "$work/current.json")"; then
        echo "::error::$public/$channel/latest.json is not a manifest; set R2_FORCE=1 to replace it" >&2
        exit 1
      fi
      if newer "$published" "$version"; then
        echo "::warning::$channel/latest.json already offers $published, newer than $version: left in place (the files stay under $prefix/)"
        exit 0
      fi
      ;;
    404) ;; # the channel's first manifest
    *)
      echo "::error::could not read $public/$channel/latest.json (HTTP ${code:-none}); set R2_FORCE=1 to publish without the check" >&2
      exit 1
      ;;
  esac
fi

run aws s3 cp "$work/latest.json" "s3://$R2_BUCKET/$channel/latest.json" \
  --endpoint-url "$endpoint" \
  --content-type application/json \
  --cache-control "no-cache" \
  --only-show-errors

if [ "$dry_run" = "1" ]; then
  out="${R2_DRY_RUN_OUT:-r2-latest.json}"
  cp "$work/latest.json" "$out"
  echo "Dry run: the $channel manifest would be $public/$channel/latest.json, written to $out"
  exit 0
fi

served="$(curl -fsSL "$public/$channel/latest.json?v=$RANDOM$RANDOM" |
  python3 -c 'import json, sys; print(json.load(sys.stdin)["version"])')"
if [ "$served" != "$version" ]; then
  echo "::error::$public/$channel/latest.json serves $served after publishing $version" >&2
  exit 1
fi
echo "Published $version on $public/$channel/latest.json"
