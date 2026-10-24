# Mac M2 / 16 GB recovered 100k review — 2026-10-24

Candidate: `b6591b7405320c924dc341111006d8d87b1e0c8a`.

**Failed qualification, complete 2,250-observation query protocol.**

All three fresh processes complete five warmup passes and 30 measured passes
over the frozen 25-query order. The recorded exit is 1. Every raw observation,
per-run summary and class SLO, total-p95 CV, manifest/corpus/helper hashes,
source/binary/setup/recovery binding, cold record, RSS and storage arithmetic
is rechecked with the unchanged existing harness by `review.py`.

The source checkout is clean at `b6591b7`; the retained executable hash matches
the report, setup and recovery receipt. Model verification and full Doctor
before/after recovery are recorded separately. Original failure bytes and the
failed baseline preparation remain unchanged.

## Warm query gates

| Class | Strict threshold | Run 1 p95 | Run 2 p95 | Run 3 p95 | Overall |
| --- | --- | --- | --- | --- | --- |
| Identifier | < 100 ms | 522 ms | 299 ms | 211 ms | fail |
| Ordinary lexical | < 100 ms | 502 ms | 400 ms | 371 ms | fail |
| Exact fallback | < 750 ms | 322 ms | 255 ms | 225 ms | pass |
| Hybrid without fallback | < 500 ms | 655 ms | 549 ms | 470 ms | fail |

Total server p95 values are **560 / 423 / 357 ms**. Population CV is
**18.928468601%**, failing the <= 10% limit. Exact fallback passes every run;
hybrid passes only run 3, which does not satisfy the every-run requirement.
Every measured request returns evidence and stops at `limit_reached`.

## Variability diagnosis from the retained samples

| Run | Passes 1–10 server p95 | Passes 11–20 server p95 | Passes 21–30 server p95 |
| --- | --- | --- | --- |
| 1 | 645 ms | 528 ms | 552 ms |
| 2 | 525 ms | 442 ms | 332 ms |
| 3 | 329 ms | 389 ms | 369 ms |

The distribution changes within as well as between processes, especially in
the first 20 passes of run 2. Run 3 still fails identifier and ordinary lexical
SLOs, so discarding earlier runs would not repair qualification. All 2,250
observations remain included and thresholds are unchanged.

Exact, lexical and vector times all shift; this is broader than model loading.
For identifier queries the median unassigned wall remainder is 94 / 88 / 86 ms
across the three runs, while median exact time is 97 / 88 / 26 ms. Ordinary
lexical medians have remainder 93 / 86 / 85 ms and lexical stage
86.5 / 64 / 20.5 ms. Hybrid remainders are 217 / 201 / 190 ms. These remainders
are computed without double-counting overlapping materialization/fallback
timers; they are profiling targets, not attribution to an individual stage.

No CPU frequency, thermal, background-process or page-fault telemetry was
recorded. Cache state or machine contention are hypotheses, not established
causes. Do not repeat or relax the frozen run to obtain a preferred sample.
Profile the broad query, exact lookups and application open/preflight work
on a reduced fixture before a further full protocol.

## Separate cold observations

| Run | Process start | OS-cache lexical server / client | First model-use query server / client | First hybrid server / client |
| --- | --- | --- | --- | --- |
| 1 | 11.607 ms | 706 / 1048.747 ms | 1230 / 1244.088 ms | 593 / 595.833 ms |
| 2 | 24.158 ms | 730 / 963.936 ms | 1294 / 1308.092 ms | 711 / 714.784 ms |
| 3 | 12.438 ms | 672 / 937.469 ms | 1327 / 1336.636 ms | 538 / 540.773 ms |

Each process records successful actual macOS cache eviction through the
tracked helper. Its digest matches the candidate. Cold queries and first model
use are separate from warm samples; first model-use query total is not isolated
inference time. No cold-cache SLO is invented.

## Memory, storage, and recovered preparation

| Metric | Value |
| --- | --- |
| Run 1 peak process-tree RSS | 814,710,784 bytes (776.969 MiB) |
| Run 2 peak process-tree RSS | 820,150,272 bytes (782.156 MiB) |
| Run 3 peak process-tree RSS | 815,267,840 bytes (777.500 MiB) |
| Managed index including WAL/SHM | 2,002,290,968 bytes |
| Logical live content | 150,000,000 bytes |
| Logical history | 0 bytes |
| Active vector payload | 153,600,000 bytes |
| Cached vector payload | 153,600,000 bytes |
| Total logical vector payload | 307,200,000 bytes |
| Cached membership recovery | 39.651 s, 100,000 inputs reused, zero inferred/cached anew |
| Fresh-candidate ingest/inference throughput | unavailable; deliberately null and unqualified |

RSS sampling has no errors; database/WAL/SHM sizes sum to managed bytes.
The recovered setup is explicit about reusing inference from failed baseline
`1ce11d0`. The unchanged `setup_and_ingest_throughput_recorded` flag is false.
No fresh-preparation pass can be inferred from the successful recovery or
query observations. Memory and storage observations do not invent new SLOs.

## Scope and remaining gates

The failed flags are exactly `all_class_slos`, `total_p95_cv_at_most_10_percent`
and `setup_and_ingest_throughput_recorded`. The protocol, count, cold-method
and RSS flags pass. No samples or failures were discarded.

Linux baseline `1ce11d0`, Linux stress `3827248`, and this Mac recovery
`b6591b7` are different candidates. Their side-by-side evidence is a status
comparison, not a platform speed comparison or a causal repair estimate.
Same-candidate two-platform qualification, fresh setup throughput, performance
repair/profiling and independent source review remain open. This report makes
no retrieval-quality or token-cost superiority claim.
