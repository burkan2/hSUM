#!/usr/bin/env bash
#
# Agent retrieval benchmark: hSUM vs. the tools an agent uses without it.
#
# What this measures, and why it is the honest comparison:
#
# A coding agent answering "where is X handled?" has three realistic options.
#   1. Read the whole file           -- always correct, most expensive
#   2. Shell out to ripgrep/grep     -- cheap, but returns lines with no
#                                       guarantee they still match what was
#                                       indexed, and misses concept queries
#   3. hsum search + hsum get        -- returns a passage pinned to an
#                                       immutable revision, with a verifiable
#                                       source hash
#
# The metric that matters to an agent operator is BYTES RETURNED, because
# bytes become context tokens and tokens are billed. Wall time matters less
# (all three are sub-second) but is recorded anyway.
#
# This harness deliberately does NOT try to make hSUM look cheap. It reports
# whatever the commands actually return, including the cases where ripgrep
# wins on bytes. A benchmark that only publishes favorable numbers is not a
# benchmark.
#
# Usage:
#   benches/agent_retrieval.sh [--json] [--runs N]
#
# Requires: an initialized hSUM index for this repository (hsum init), and
# ripgrep on PATH. Falls back gracefully when a tool is missing.

set -euo pipefail

RUNS=5
FORMAT=human

while [ $# -gt 0 ]; do
  case "$1" in
    --json) FORMAT=json; shift ;;
    --runs) RUNS="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

HSUM="${HSUM:-$REPO_ROOT/target/release/hsum}"
if [ ! -x "$HSUM" ]; then
  echo "error: hsum binary not found at $HSUM" >&2
  echo "build it first: cargo build --locked --release" >&2
  exit 1
fi

if ! "$HSUM" status >/dev/null 2>&1; then
  echo "error: no hSUM index for this repository. Run: $HSUM init" >&2
  exit 1
fi

have_rg=0
command -v rg >/dev/null 2>&1 && have_rg=1

# Median wall-clock milliseconds over RUNS executions of a command.
# Median rather than mean so one scheduler hiccup does not move the number.
median_ms() {
  local -a samples=()
  local i start end
  for ((i = 0; i < RUNS; i++)); do
    start=$(python3 -c 'import time;print(int(time.perf_counter_ns()))')
    eval "$1" >/dev/null 2>&1 || true
    end=$(python3 -c 'import time;print(int(time.perf_counter_ns()))')
    samples+=($(((end - start) / 1000000)))
  done
  printf '%s\n' "${samples[@]}" | sort -n | awk '{a[NR]=$1} END{print (NR%2)?a[(NR+1)/2]:int((a[NR/2]+a[NR/2+1])/2)}'
}

# Bytes written to stdout by a command. This is the payload an agent pays
# context tokens for.
bytes_out() {
  # `|| true` matters: ripgrep exits 1 when a pattern has no match, which is a
  # legitimate benchmark result (scenario 3 depends on it), not a harness
  # failure. Without this, `set -e` would abort the run on the most
  # interesting row.
  local out
  out=$(eval "$1" 2>/dev/null || true)
  printf '%s' "$out" | wc -c | tr -d ' '
}

emit_rows() {
  # scenario | tool | bytes | ms | note
  local scenario="$1" tool="$2" cmd="$3" note="$4"
  local b m
  b=$(bytes_out "$cmd")
  m=$(median_ms "$cmd")
  if [ "$FORMAT" = json ]; then
    printf '  {"scenario":"%s","tool":"%s","bytes":%s,"median_ms":%s,"note":"%s"},\n' \
      "$scenario" "$tool" "$b" "$m" "$note"
  else
    printf '%-26s %-14s %10s %8s  %s\n' "$scenario" "$tool" "$b" "$m" "$note"
  fi
}

if [ "$FORMAT" = json ]; then
  echo '{'
  printf '  "machine": "%s / %s",\n' "$(sysctl -n machdep.cpu.brand_string 2>/dev/null || uname -m)" "$(uname -sr)"
  printf '  "hsum_version": "%s",\n' "$("$HSUM" --version | awk '{print $2}')"
  printf '  "runs": %s,\n' "$RUNS"
  printf '  "corpus": "%s",\n' "$("$HSUM" status 2>/dev/null | awk -F': *' '/Active documents/{d=$2} /Active passages/{p=$2} END{print d" documents, "p" passages"}')"
  echo '  "results": ['
