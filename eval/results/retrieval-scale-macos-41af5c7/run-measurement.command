#!/bin/bash
set -euo pipefail
root="/Users/b.k./.cache/hsum-qualification/2026-11-01-41af5c7-macos-fresh"
source="/Users/b.k./.codex/worktrees/hsum-qualification-41af5c7/hSUM"
export PYTHONDONTWRITEBYTECODE=1
if [[ ! -f "$root/prepare.exit" ]]; then
  echo "Waiting for fresh preparation; authentication will be requested only after it succeeds."
  deadline=$((SECONDS + 21600))
  while [[ ! -f "$root/prepare.exit" ]]; do
    if (( SECONDS >= deadline )); then
      echo "Preparation did not finish within six hours; no measurement started." >&2
      exit 2
    fi
    sleep 5
  done
fi
if [[ "$(cat "$root/prepare.exit")" != 0 ]]; then
  echo "Fresh preparation is not complete and successful; measurement was not started."
  exit 2
fi
python3 - "$root" <<'VERIFY'
import hashlib,json,pathlib,subprocess,sys
root=pathlib.Path(sys.argv[1]);r=json.loads((root/'launch.json').read_text());s=pathlib.Path(r['source_checkout'])
assert subprocess.check_output(['git','rev-parse','HEAD'],cwd=s,text=True).strip()==r['candidate']
assert not subprocess.check_output(['git','status','--porcelain'],cwd=s,text=True).strip()
assert hashlib.sha256((root/'bin/hsum').read_bytes()).hexdigest()==r['binary_sha256']
for n,h in r['source_sha256'].items():assert hashlib.sha256((s/n).read_bytes()).hexdigest()==h,n
assert (root/'setup-macos-arm64.json').is_file()
assert not (root/'measurement.started').exists(), 'measurement already attempted; preserve prior evidence'
VERIFY
echo "Authenticate locally for the frozen macOS cold-cache protocol."
sudo -v
# Refuse duplicate runs, including after a failed measurement; keep all evidence.
mkdir "$root/measurement.started"
parent_pid=$$
(while kill -0 "$parent_pid" 2>/dev/null; do sudo -n -v || exit; sleep 30; done) &
keepalive_pid=$!
trap 'kill "$keepalive_pid" 2>/dev/null || true' EXIT
python3 - "$root" <<'RECEIPT'
import hashlib,json,pathlib,sys,time
root=pathlib.Path(sys.argv[1]);r=json.loads((root/'launch.json').read_text())
helper=pathlib.Path(r['source_checkout'])/'benches/retrieval_scale/drop-cache-macos.sh'
(root/'measurement-launch.json').write_text(json.dumps({'candidate':r['candidate'],'binary_sha256':r['binary_sha256'],'helper_sha256':hashlib.sha256(helper.read_bytes()).hexdigest(),'started_at_unix_seconds':time.time()},indent=2)+'\n')
RECEIPT
echo "Running the unchanged 3 x 750 observation protocol. Output: $root/measurement.log"
cd "$source"
set +e
caffeinate -i python3 benches/retrieval_scale/harness.py run \
  --hsum "$root/bin/hsum" --corpus "$root/corpus" --home "$root/home" \
  --setup-report "$root/setup-macos-arm64.json" \
  --cold-cache-helper "$source/benches/retrieval_scale/drop-cache-macos.sh" \
  --output "$root/macos-arm64-100k.json" > "$root/measurement.log" 2>&1
result=$?
set -e
printf '%s\n' "$result" > "$root/measurement.exit"
echo "Measurement finished with exit $result. Evidence retained in $root."
exit "$result"
