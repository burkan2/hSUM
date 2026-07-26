#!/usr/bin/env bash
# Build upgrade evidence from the actual published alpha.1 executable. This
# intentionally downloads a checksum-pinned release asset instead of
# synthesizing an old index with the current binary.
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 /absolute/path/to/current-hsum" >&2
  exit 2
fi

current_binary="$1"
if [ ! -x "$current_binary" ]; then
  echo "current hsum binary is not executable: $current_binary" >&2
  exit 2
fi

case "$("$current_binary" --version)" in
  "hsum 0.1.0-alpha.2") ;;
  *)
    echo "upgrade smoke requires an hsum 0.1.0-alpha.2 candidate" >&2
    exit 2
    ;;
esac

case "$(uname -s):$(uname -m)" in
  Linux:x86_64)
    asset="hsum-v0.1.0-alpha.1-x86_64-unknown-linux-gnu.tar.gz"
    expected_sha256="ae2a8a98cd2179a4f3c367afb99ffedf603bf8587dff0b7e6321641a29f58964"
    archive_kind="tar"
    ;;
  Darwin:arm64)
    asset="hsum-v0.1.0-alpha.1-aarch64-apple-darwin.zip"
    expected_sha256="aff1142dadf674f222962c11535095a3d8fc51ca38aafc54824e8f8cc2076143"
    archive_kind="zip"
    ;;
  *)
    echo "released alpha.1 upgrade smoke supports Linux x86_64 and macOS arm64" >&2
    exit 2
    ;;
esac

trial_root=$(mktemp -d "${TMPDIR:-/tmp}/hsum-alpha1-upgrade.XXXXXX")
cleanup() {
  rm -rf "$trial_root"
}
trap cleanup EXIT

archive="$trial_root/$asset"
release_url="https://github.com/burkan2/hSUM/releases/download/v0.1.0-alpha.1/$asset"
curl --fail --location --silent --show-error "$release_url" --output "$archive"
actual_sha256=$(shasum -a 256 "$archive" | awk '{print $1}')
if [ "$actual_sha256" != "$expected_sha256" ]; then
  echo "published alpha.1 archive checksum mismatch" >&2
  echo "expected: $expected_sha256" >&2
  echo "actual:   $actual_sha256" >&2
  exit 1
fi

released_dir="$trial_root/released"
mkdir -p "$released_dir"
if [ "$archive_kind" = "zip" ]; then
  unzip -q "$archive" -d "$released_dir"
  released_binary="$released_dir/release/hsum"
else
  tar -xzf "$archive" -C "$released_dir"
  released_binary="$released_dir/hsum"
fi
if [ ! -x "$released_binary" ]; then
  echo "published alpha.1 archive did not contain an executable hsum binary" >&2
  exit 1
fi
if command -v xattr >/dev/null 2>&1; then
  xattr -d com.apple.quarantine "$released_binary" 2>/dev/null || true
fi
test "$("$released_binary" --version)" = "hsum 0.1.0-alpha.1"

repository="$trial_root/repository"
export HSUM_HOME="$trial_root/hsum-home"
mkdir -p "$repository"
git init --quiet "$repository"
cat > "$repository/README.md" <<'EOF'
# Released-index upgrade fixture

ReleasedAlphaUpgradeIdentifier is evidence created by the published alpha.1 binary.
EOF

cd "$repository"
"$released_binary" --no-color --no-progress init > "$trial_root/alpha1-init.txt"
"$released_binary" --no-color --no-progress doctor > "$trial_root/alpha1-doctor.txt"
"$released_binary" --no-color --no-progress search \
  ReleasedAlphaUpgradeIdentifier --json > "$trial_root/alpha1-search.json"
old_citation=$(python3 -c \
  'import json, sys; print(json.load(sys.stdin)["results"][0]["citation_uri"])' \
  < "$trial_root/alpha1-search.json")

set +e
"$current_binary" --no-color --no-progress search \
  ReleasedAlphaUpgradeIdentifier > "$trial_root/stale-search.txt" \
  2> "$trial_root/stale-search-error.txt"
stale_status=$?
set -e
test "$stale_status" -eq 5
grep -Fq "code: PIPELINE_FINGERPRINT" "$trial_root/stale-search-error.txt"
grep -Fq "hsum init --rebuild" "$trial_root/stale-search-error.txt"

"$current_binary" --no-color --no-progress init --rebuild --dry-run \
  > "$trial_root/rebuild-dry-run.txt"
grep -Fq "Would replace the trusted index" "$trial_root/rebuild-dry-run.txt"

"$current_binary" --no-color --no-progress init --rebuild \
  > "$trial_root/rebuild.txt"
grep -Fq "Prior evidence and citations no longer resolve" "$trial_root/rebuild.txt"
"$current_binary" --no-color --no-progress doctor > "$trial_root/alpha2-doctor.txt"
"$current_binary" --no-color --no-progress search \
  ReleasedAlphaUpgradeIdentifier --json > "$trial_root/alpha2-search.json"

set +e
"$current_binary" --no-color --no-progress get "$old_citation" --json \
  > "$trial_root/old-get.txt" 2> "$trial_root/old-get-error.json"
old_get_status=$?
set -e
test "$old_get_status" -eq 1
python3 - "$trial_root/old-get-error.json" <<'PY'
import json
import pathlib
import sys

error = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert error["code"] == "NOT_FOUND", error
PY

echo "released alpha.1 upgrade smoke passed"
