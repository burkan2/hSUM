#!/bin/bash
set -u
qualification_root=/Users/b.k./.cache/hsum-qualification/2026-10-24-macos-recovery-b6591b7
cd "$qualification_root/source" || exit 1
if [ -e "$qualification_root/measurement.started" ]; then
  echo 'Measurement was already launched. Inspect measurement.log and measurement.exit.'
  exit 1
fi
if [ "$(cat "$qualification_root/recovery.exit")" != 0 ] || [ ! -f "$qualification_root/setup-recovered-macos-arm64.json" ]; then
  echo 'Recovery is not ready. Inspect recovery.log; no measurement was started.'
  exit 1
fi
printf 'hSUM recovered Mac 100k protocol — candidate b6591b7\n'
printf 'Cached inference was reused. Fresh ingest/inference throughput remains unqualified.\n'
printf 'Authenticate only at the local sudo prompt below.\n'
sudo -v || exit 1
(while /bin/kill -0 "$$" 2>/dev/null; do sudo -n -v || exit; sleep 60; done) &
qualification_keepalive=$!
trap 'kill "$qualification_keepalive" 2>/dev/null || true' EXIT
( set -o noclobber; date -u > "$qualification_root/measurement.started" ) || exit 1
set -o pipefail
/usr/bin/caffeinate -i python3 -u benches/retrieval_scale/harness.py run \
  --hsum "$qualification_root/bin/hsum" \
  --corpus /Users/b.k./.cache/hsum-qualification/2026-10-23-100k-lbse5n1f/corpus \
  --home "$qualification_root/home" \
  --setup-report "$qualification_root/setup-recovered-macos-arm64.json" \
  --cold-cache-helper "$qualification_root/source/benches/retrieval_scale/drop-cache-macos.sh" \
  --output "$qualification_root/macos-arm64-100k-recovered.json" 2>&1 | tee "$qualification_root/measurement.log"
qualification_statuses=("${PIPESTATUS[@]}")
qualification_exit=${qualification_statuses[0]}
if [ "$qualification_exit" = 0 ] && [ "${qualification_statuses[1]}" != 0 ]; then qualification_exit=${qualification_statuses[1]}; fi
printf '%s\n' "$qualification_exit" > "$qualification_root/measurement.exit"
printf '\nMeasurement exit status: %s\nEvidence: %s\n' "$qualification_exit" "$qualification_root"
exit "$qualification_exit"
