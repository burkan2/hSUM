#!/usr/bin/env bash
# Run the frozen five-trial first-user protocol against one prebuilt artifact.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 /absolute/path/to/hsum /absolute/path/to/evidence-directory" >&2
  exit 2
fi

binary="$1"
evidence_root="$2"
case "$binary:$evidence_root" in
  /*:/*) ;;
  *) echo "binary and evidence directory paths must be absolute" >&2; exit 2 ;;
esac
if [ ! -x "$binary" ]; then
  echo "hsum binary is not executable: $binary" >&2
  exit 2
fi
if [ -e "$evidence_root" ]; then
  if [ ! -d "$evidence_root" ] || [ -n "$(find "$evidence_root" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
    echo "evidence directory must be absent or empty: $evidence_root" >&2
    exit 2
  fi
else
  mkdir -p "$evidence_root"
fi

repository_root=$(cd "$(dirname "$0")/.." && pwd)
binary_sha256=$(shasum -a 256 "$binary" | awk '{print $1}')
version=$($binary --version --verbose | head -n 1)
platform=$(uname -s)
architecture=$(uname -m)
candidate_commit_sha=${HSUM_CANDIDATE_COMMIT_SHA:-${GITHUB_SHA:-unverified-local}}

for trial in 1 2 3 4 5; do
  log="$evidence_root/trial-$trial.log"
  started=$(date +%s)
  bash "$repository_root/scripts/release-smoke.sh" "$binary" > "$log" 2>&1
  finished=$(date +%s)
  grep -Fqx "release smoke passed" "$log"
  printf '%s\n' "$((finished - started))" > "$evidence_root/trial-$trial.duration-seconds"
done

HSUM_TRIAL_EVIDENCE_ROOT="$evidence_root" \
HSUM_TRIAL_BINARY_SHA256="$binary_sha256" \
HSUM_TRIAL_VERSION="$version" \
HSUM_TRIAL_PLATFORM="$platform" \
HSUM_TRIAL_ARCHITECTURE="$architecture" \
HSUM_TRIAL_COMMIT_SHA="$candidate_commit_sha" \
HSUM_TRIAL_RUN_ID="${GITHUB_RUN_ID:-local}" \
HSUM_TRIAL_RUN_ATTEMPT="${GITHUB_RUN_ATTEMPT:-local}" \
python3 - <<'PY'
import json
import os
import pathlib

root = pathlib.Path(os.environ["HSUM_TRIAL_EVIDENCE_ROOT"])
trials = []
for trial in range(1, 6):
    log_path = root / f"trial-{trial}.log"
    log = log_path.read_text(encoding="utf-8")
    first_init = next(
        int(line.removeprefix("first_init_seconds="))
        for line in log.splitlines()
        if line.startswith("first_init_seconds=")
    )
    trials.append({
        "trial": trial,
        "passed": log.splitlines()[-1] == "release smoke passed",
        "first_init_seconds": first_init,
        "total_seconds": int(
            (root / f"trial-{trial}.duration-seconds").read_text(encoding="utf-8")
        ),
    })

report = {
    "schema_version": "hsum.clean-machine-trials.v1",
    "passed": all(trial["passed"] for trial in trials) and len(trials) == 5,
    "protocol": {
        "trial_count": 5,
        "fresh_repository_per_trial": True,
        "fresh_hsum_home_per_trial": True,
        "cli_citation_round_trip": True,
        "mcp_search_get_status_project_round_trip": True,
    },
    "candidate": {
        "version": os.environ["HSUM_TRIAL_VERSION"],
        "sha256": os.environ["HSUM_TRIAL_BINARY_SHA256"],
        "commit_sha": os.environ["HSUM_TRIAL_COMMIT_SHA"],
    },
    "platform": {
        "system": os.environ["HSUM_TRIAL_PLATFORM"],
        "machine": os.environ["HSUM_TRIAL_ARCHITECTURE"],
    },
    "github": {
        "run_id": os.environ["HSUM_TRIAL_RUN_ID"],
        "run_attempt": os.environ["HSUM_TRIAL_RUN_ATTEMPT"],
    },
    "trials": trials,
}
assert report["passed"] is True
(root / "report.json").write_text(
    json.dumps(report, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="utf-8",
)
PY

echo "five clean-machine trials passed"
