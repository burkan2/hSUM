#!/usr/bin/env bash
# Stage a pinned GitHub CLI with source-bound attestation verification flags.
set -euo pipefail
if [ "$#" -ne 1 ] || [[ "$1" != /* ]] || [ -e "$1" ]; then
  echo "usage: $0 /absolute/path/to/absent-directory" >&2
  exit 2
fi
destination=$1
version=2.103.3
case "$(uname -s):$(uname -m)" in
  Linux:x86_64)
    asset="gh_${version}_linux_amd64.tar.gz"
    digest=4c806978ffce1cec70853302a56d70ac6da4f19bae7adca4e1a8ab00633dd91d
    ;;
  Darwin:arm64)
    asset="gh_${version}_macOS_arm64.zip"
    digest=d700207eae9ae98fe2b89535c2c087c07823d4628c56364695a5a07ef311c062
    ;;
  *) echo "unsupported release tool target" >&2; exit 2 ;;
esac
temporary=$(mktemp -d "${TMPDIR:-/tmp}/hsum-release-gh.XXXXXX")
trap 'rm -rf "$temporary"' EXIT
curl --fail --location --silent --show-error --proto '=https' --tlsv1.2 \
  "https://github.com/cli/cli/releases/download/v${version}/${asset}" --output "$temporary/$asset"
(cd "$temporary" && printf '%s  %s\n' "$digest" "$asset" | shasum -a 256 -c -)
mkdir -m 700 "$destination"
case "$asset" in
  *.zip) unzip -q "$temporary/$asset" -d "$temporary/unpacked" ;;
  *) mkdir "$temporary/unpacked"; tar -xzf "$temporary/$asset" -C "$temporary/unpacked" ;;
esac
cp "$temporary/unpacked/gh_${version}_"*/bin/gh "$destination/gh"
chmod 700 "$destination/gh"
"$destination/gh" --version
