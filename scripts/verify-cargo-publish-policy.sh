#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd -P)
cd "$repo_root"

if [ "$#" -gt 1 ]; then
  echo "usage: verify-cargo-publish-policy.sh [CARGO_TOML]" >&2
  exit 2
fi

manifest=${1:-Cargo.toml}
python3 - "$manifest" <<'PY'
import pathlib
import sys
import tomllib

manifest = pathlib.Path(sys.argv[1])
try:
    package = tomllib.loads(manifest.read_text(encoding="utf-8"))["package"]
except (OSError, KeyError, tomllib.TOMLDecodeError) as error:
    raise SystemExit(f"could not read Cargo publish policy from {manifest}: {error}")

if package.get("publish") != ["crates-io"]:
    raise SystemExit(
        f'{manifest}: [package].publish must be exactly ["crates-io"]'
    )
PY

echo "Cargo publish policy verified: crates-io is the only allowed registry."
