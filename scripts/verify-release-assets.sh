#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: verify-release-assets.sh TAG DIST_DIR" >&2
  exit 2
fi

tag=$1
dist_dir=$2

if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+-alpha\.[0-9]+$ ]]; then
  echo "release tag must be an exact vX.Y.Z-alpha.N version" >&2
  exit 2
fi
test -d "$dist_dir" || { echo "release dist directory is missing: $dist_dir" >&2; exit 1; }

expected_assets=$(mktemp)
actual_assets=$(mktemp)
expected_checksums=$(mktemp)
actual_checksums=$(mktemp)
trap 'rm -f "$expected_assets" "$actual_assets" "$expected_checksums" "$actual_checksums"' EXIT

cat > "$expected_assets" <<EOF
SHA256SUMS
hsum-$tag-aarch64-apple-darwin.spdx.json
hsum-$tag-aarch64-apple-darwin.zip
hsum-$tag-aarch64-apple-darwin.zip.sha256
hsum-$tag-cargo-licenses.json
hsum-$tag-x86_64-unknown-linux-gnu.spdx.json
hsum-$tag-x86_64-unknown-linux-gnu.tar.gz
hsum-$tag-x86_64-unknown-linux-gnu.tar.gz.sha256
install-hsum.sh
EOF

cat > "$expected_checksums" <<EOF
hsum-$tag-aarch64-apple-darwin.spdx.json
hsum-$tag-aarch64-apple-darwin.zip
hsum-$tag-cargo-licenses.json
hsum-$tag-x86_64-unknown-linux-gnu.spdx.json
hsum-$tag-x86_64-unknown-linux-gnu.tar.gz
install-hsum.sh
EOF

find "$dist_dir" -maxdepth 1 -type f -exec basename {} \; | LC_ALL=C sort > "$actual_assets"

if ! cmp -s "$expected_assets" "$actual_assets"; then
  echo "release assets do not match the immutable asset contract" >&2
  diff -u "$expected_assets" "$actual_assets" >&2 || true
  exit 1
fi

awk 'NF == 2 { print $2 }' "$dist_dir/SHA256SUMS" | LC_ALL=C sort > "$actual_checksums"
if ! cmp -s "$expected_checksums" "$actual_checksums"; then
  echo "SHA256SUMS does not cover the complete checksummed asset contract" >&2
  diff -u "$expected_checksums" "$actual_checksums" >&2 || true
  exit 1
fi

(
  cd "$dist_dir"
  for archive in \
    "hsum-$tag-aarch64-apple-darwin.zip" \
    "hsum-$tag-x86_64-unknown-linux-gnu.tar.gz"
  do
    sidecar="$archive.sha256"
    if ! awk -v expected="$archive" '
      NR == 1 {
        if (NF != 2 || length($1) != 64 || $1 ~ /[^0-9A-Fa-f]/ || $2 != expected) {
          exit 1
        }
        next
      }
      { exit 1 }
      END { if (NR != 1) exit 1 }
    ' "$sidecar"; then
      echo "$sidecar must contain exactly one SHA-256 for $archive" >&2
      exit 1
    fi
    shasum -a 256 -c "$sidecar"
  done
  shasum -a 256 -c SHA256SUMS
)
