#!/usr/bin/env bash
# Rebuild the release binary in an isolated target directory and require
# byte-for-byte equality with the candidate that will be packaged.
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 /absolute/path/to/candidate-hsum" >&2
  exit 2
fi

candidate="$1"
if [ ! -x "$candidate" ]; then
  echo "candidate hsum binary is not executable: $candidate" >&2
  exit 2
fi

repository_root=$(cd "$(dirname "$0")/.." && pwd)
isolated_target=$(mktemp -d "${TMPDIR:-/tmp}/hsum-reproducible-build.XXXXXX")
cleanup() {
  rm -rf "$isolated_target"
}
trap cleanup EXIT

(
  cd "$repository_root"
  CARGO_TARGET_DIR="$isolated_target" cargo build --locked --release
)
rebuilt="$isolated_target/release/hsum"
if ! cmp -s "$candidate" "$rebuilt"; then
  echo "isolated release build is not byte-for-byte reproducible" >&2
  echo "candidate: $(shasum -a 256 "$candidate" | awk '{print $1}')" >&2
  echo "rebuilt:   $(shasum -a 256 "$rebuilt" | awk '{print $1}')" >&2
  exit 1
fi

echo "reproducible release build passed: $(shasum -a 256 "$candidate" | awk '{print $1}')"
