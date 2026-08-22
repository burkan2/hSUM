#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: semantic-product-smoke.sh HSUM_BINARY REPORT_PATH" >&2
  exit 2
fi

binary_input=$1
report_path=$2
binary_dir=$(cd "$(dirname "$binary_input")" && pwd -P)
binary="$binary_dir/$(basename "$binary_input")"
test -x "$binary" || { echo "hsum binary is not executable: $binary" >&2; exit 1; }
test -n "${HSUM_HOME:-}" || { echo "HSUM_HOME must select an isolated model cache" >&2; exit 1; }
export HSUM_OFFLINE=1

trial_root=$(mktemp -d)
trap 'rm -rf "$trial_root"' EXIT
repository="$trial_root/repository"
mkdir "$repository"
git -C "$repository" init -q
git -C "$repository" config user.email smoke@hsum.invalid
git -C "$repository" config user.name "hSUM semantic smoke"

cat > "$repository/semantic.md" <<'EOF'
The cobalt lantern protocol preserves immutable local evidence for later review.
EOF
cat > "$repository/operations.md" <<'EOF'
Operators verify the archive checksum before they inspect a release candidate.
EOF
git -C "$repository" add semantic.md operations.md
git -C "$repository" commit -q -m "semantic product smoke fixture"

common=("$binary" --no-color --no-progress)
(
  cd "$repository"
  "${common[@]}" init . \
    --index native-semantic \
    --project default \
    --embedding-model bge-small-en-v1-5-fp32 \
    > "$trial_root/init.txt"
  "${common[@]}" ingest --reembed > "$trial_root/reembed.txt"
  "${common[@]}" model list --json > "$trial_root/models.json"
  for mode in auto lexical semantic hybrid; do
    "${common[@]}" search "cobalt lantern" \
      --mode "$mode" \
      --limit 5 \
      --timeout-ms 10000 \
      --explain \
      --json \
      > "$trial_root/search-$mode.json"
  done
)

citation=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["results"][0]["citation_uri"])' \
  < "$trial_root/search-hybrid.json")
(
  cd "$repository"
  "${common[@]}" get "$citation" --verify-source-hash --json > "$trial_root/get.json"
)

HSUM_BINARY="$binary" \
HSUM_REPOSITORY="$repository" \
MCP_REPORT_PATH="$trial_root/mcp.json" \
python3 - <<'PY'
import json
import os
import subprocess

process = subprocess.Popen(
    [os.environ["HSUM_BINARY"], "--no-color", "--no-progress", "mcp"],
    cwd=os.environ["HSUM_REPOSITORY"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
)

def send(frame):
    process.stdin.write(json.dumps(frame, separators=(",", ":")) + "\n")
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
        "clientInfo": {"name": "semantic-product-smoke", "version": "1"},
    },
})
initialized = receive()
assert initialized["id"] == 1
assert initialized["result"]["serverInfo"]["name"] == "hsum"
send({"jsonrpc": "2.0", "method": "notifications/initialized"})

summaries = {}
for request_id, mode in enumerate(("semantic", "hybrid"), start=2):
    send({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "tools/call",
        "params": {
            "name": "evidence_search",
            "arguments": {
                "query": "cobalt lantern",
                "mode": mode,
                "limit": 5,
                "timeout_ms": 10000,
                "explain": True,
            },
        },
    })
    response = receive()
    assert "error" not in response, response
    packet = response["result"]["structuredContent"]
    assert packet["requested_mode"] == mode
    assert packet["effective_mode"] == mode
    assert packet["results"]
    assert "vector" in packet["retrievers"]
    summaries[mode] = {
        "effective_mode": packet["effective_mode"],
        "retrievers": packet["retrievers"],
        "examined": packet["examined"],
        "result_count": len(packet["results"]),
    }

process.stdin.close()
assert process.wait(timeout=10) == 0
assert process.stderr.read() == ""
with open(os.environ["MCP_REPORT_PATH"], "w", encoding="utf-8") as output:
    json.dump(summaries, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY

HSUM_SMOKE_ROOT="$trial_root" \
HSUM_SMOKE_REPORT="$report_path" \
HSUM_SMOKE_COMMIT="${GITHUB_SHA:-unknown}" \
python3 - <<'PY'
import json
import os
import pathlib
import platform

root = pathlib.Path(os.environ["HSUM_SMOKE_ROOT"])

def load(name):
    return json.loads((root / name).read_text(encoding="utf-8"))

models = load("models.json")
assert models["selected_index"] == "native-semantic"
assert models["selected_index_state"] == "indexed"
model = models["models"][0]
assert model["id"] == "bge-small-en-v1-5-fp32"
assert model["state"] == "installed"
assert model["pinned_by_selected_index"] is True

searches = {mode: load(f"search-{mode}.json") for mode in ("auto", "lexical", "semantic", "hybrid")}
for mode, packet in searches.items():
    assert packet["requested_mode"] == mode
    assert packet["results"], mode
assert searches["auto"]["effective_mode"] == "hybrid"
assert searches["lexical"]["effective_mode"] == "lexical"
assert searches["semantic"]["effective_mode"] == "semantic"
assert searches["hybrid"]["effective_mode"] == "hybrid"
assert searches["lexical"]["examined"]["vector"] == 0
assert searches["lexical"]["timing_ms"]["query_embedding"] == 0
assert searches["semantic"]["retrievers"] == ["vector"]
assert "vector" in searches["auto"]["retrievers"]
assert "vector" in searches["hybrid"]["retrievers"]
assert [item["citation_uri"] for item in searches["auto"]["results"]] == [
    item["citation_uri"] for item in searches["hybrid"]["results"]
]

get = load("get.json")
assert get["source_hash_verification"] == "unchanged"
assert "cobalt lantern" in get["content"]

report = {
    "schema_version": "hsum.semantic-product-smoke.v1",
    "passed": True,
    "commit_sha": os.environ["HSUM_SMOKE_COMMIT"],
    "platform": {"system": platform.system(), "machine": platform.machine()},
    "offline": True,
    "model": {
        "id": model["id"],
        "state": model["state"],
        "manifest_sha256": model["manifest_sha256"],
        "upstream_revision": model["upstream_revision"],
        "index_state": models["selected_index_state"],
    },
    "cli": {
        mode: {
            "effective_mode": packet["effective_mode"],
            "retrievers": packet["retrievers"],
            "examined": packet["examined"],
            "result_count": len(packet["results"]),
        }
        for mode, packet in searches.items()
    },
    "mcp": load("mcp.json"),
    "immutable_get_verified": True,
}
report_path = pathlib.Path(os.environ["HSUM_SMOKE_REPORT"])
report_path.parent.mkdir(parents=True, exist_ok=True)
report_path.write_text(
    json.dumps(report, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="utf-8",
)
PY

echo "semantic product smoke passed: $report_path"

