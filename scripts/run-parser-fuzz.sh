#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: run-parser-fuzz.sh LOG_DIRECTORY" >&2
  exit 2
fi
seconds=${FUZZ_SECONDS:-15}
if [[ ! "$seconds" =~ ^[1-9][0-9]{0,3}$ ]] || (( seconds > 1800 )); then
  echo "FUZZ_SECONDS must be an integer from 1 to 1800" >&2
  exit 2
fi
mkdir -p "$1"
for target in citation_uri query_parser search_cursor jsonl_snapshot mcp_frame chunk_bytes; do
  cargo +nightly-2026-08-15 fuzz run "$target" -- \
    -max_total_time="$seconds" -max_len=65536 -timeout=5 \
    -rss_limit_mb=2048 -print_final_stats=1 2>&1 | tee "$1/$target.log"
done
