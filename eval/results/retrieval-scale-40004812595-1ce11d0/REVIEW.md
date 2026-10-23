# Linux frozen 100k review — 2026-10-23

Candidate: `1ce11d00bf8cad949c12a9c496ab7dba3675717f`. Run [40004812595](https://github.com/burkan2/hSUM/actions/runs/40004812595).

**Disposition: failed latency qualification, complete 2,250-observation report.**

The artifact ZIP SHA-256 matches GitHub metadata. All query/pass order, class
paths, source/setup/binary bindings, manifest/corpus/helper hashes, every raw
stage number, all per-run summaries and SLO flags, and total-p95 CV were
reviewed with the unchanged existing harness. No thresholds were relaxed.

| Warm class | Strict threshold | Run 1 p95 | Run 2 p95 | Run 3 p95 | Result |
| --- | --- | --- | --- | --- | --- |
| Identifier | < 100 ms | 141 ms | 137 ms | 129 ms | fail |
| Ordinary lexical | < 100 ms | 286 ms | 284 ms | 283 ms | fail |
| Exact fallback | < 750 ms | 147 ms | 149 ms | 161 ms | pass |
| Hybrid without fallback | < 500 ms | 273 ms | 277 ms | 264 ms | pass |

All runs have 750 samples, five warmup passes and 30 measured passes. Every
measured request returns evidence and stops at `limit_reached`. Total p95s are
`277 / 275 / 268 ms`; population CV is **1.411687427%**, passing the <= 10% gate.

## Cold observations

Cold values are separate from the 2,250 warm observations. These are query
totals, including first model use; they are not isolated inference timings.

| Run | Process start | OS-cache lexical server / client | First model-use query server / client | First hybrid server / client |
| --- | --- | --- | --- | --- |
| 1 | 3.212 ms | 305 / 658.845 ms | 1011 / 1021.330 ms | 537 / 539.848 ms |
| 2 | 3.664 ms | 325 / 674.416 ms | 1044 / 1056.134 ms | 607 / 609.948 ms |
| 3 | 3.932 ms | 323 / 683.088 ms | 1045 / 1057.821 ms | 585 / 588.183 ms |

All three runs recorded successful cache eviction through the source-bound
`drop-cache-linux.sh`; its SHA-256 is in the raw report and verified against
the candidate. The warm hybrid SLO is not applied to the separate cold values.

## Memory, storage, and preparation

| Metric | Observed value |
| --- | --- |
| Peak process-tree RSS, run 1 | 545,128,448 bytes (519.875 MiB) |
| Peak process-tree RSS, run 2 | 556,302,336 bytes (530.531 MiB) |
| Peak process-tree RSS, run 3 | 536,739,840 bytes (511.875 MiB) |
| Managed index, including WAL and SHM | 2,002,340,264 bytes |
| Logical live content | 150,000,000 bytes |
| Logical retained history | 0 bytes |
| Logical active vector payload | 153,600,000 bytes |
| Logical cached vector payload | 153,600,000 bytes |
| Total logical vector payload | 307,200,000 bytes |
| Lexical initialization/ingest | 32.594 s; 3068.075 passages/s |
| Re-embedding | 4659.190 s; 21.463 passages/s |

RSS samplers have no errors. Physical file sizes sum to the recorded managed
bytes. There are 100,000 cached vectors and no logical history; cached plus
active vector payload is 307,200,000 bytes. These are recorded observations,
not a new memory/storage SLO or proof of million-passage boundedness.

## Remaining diagnosis and scope

`ordinary-02` examines only 50 lexical candidates in its recorded observations.
Its first warm sample spends 199 ms in lexical retrieval and 282 ms total.
`identifier-01` spends 31 ms exact, 7 ms lexical, and 122 ms total in its first
sample. Per-query distributions and the unassigned wall-time remainder are
retained in `review.json`; body materialization and exact-fallback timers
overlap with other stages and must not be added twice. The roughly 80 ms
non-retriever remainder is a profiling target, not an established attribution
to a particular open/snapshot/checksum operation.

The adaptive cursor repair `3827248` was motivated by repeated ranking at
500-candidate stress recovery. It does not by itself explain or qualify these
50-candidate 100k failures. Profile the remaining broad ranking and application
orchestration before another expensive 100k run; preserve every integrity check.

The executable and full index are absent from the upload, so binary/model
and stored-field integrity are bound to the frozen harness execution and its
recorded checks, not independently rerun against missing binary/index bytes.
The eight frozen qualification flags are recomputed exactly; only
`all_class_slos` is false. Raw failed exit output and both JSON reports remain
unaltered. The Mac report and a reviewed cross-platform comparison are still
outstanding. This report makes no retrieval-quality or token-cost superiority
claim.
