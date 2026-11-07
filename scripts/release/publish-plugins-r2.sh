#!/usr/bin/env bash
# Mirror the plugin store onto Cloudflare R2, the store's second source.
#
#   publish-plugins-r2.sh --check     prints "configured" or "absent"
#   publish-plugins-r2.sh [--dry-run] stages, verifies, uploads
#
# mirror-plugins.py reads the merged registry on waveflow.app (the only
# copy that lists the private plugins), downloads every archive it names
# and checks each one as the app would before installing it; it stages
# nothing unless all of them pass. This script then fills the bucket:
#
#   plugins/<id>/<id>-v<version>.zip   every archive the registry names
#   plugins/registry.json              the registry, byte for byte
#
# The app falls back to both (commands/plugin_store.rs): the registry
# right after waveflow.app's, an archive when its own address fails.
#
# The registry goes up last, and only once every archive answers on the
# public domain with the staged size: it is what makes a client
# download, so it never names something that is not there yet.
#
# Nothing is ever deleted: an archive a newer registry no longer names
# stays, so an installation that read the previous registry can still
# fetch what it pinned.
#
# Environment:
#   R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY, R2_ACCOUNT_ID, R2_BUCKET
#                     as publish-r2.sh (r2-common.sh)
#   R2_PUBLIC_URL     the bucket's public domain, as compiled into the
#                     app (default https://download.waveflow.app)
#   PLUGIN_REGISTRY_URL   the registry to copy (default waveflow.app's)
#   ALLOW_REMOVAL=1   publish a registry that drops plugins the mirror
#                     lists — a withdrawal; refused otherwise
#   R2_DRY_RUN=1      the same as --dry-run: download and verify for
#                     real, upload nothing, print every upload
#   PYTHON            the interpreter (default python3), which needs the
#                     blake3 package
#
# None of the four R2_* secrets set means no mirror: the script says so
# and succeeds, so a fork still runs green. Some but not all is an error.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR source=r2-common.sh
. "$here/r2-common.sh"

if [ "${1:-}" = "--check" ]; then
  r2_check
  exit
fi
if [ "${1:-}" = "--dry-run" ]; then
  export R2_DRY_RUN=1
fi
dry_run="${R2_DRY_RUN:-0}"
python="${PYTHON:-python3}"

state="$(r2_state)"
case "$state" in
  partial)
    echo "::error::$R2_HALF_CONFIGURED" >&2
    exit 1
    ;;
  absent)
    if [ "$dry_run" != "1" ]; then
      echo "No R2_* secret set: the R2 mirror is not configured, nothing to publish."
      exit 0
    fi
    ;;
esac

public="${R2_PUBLIC_URL:-https://download.waveflow.app}"
public="${public%/}"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

stage_args=(--out "$work/stage" --published "$public/plugins/registry.json")
if [ -n "${PLUGIN_REGISTRY_URL:-}" ]; then
  stage_args+=(--registry-url "$PLUGIN_REGISTRY_URL")
fi
if [ "${ALLOW_REMOVAL:-0}" = "1" ]; then
  stage_args+=(--allow-removal)
fi
# Without the bucket's keys: the staging downloads from addresses the
# registry names and needs none of them.
env -u R2_ACCESS_KEY_ID -u R2_SECRET_ACCESS_KEY -u R2_ACCOUNT_ID -u R2_BUCKET \
  "$python" "$here/mirror-plugins.py" "${stage_args[@]}"

r2_aws_env "$state"

# The archives, sorted so a log reads the same from one run to the next.
# Five minutes at the edge, as a release's payloads: a version is not
# meant to be republished with other bytes, but if it ever is, a stale
# copy would not match the registry's blake3 for long.
archives="$(cd "$work/stage" && find plugins -mindepth 2 -type f -name '*.zip' | LC_ALL=C sort)"
if [ -z "$archives" ]; then
  echo "::error::nothing staged under plugins/" >&2
  exit 1
fi
count=0
while IFS= read -r key; do
  r2_run aws s3 cp "$work/stage/$key" "s3://$R2_BUCKET/$key" \
    --endpoint-url "$R2_ENDPOINT" \
    --content-type application/zip \
    --cache-control "public, max-age=300" \
    --only-show-errors
  count=$((count + 1))
done <<<"$archives"
if [ "$dry_run" != "1" ]; then echo "Uploaded $count archives under plugins/."; fi

if [ "$dry_run" != "1" ]; then
  # Through the public domain, as the app will ask: the S3 API accepting
  # a file says nothing about the custom domain serving it. The query
  # string is a cache key the edge has not seen.
  while IFS= read -r key; do
    expected="$(wc -c <"$work/stage/$key" | tr -d ' ')"
    got="$(curl -fsSIL "$public/$key?v=$RANDOM$RANDOM" | tr -d '\r' |
      awk -F': ' 'tolower($1) == "content-length" { n = $2 } END { print n }')"
    if [ "$got" != "$expected" ]; then
      echo "::error::$public/$key answers with ${got:-no} bytes, the archive has $expected; the registry stays unpublished" >&2
      exit 1
    fi
    echo "verified $public/$key ($got bytes)"
  done <<<"$archives"
fi

r2_run aws s3 cp "$work/stage/plugins/registry.json" "s3://$R2_BUCKET/plugins/registry.json" \
  --endpoint-url "$R2_ENDPOINT" \
  --content-type application/json \
  --cache-control "no-cache" \
  --only-show-errors

if [ "$dry_run" = "1" ]; then
  echo "Dry run: nothing uploaded; the registry would be $public/plugins/registry.json"
  exit 0
fi

# Read back, compared byte for byte with what was staged.
curl -fsSL "$public/plugins/registry.json?v=$RANDOM$RANDOM" -o "$work/served.json"
if ! cmp -s "$work/served.json" "$work/stage/plugins/registry.json"; then
  echo "::error::$public/plugins/registry.json does not serve the registry just uploaded" >&2
  exit 1
fi
echo "Published the plugin registry on $public/plugins/registry.json"