else
  echo
  echo "hSUM agent-retrieval benchmark"
  echo "machine:  $(sysctl -n machdep.cpu.brand_string 2>/dev/null || uname -m), $(uname -sr)"
  echo "hsum:     $("$HSUM" --version)"
  echo "corpus:   $("$HSUM" status 2>/dev/null | awk -F': *' '/Active documents/{d=$2} /Active passages/{p=$2} END{print d" documents, "p" passages"}')"
  echo "runs:     $RUNS (median reported)"
  echo
  printf '%-26s %-14s %10s %8s  %s\n' "SCENARIO" "TOOL" "BYTES" "MED_MS" "NOTE"
  printf '%-26s %-14s %10s %8s  %s\n' "--------------------------" "--------------" "----------" "--------" "----"
fi

# ---------------------------------------------------------------------------
# Scenario 1: exact identifier lookup.
# The common case. An agent knows the symbol name and wants its definition.
# ---------------------------------------------------------------------------
SYM="fn verify_source_hash"

if [ "$have_rg" = 1 ]; then
  emit_rows "1-identifier" "ripgrep" \
  "rg -n '$SYM' src/" "lines only, no verification"
fi

if [ "$have_rg" = 1 ]; then
  emit_rows "1-identifier" "ripgrep+ctx" \
  "rg -n -A20 '$SYM' src/" "what an agent needs to read the body"
fi

emit_rows "1-identifier" "hsum-search" \
  "'$HSUM' search '\"$SYM\"' --limit 1" "ranked, cited"

emit_rows "1-identifier" "hsum-get-1k" \
  "'$HSUM' search '\"$SYM\"' --limit 1 --json | python3 -c \"import sys,json;d=json.load(sys.stdin);print(d['results'][0]['citation_uri'])\" | xargs -I{} '$HSUM' get '{}' --max-bytes 1024" \
  "passage pinned to revision"

emit_rows "1-identifier" "read-whole-file" \
  "cat src/runtime.rs" "always correct, worst cost"

# ---------------------------------------------------------------------------
# Scenario 2: concept query.
# Words that describe a behavior rather than name a symbol. This is where
# lexical tools and hSUM both have limits worth publishing.
# ---------------------------------------------------------------------------
CONCEPT="pipeline fingerprint"

if [ "$have_rg" = 1 ]; then
  emit_rows "2-concept" "ripgrep" \
  "rg -n '$CONCEPT' src/ docs/ README.md" "every literal hit, unranked"
fi

emit_rows "2-concept" "hsum-search" \
  "'$HSUM' search '$CONCEPT' --limit 3" "BM25-ranked, top 3"

# ---------------------------------------------------------------------------
# Scenario 3: paraphrased query, no literal match in the corpus.
# Both tools are lexical, so both are expected to miss. Published because a
# benchmark that hides a limitation is marketing, not measurement.
# ---------------------------------------------------------------------------
PARA="ingest deletion confirmation"

if [ "$have_rg" = 1 ]; then
  emit_rows "3-paraphrase" "ripgrep" \
  "rg -n '$PARA' src/" "expected: no hits"
fi

emit_rows "3-paraphrase" "hsum-search" \
  "'$HSUM' search '$PARA' --limit 3" "expected: no evidence"

# ---------------------------------------------------------------------------
# Scenario 4: cold-start cost.
# hSUM requires an index. ripgrep does not. This is hSUM's setup tax and
# belongs in any honest comparison.
# ---------------------------------------------------------------------------
if [ "$FORMAT" = human ]; then
  echo
  echo "setup cost (one time, not per query):"
  ING_MS=$(median_ms "'$HSUM' ingest --dry-run")
  printf '  %-24s %8s ms   (ripgrep setup cost: 0 ms)\n' "hsum ingest --dry-run" "$ING_MS"
fi

if [ "$FORMAT" = json ]; then
  # Trailing empty object keeps the array valid after the comma-terminated rows.
  echo '  {}'
  echo '  ]'
  echo '}'
else
  echo
  echo "Reading these numbers:"
  echo "  BYTES is the payload an agent pays context tokens for. Lower is cheaper,"
  echo "  but cheap and wrong is not a win: only the hsum rows carry a citation"
  echo "  that resolves to a verified revision."
  echo
fi
