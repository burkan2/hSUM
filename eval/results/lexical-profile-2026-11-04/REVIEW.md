# Bounded lexical frontier — November 4, 2026

Status: local contributor checks, 35 optimized search tests and release smoke
pass. Independent source review approved the bounded implementation. Linux
paired diagnostics and full native qualification remain required. This report
does not supersede candidate `5ca552c`'s retained Linux qualification failure.

The stack sample of the unchanged `5ca552c` binary identified SQLite temporary
table processing and page access among the remaining broad-query costs. The
sample perturbs timings and is diagnostic only. Cache-size and in-memory-temp
experiments gave small gains; FTS compaction of a separate disposable clone
took 2.40 seconds and also gave a small gain. Neither change is selected for
production. Their measurements and prototype source remain retained.

A Rust-frontier SQL prototype returned identical top-500 IDs and score bits
with medians 64.50 / 45.20 / 63.10 ms across its A/B/A phases (ten samples each,
all retained). Production adds resource limits, exact fallback for ambiguous
or malformed tie metadata, and persistent cancellation checkpoints; see
`docs/lexical-ranking.md` for the implemented bounds and snapshot behavior.

Tests were written before the frontier implementation and lazy-cursor plan
helper. Retained red logs show those missing symbols. The initial contributor
check then found a complex return type; a named result struct fixed it. Final
checks include full contributor gates, 35 optimized search tests, the additional
inside-metadata cancellation case, and actual release smoke. No test or gate
threshold was weakened. Independent review checked the algorithm, capacity,
metadata bounds, ambiguous ties, cancellation identity and lazy body checks.
Source whitespace checks pass. Retained raw compiler/test logs preserve four
trailing blank lines and one whitespace-only linker diagnostic line.

## Full-query product diagnostic

The unchanged diagnostic performs two warmup and ten measured passes over all
25 frozen queries for each phase. All **900** returned result objects and
explanations match the retained complete baseline arrays; 750 observations are
measured. Author and independent review recomputed every response digest;
independent review also recomputed all 75 query summaries and checked order,
candidate bounds, source/harness/query identities and both binary hashes.

| Ordinary-02 total latency | Baseline before | Candidate | Baseline after |
| --- | ---: | ---: | ---: |
| Median (ms) | 88 | 65 | 125 |
| p95 / worst (ms) | 122 | 113 | 231 |
| Peak process-tree RSS (bytes) | 675,364,864 | 762,494,976 | 826,064,896 |

The final control is substantially slower across classes. The surrounding
controls drift, so these observations do not support a clean gain estimate or
an SLO pass. All observations are preserved; no repeat replaces them. The next
step is a paired Linux diagnostic on a single runner, recording its CPU model,
followed by unchanged full qualification on both platforms for one candidate.

Baseline binary: `8f6867f4ede78ae89c6baa6e5183910a8a260908d7ca2f642dda1ba10ad40b59`
from the clean frozen `5ca552c` build. Candidate binary:
`f7ed0302e2b66a437b3332ab002148eed3c6f0698b160d7fe5595c76dc401b70`.
The candidate was built from the reviewed pending source on parent `1124336`;
the receipt binds source hashes. Diagnostic fixture: disposable recovered-index
clone under `2026-10-31-performance/home`, not a native fresh-ingest trial.

The previous release-workflow checkpoint `1124336` passed all seven standard
workflows; `prior-checkpoint-ci.json` retains their run identities. Its production
retrieval code remains `5ca552c`, so those checks do not qualify this new frontier.

## Disk evidence

Build-cleanup receipts record removal of rebuildable frozen-5ca target files
and development dependency/build caches after checks. Saved release/debug
binaries were hash-checked and preserved, as were all source checkouts, reports,
crash snapshots, recovered indexes and fresh qualification indexes. About 6 GiB
remained afterward. The user subsequently confirmed 9 GiB free for fresh Mac
qualification; availability is checked again before starting preparation.
