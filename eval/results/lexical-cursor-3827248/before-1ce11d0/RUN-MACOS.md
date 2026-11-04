# Mac 100k qualification

Candidate: `1ce11d00bf8cad949c12a9c496ab7dba3675717f`, clean attached
qualification checkout. The binary and query manifest hashes, machine, and
protocol are recorded in `launch.json`.

Preparation is running under `caffeinate`. Its output is in `prepare.log`.
Wait until `prepare.exit` contains `0` and `setup-macos-arm64.json` exists.
A nonzero exit means preparation failed; inspect the log before continuing.

The full measurement requires macOS cache eviction. Unattended `sudo -n
/usr/sbin/purge` returned `sudo: a password is required`. Run the following
in your own Terminal after successful preparation. Enter your password only
at the local sudo prompt. Keep the Mac on power and avoid other heavy jobs
during measurement. This uses the tracked cache helper and all frozen gates.

```bash
cd /Users/b.k./.codex/worktrees/retrieval-qualification/hSUM &&
test "$(cat /Users/b.k./.cache/hsum-qualification/2026-10-23-100k-lbse5n1f/prepare.exit)" = 0 &&
sudo -v &&
/usr/bin/caffeinate -i python3 -u benches/retrieval_scale/harness.py run \
  --hsum /Users/b.k./.cache/hsum-qualification/2026-10-23-100k-lbse5n1f/bin/hsum \
  --corpus /Users/b.k./.cache/hsum-qualification/2026-10-23-100k-lbse5n1f/corpus \
  --home /Users/b.k./.cache/hsum-qualification/2026-10-23-100k-lbse5n1f/home \
  --setup-report /Users/b.k./.cache/hsum-qualification/2026-10-23-100k-lbse5n1f/setup-macos-arm64.json \
  --cold-cache-helper /Users/b.k./.codex/worktrees/retrieval-qualification/hSUM/benches/retrieval_scale/drop-cache-macos.sh \
  --output /Users/b.k./.cache/hsum-qualification/2026-10-23-100k-lbse5n1f/macos-arm64-100k.json
```

Inspect the final JSON and command exit status even when the report is written:
a report can retain valid observations while failing a latency or variability
gate. Do not use `--allow-missing-cold-cache` for qualification.

## Continuation launcher

`run-macos-qualification.command` is now open in Terminal. Authenticate only
at its local `sudo -v` prompt. It keeps that Terminal's authorization valid,
waits for `prepare.exit = 0` plus the setup report, then runs the exact command
above under `caffeinate`. It preserves `measurement.started`, `measurement.log`,
and `measurement.exit`, and refuses a duplicate launch. A nonzero measurement
exit is retained as a failure; no threshold or cold-cache bypass is used.
Preparation and measurement remain pinned to `1ce11d0`, regardless of newer
source changes in the primary checkout.

The active launcher owns this run. Keep the manual command as a reference; do
not start a second measurement while the launcher is waiting or measuring.
