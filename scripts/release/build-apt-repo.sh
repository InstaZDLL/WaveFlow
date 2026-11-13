#!/usr/bin/env bash
# Build the static, signed APT repository from a folder of release .debs.
#
#   build-apt-repo.sh <debs-dir> <out-dir>
#
# <debs-dir> holds the Tauri bundles as the releases name them
# (WaveFlow_<version>_linux-x86_64.deb). <out-dir> receives the tree
# below, ready to be served as-is from any static host (the R2 bucket
# today; a plain web server or an S3-compatible bucket later):
#
#   waveflow.gpg                               public key, binary (signed-by=)
#   waveflow.asc                               the same key, armored
#   dists/stable/Release | Release.gpg | InRelease
#   dists/stable/main/binary-amd64/Packages | Packages.gz
#   pool/main/w/wave-flow/wave-flow_<version>_amd64.deb
#
# Nothing here knows where the tree goes: uploading is the caller's job.
#
# Environment:
#   APT_GPG_KEY_FPR   full fingerprint (40 hex) the signing key must have;
#                     a different key aborts before anything is signed
#   APT_GPG_PASSPHRASE_FILE  path of a file holding the key's passphrase;
#                     the passphrase never travels on a command line
#   GNUPGHOME         keyring holding the secret key (already imported)
#
# Needs: dpkg-deb, apt-ftparchive (apt-utils), gpg, gzip.
set -euo pipefail

debs="${1:?usage: build-apt-repo.sh <debs-dir> <out-dir>}"
out="${2:?usage: build-apt-repo.sh <debs-dir> <out-dir>}"
fpr="${APT_GPG_KEY_FPR:?APT_GPG_KEY_FPR is not set}"
pass_file="${APT_GPG_PASSPHRASE_FILE:?APT_GPG_PASSPHRASE_FILE is not set}"

package="wave-flow"
suite="stable"
component="main"
arch="amd64"

rm -rf "$out"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
pool="$out/pool/$component/w/$package"
dist="$out/dists/$suite"
bin="$dist/$component/binary-$arch"
mkdir -p "$pool" "$bin"

shopt -s nullglob
count=0
for f in "$debs"/WaveFlow_*_linux-x86_64.deb; do
  name="$(dpkg-deb -f "$f" Package)"
  version="$(dpkg-deb -f "$f" Version)"
  farch="$(dpkg-deb -f "$f" Architecture)"
  if [ "$name" != "$package" ] || [ "$farch" != "$arch" ]; then
    echo "::error::$(basename "$f") is $name/$farch, expected $package/$arch" >&2
    exit 1
  fi
  # A colon (an epoch) is not allowed in a file name of the pool.
  cp "$f" "$pool/${package}_${version#*:}_${arch}.deb"
  count=$((count + 1))
done
if [ "$count" -eq 0 ]; then
  echo "::error::no .deb in $debs" >&2
  exit 1
fi
echo "Pool: $count package(s)"

# Paths in Packages are relative to the repository root, so the
# generator runs from there.
(
  cd "$out"
  apt-ftparchive packages "pool/$component" >"dists/$suite/$component/binary-$arch/Packages"
  gzip -9nk "dists/$suite/$component/binary-$arch/Packages"
)
if ! grep -q "^Package: $package\$" "$bin/Packages"; then
  echo "::error::Packages does not list $package" >&2
  exit 1
fi

# Valid-Until is left out on purpose: the repository is rebuilt on a
# release, not on a clock, so an expiry would turn a quiet month into
# "Release file expired" for every user.
(
  cd "$dist"
  apt-ftparchive \
    -o "APT::FTPArchive::Release::Origin=WaveFlow" \
    -o "APT::FTPArchive::Release::Label=WaveFlow" \
    -o "APT::FTPArchive::Release::Suite=$suite" \
    -o "APT::FTPArchive::Release::Codename=$suite" \
    -o "APT::FTPArchive::Release::Architectures=$arch" \
    -o "APT::FTPArchive::Release::Components=$component" \
    -o "APT::FTPArchive::Release::Description=WaveFlow APT repository" \
    release . >"$out/Release.tmp"
)
# Written outside the suite folder so the generator cannot hash it.
mv "$out/Release.tmp" "$dist/Release"
# Older apt-ftparchive builds stamp no Date; add it (UTC, the format apt parses).
grep -q '^Date:' "$dist/Release" ||
  sed -i "1i Date: $(LC_ALL=C date -u '+%a, %d %b %Y %H:%M:%S UTC')" "$dist/Release"

# Public key, in both forms.
gpg --batch --export "$fpr" >"$out/waveflow.gpg"
gpg --batch --armor --export "$fpr" >"$out/waveflow.asc"

sign=(gpg --batch --yes --pinentry-mode loopback --passphrase-file "$pass_file" --local-user "$fpr")
"${sign[@]}" --clearsign --output "$dist/InRelease" "$dist/Release"
"${sign[@]}" --detach-sign --armor --output "$dist/Release.gpg" "$dist/Release"

# What a client does: verify both signatures against the exported key.
verify="$(mktemp -d)"
trap 'rm -rf "$verify"' EXIT
gpg --batch --no-default-keyring --keyring "$verify/k.gpg" --import "$out/waveflow.gpg" 2>/dev/null
gpgv --keyring "$verify/k.gpg" "$dist/Release.gpg" "$dist/Release"
gpgv --keyring "$verify/k.gpg" "$dist/InRelease"
echo "Built $out (signed by $fpr)"
