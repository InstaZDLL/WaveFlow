#!/usr/bin/env bash
# Upload an APT tree built by build-apt-repo.sh to the R2 bucket.
#
#   publish-apt-r2.sh [--dry-run] <tree-dir> <prefix>
#
# <prefix> is the folder in the bucket the repository is served from
# (apt, or apt-test for a rehearsal), reached as
# https://download.waveflow.app/<prefix>/. Nothing outside that prefix is
# ever written or deleted.
#
# Order matters for a client that fetches while we write: an index never
# names a file that is not there yet, and the signed entry point goes last.
#
#   1. pool/                         immutable: a version's bytes never change
#   2. waveflow.gpg / waveflow.asc
#   3. Packages, Packages.gz
#   4. Release, Release.gpg
#   5. InRelease                     the file apt reads first
#   6. pool/ files no longer listed  only after InRelease no longer names them
#
# Environment: R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY, R2_ACCOUNT_ID,
# R2_BUCKET (r2-common.sh), R2_DRY_RUN=1 as --dry-run.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source-path=SCRIPTDIR source=r2-common.sh
. "$here/r2-common.sh"

if [ "${1:-}" = "--dry-run" ]; then
  export R2_DRY_RUN=1
  shift
fi
tree="${1:?usage: publish-apt-r2.sh [--dry-run] <tree-dir> <prefix>}"
prefix="${2:?usage: publish-apt-r2.sh [--dry-run] <tree-dir> <prefix>}"

# Only the two folders an APT repository lives in: this script must not
# be able to reach releases/, stable/, plugins/ or anything else.
case "$prefix" in
  apt | apt-[a-z0-9-]*) ;;
  *)
    echo "::error::prefix '$prefix' is not apt or apt-<name>" >&2
    exit 1
    ;;
esac

state="$(r2_state)"
if [ "$state" != "configured" ] && [ "${R2_DRY_RUN:-0}" != "1" ]; then
  echo "::error::R2 is $state: all four R2_* secrets are needed to publish the APT repository" >&2
  exit 1
fi
r2_aws_env "$state"
endpoint="$R2_ENDPOINT"
bucket="$R2_BUCKET"

# one <file> <content-type> <cache-control>
one() {
  r2_run aws s3 cp "$tree/$1" "s3://$bucket/$prefix/$1" \
    --endpoint-url "$endpoint" \
    --content-type "$2" \
    --cache-control "$3" \
    --only-show-errors
}

immutable="public, max-age=31536000, immutable"
short="public, max-age=60"

for f in "$tree"/pool/*/*/*/*.deb; do
  one "${f#"$tree"/}" application/vnd.debian.binary-package "$immutable"
done
one waveflow.gpg application/octet-stream "$short"
one waveflow.asc text/plain "$short"
one dists/stable/main/binary-amd64/Packages text/plain "$short"
one dists/stable/main/binary-amd64/Packages.gz application/gzip "$short"
one dists/stable/Release text/plain "$short"
one dists/stable/Release.gpg application/pgp-signature "$short"
one dists/stable/InRelease text/plain "$short"
echo "Uploaded the repository to $prefix/."

# Pool files the new index no longer lists. In a dry run nothing was
# uploaded, so there is nothing to compare against: skip.
if [ "${R2_DRY_RUN:-0}" != "1" ]; then
  listed="$(cd "$tree" && find pool -name '*.deb' | sort)"
  remote="$(aws s3 ls "s3://$bucket/$prefix/pool/" --recursive --endpoint-url "$endpoint" |
    awk '{ print $4 }' | sed "s#^$prefix/##" | grep '\.deb$' | sort || true)"
  stale="$(comm -13 <(printf '%s\n' "$listed") <(printf '%s\n' "$remote"))"
  while IFS= read -r key; do
    [ -n "$key" ] || continue
    echo "Removing stale $key"
    aws s3 rm "s3://$bucket/$prefix/$key" --endpoint-url "$endpoint" --only-show-errors
  done <<<"$stale"
fi
