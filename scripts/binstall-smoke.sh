#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 4 ]; then
  echo "usage: binstall-smoke.sh BINSTALL_BIN TARGET INSTALL_DIR REPORT_JSON" >&2
  exit 2
fi

binstall_bin=$1
target=$2
install_dir=$3
report_json=$4

repo_root=$(cd "$(dirname "$0")/.." && pwd -P)
cd "$repo_root"

case "$target" in
  aarch64-apple-darwin|x86_64-unknown-linux-gnu) ;;
  *) echo "unsupported cargo-binstall smoke target: $target" >&2; exit 2 ;;
esac
test -x "$binstall_bin" || { echo "cargo-binstall is not executable: $binstall_bin" >&2; exit 1; }
test ! -e "$install_dir/hsum" || { echo "install target already exists: $install_dir/hsum" >&2; exit 1; }
mkdir -p "$install_dir" "$(dirname "$report_json")"

python3 scripts/verify_binstall_metadata.py
version=$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["package"]["version"])')
binstall_version=$("$binstall_bin" -V | sed -n '1p')

"$binstall_bin" \
  --manifest-path Cargo.toml \
  --version "=$version" \
  --targets "$target" \
  --strategies crate-meta-data \
  --install-path "$install_dir" \
  --no-confirm \
  --disable-telemetry \
  --no-discover-github-token \
  hsum

candidate="$install_dir/hsum"
test -x "$candidate" || { echo "cargo-binstall did not install hsum" >&2; exit 1; }
test ! -e "$install_dir/xtask" || { echo "cargo-binstall exposed internal xtask" >&2; exit 1; }
version_output=$("$candidate" --version --verbose)
grep -Fqx "hsum $version" <<< "$version_output"
grep -Fqx "Target: $target" <<< "$version_output"
installed_sha256=$(shasum -a 256 "$candidate" | awk '{print $1}')
if [ -n "${GITHUB_SHA:-}" ]; then
  test "$(git rev-parse HEAD)" = "$GITHUB_SHA" || {
    echo "checkout HEAD does not match GITHUB_SHA" >&2
    exit 1
  }
  git diff --quiet
  git diff --cached --quiet
  source_sha=$GITHUB_SHA
  source_state=github-clean-checkout
else
  source_sha=$(git rev-parse HEAD)
  source_state=local-working-tree
fi
run_id=${GITHUB_RUN_ID:-local}

python3 - \
  "$report_json" "$target" "$version" "$binstall_version" \
  "$installed_sha256" "$source_sha" "$source_state" "$run_id" <<'PY'
import json
import pathlib
import sys

(
    report_path,
    target,
    package_version,
    binstall_version,
    installed_sha256,
    source_sha,
    source_state,
    run_id,
) = sys.argv[1:]
report = {
    "schema_version": "hsum.binstall-smoke.v1",
    "passed": True,
    "target": target,
    "package_version": package_version,
    "binstall_version": binstall_version,
    "installed_sha256": installed_sha256,
    "source_commit_sha": source_sha,
    "source_state": source_state,
    "github_run_id": run_id,
    "strategy": "crate-meta-data",
    "compile_fallback_used": False,
    "quick_install_used": False,
    "telemetry_disabled": True,
    "installed_binary": "hsum",
    "internal_xtask_absent": True,
}
pathlib.Path(report_path).write_text(
    json.dumps(report, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="utf-8",
)
PY

echo "cargo-binstall smoke passed: target=$target sha256=$installed_sha256"
