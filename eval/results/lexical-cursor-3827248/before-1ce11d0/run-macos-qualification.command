#!/bin/bash
set -u
qualification_root=/Users/b.k./.cache/hsum-qualification/2026-10-23-100k-lbse5n1f
qualification_checkout=/Users/b.k./.codex/worktrees/retrieval-qualification/hSUM
cd "$qualification_checkout" || exit 1
if [ -e "$qualification_root/measurement.started" ]; then
  echo 'A measurement was already launched. Inspect measurement.log and measurement.exit before repeating.'
  exit 1
fi
printf 'Authenticate locally now; measurement will wait for successful preparation.\n'
sudo -v || exit 1
# Keep this Terminal's sudo ticket valid for the three cold-cache measurements.
(while /bin/kill -0 "$$" 2>/dev/null; do sudo -n -v || exit; sleep 60; done) &
qualification_keepalive=$!
trap 'kill "$qualification_keepalive" 2>/dev/null || true' EXIT

printf 'Waiting for Mac 100k preparation to finish. Keep this Terminal open.\n'
while [ ! -f "$qualification_root/prepare.exit" ]; do
  if ! /bin/kill -0 11401 2>/dev/null; then
    echo 'Preparation stopped without an exit receipt. Inspect prepare.log.'
    exit 1
  fi
  sleep 5
done
if [ "$(cat "$qualification_root/prepare.exit")" != 0 ] || [ ! -f "$qualification_root/setup-macos-arm64.json" ]; then
  echo 'Preparation failed. Inspect prepare.log; measurement was not started.'
  exit 1
fi
( set -o noclobber; date -u > "$qualification_root/measurement.started" ) || exit 1
set -o pipefail
/usr/bin/caffeinate -i python3 -u benches/retrieval_scale/harness.py run \
  --hsum "$qualification_root/bin/hsum" \
  --corpus "$qualification_root/corpus" \
  --home "$qualification_root/home" \
  --setup-report "$qualification_root/setup-macos-arm64.json" \
  --cold-cache-helper "$qualification_checkout/benches/retrieval_scale/drop-cache-macos.sh" \
  --output "$qualification_root/macos-arm64-100k.json" 2>&1 | tee "$qualification_root/measurement.log"
qualification_exit=${PIPESTATUS[0]}
printf '%s\n' "$qualification_exit" > "$qualification_root/measurement.exit"
printf '\nMeasurement exit status: %s\nEvidence: %s\n' "$qualification_exit" "$qualification_root"
exit "$qualification_exit"
