#!/usr/bin/env bash
# Exercise the built hSUM executable as a first-time user without writing into
# the checkout. This is intentionally shell-only so both supported CI runners
# test the same artifact the release job packages.
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 /absolute/path/to/hsum" >&2
  exit 2
fi

binary="$1"
if [ ! -x "$binary" ]; then
  echo "hsum binary is not executable: $binary" >&2
  exit 2
fi

repository_root=$(cd "$(dirname "$0")/.." && pwd)
expected_version=$(awk -F '"' '$1 ~ /^version = / { print $2; exit }' "$repository_root/Cargo.toml")
if [ -z "$expected_version" ]; then
  echo "could not determine the expected package version" >&2
  exit 2
fi

trial_root=$(mktemp -d "${TMPDIR:-/tmp}/hsum-release-smoke.XXXXXX")
cleanup() {
  rm -rf "$trial_root"
}
trap cleanup EXIT

repository="$trial_root/repository"
export HSUM_HOME="$trial_root/hsum-home"
mkdir -p "$repository/src"
git init --quiet "$repository"

cat > "$repository/README.md" <<'EOF'
# Release smoke fixture

AlphaIdentifier is evidence indexed by the candidate artifact.
EOF

cat > "$repository/src/lib.rs" <<'EOF'
pub const ALPHA_IDENTIFIER: &str = "AlphaIdentifier";
EOF

"$binary" --version --verbose > "$trial_root/version.txt"
grep -Fqx "hsum $expected_version" <(head -n 1 "$trial_root/version.txt")

cd "$repository"
"$binary" --no-color --no-progress init --dry-run > "$trial_root/init-dry-run.txt"
init_started=$(date +%s)
"$binary" --no-color --no-progress init > "$trial_root/init.txt"
init_finished=$(date +%s)
printf 'first_init_seconds=%s\n' "$((init_finished - init_started))"
"$binary" --no-color --no-progress search AlphaIdentifier --json > "$trial_root/search.json"

citation=$(python3 -c 'import json, sys; print(json.load(sys.stdin)["results"][0]["citation_uri"])' < "$trial_root/search.json")
"$binary" --no-color --no-progress get "$citation" --verify-source-hash --json > "$trial_root/get.json"
"$binary" --no-color --no-progress context --json > "$trial_root/context.json"
"$binary" --no-color --no-progress doctor > "$trial_root/doctor.txt"
"$binary" --no-color --no-progress client config generic --format json > "$trial_root/client.json" 2> "$trial_root/client-warning.txt"

HSUM_BINARY="$binary" HSUM_REPOSITORY="$repository" python3 - <<'PY'
import json
import os
import subprocess

process = subprocess.Popen(
    [os.environ["HSUM_BINARY"], "mcp"],
    cwd=os.environ["HSUM_REPOSITORY"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
)

def send(frame):
    process.stdin.write(json.dumps(frame) + "\n")
    process.stdin.flush()

def receive():
    line = process.stdout.readline()
    assert line, process.stderr.read()
    return json.loads(line)

send({
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
        "protocolVersion": "2025-11-25",
        "capabilities": {},
        "clientInfo": {"name": "release-smoke", "version": "1"},
    },
})
initialized = receive()
assert initialized["id"] == 1
assert initialized["result"]["serverInfo"]["name"] == "hsum"

send({"jsonrpc": "2.0", "method": "notifications/initialized"})
send({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
tools = receive()["result"]["tools"]
assert {tool["name"] for tool in tools} == {
    "evidence_get",
    "evidence_project",
    "evidence_search",
    "evidence_status",
}

process.stdin.close()
assert process.wait(timeout=5) == 0
assert process.stderr.read() == ""
PY

printf '\377\376' > README.md
printf '\377\376' > src/lib.rs
set +e
"$binary" --no-color --no-progress ingest > "$trial_root/failed-ingest.txt" 2> "$trial_root/failed-ingest-error.txt"
failed_ingest_status=$?
set -e
test "$failed_ingest_status" -eq 1
grep -Fq 'FAILED:' "$trial_root/failed-ingest-error.txt"

python3 - "$trial_root" <<'PY'
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
search = json.loads((root / "search.json").read_text())
get = json.loads((root / "get.json").read_text())
context = json.loads((root / "context.json").read_text())
client = json.loads((root / "client.json").read_text())

assert search["schema_version"] == "hsum.api.v1"
assert search["results"]
assert get["source_hash_verification"] == "unchanged"
assert get["untrusted_content"] is True
assert pathlib.Path(context["database_path"]).resolve().is_relative_to(root.resolve())
assert client["hsum"]["args"][0] == "mcp"
assert "uploads no corpus data or telemetry" in (root / "client-warning.txt").read_text()
PY

test ! -e .hsum.toml
echo "release smoke passed"
