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
import select
import signal
import subprocess
import time

process = subprocess.Popen(
    [os.environ["HSUM_BINARY"], "--no-color", "--no-progress", "mcp"],
    cwd=os.environ["HSUM_REPOSITORY"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
)
stopped_workers = set()

def send(frame):
    process.stdin.write(json.dumps(frame, separators=(",", ":")) + "\n")
    process.stdin.flush()

def receive(timeout=10):
    readable, _, _ = select.select([process.stdout], [], [], timeout)
    assert readable, f"timed out waiting for MCP response; process={process.poll()}"
    line = process.stdout.readline()
    assert line, f"MCP server disconnected; process={process.poll()}"
    return json.loads(line)

def search(request_id, mode, query="cobalt lantern", timeout_ms=10_000):
    send({
        "jsonrpc": "2.0",
        "id": request_id,
        "method": "tools/call",
        "params": {
            "name": "evidence_search",
            "arguments": {
                "query": query,
                "mode": mode,
                "limit": 5,
                "timeout_ms": timeout_ms,
                "explain": True,
            },
        },
    })

def receive_until(request_id, forbidden=(), timeout=10):
    deadline = time.monotonic() + timeout
    while True:
        response = receive(max(0, deadline - time.monotonic()))
        response_id = response.get("id")
        assert response_id not in forbidden, (
            f"cancelled request {response_id} emitted a late response"
        )
        if response_id == request_id:
            return response

def model_worker_pids():
    listing = subprocess.run(
        ["ps", "-axo", "pid=,ppid=,command="],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    workers = []
    for line in listing.splitlines():
        fields = line.strip().split(None, 2)
        if len(fields) != 3:
            continue
        pid_text, parent_text, command = fields
        if int(parent_text) == process.pid and "__model-worker" in command:
            workers.append(int(pid_text))
    return workers

def stop_real_model_workers(expected):
    deadline = time.monotonic() + 10
    while len(stopped_workers) < expected and time.monotonic() < deadline:
        for pid in model_worker_pids():
            if pid in stopped_workers:
                continue
            try:
                os.kill(pid, signal.SIGSTOP)
            except ProcessLookupError:
                continue
            stopped_workers.add(pid)
        time.sleep(0.005)
    assert len(stopped_workers) == expected, (
        f"expected {expected} real model workers, observed {sorted(stopped_workers)}"
    )

def resume_model_workers():
    for pid in tuple(stopped_workers):
        try:
            os.kill(pid, signal.SIGCONT)
        except ProcessLookupError:
            pass

def summarize(response, mode):
    assert "error" not in response, response
    packet = response["result"]["structuredContent"]
    assert packet["requested_mode"] == mode
    assert packet["effective_mode"] == mode
    assert packet["results"]
    assert "vector" in packet["retrievers"]
    return {
        "effective_mode": packet["effective_mode"],
        "retrievers": packet["retrievers"],
        "examined": packet["examined"],
        "result_count": len(packet["results"]),
    }

try:
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

    cancellation_ids = (2, 3)
    heavy_query = "semantic cancellation " + ("archive evidence " * 180)
    for request_id in cancellation_ids:
        search(request_id, "semantic", f"{heavy_query}{request_id}")
    stop_real_model_workers(expected=2)

    timeout_id = 4
    search(
        timeout_id,
        "semantic",
        "semantic timeout while private workers remain occupied",
        timeout_ms=250,
    )
    timed_out = receive_until(timeout_id, forbidden=cancellation_ids, timeout=3)
    timeout_error = timed_out["error"]["data"]
    assert timeout_error["code"] == "TIMEOUT", timeout_error
    assert timeout_error["subcode"] == "REQUEST_DEADLINE", timeout_error
    assert timeout_error["retryable"] is True, timeout_error

    for request_id in cancellation_ids:
        send({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": request_id, "reason": "native qualification"},
        })
    resume_model_workers()

    semantic_id = 5
    search(semantic_id, "semantic")
    semantic_response = receive_until(
        semantic_id,
        forbidden=cancellation_ids,
        timeout=15,
    )
    hybrid_id = 6
    search(hybrid_id, "hybrid")
    hybrid_response = receive_until(
        hybrid_id,
        forbidden=cancellation_ids,
        timeout=10,
    )
    summaries = {
        "semantic": summarize(semantic_response, "semantic"),
        "hybrid": summarize(hybrid_response, "hybrid"),
        "cancellation_timeout": {
            "real_worker_processes_paused": len(stopped_workers),
            "cancelled_requests": len(cancellation_ids),
            "late_cancelled_responses_suppressed": True,
            "timeout_code": timeout_error["code"],
            "timeout_subcode": timeout_error["subcode"],
            "timeout_retryable": timeout_error["retryable"],
            "semantic_recovery": True,
        },
    }

    process.stdin.close()
    assert process.wait(timeout=10) == 0
    assert process.stderr.read() == ""
finally:
    resume_model_workers()
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)

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
