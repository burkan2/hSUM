# Performance qualification checkpoint — 2026-10-23

**Q04 remains failed; Linux report-only Q05 passes on `3827248`.**
Reviewed through 2026-10-24: both Linux baseline and Mac recovered-index runs
complete all 2,250 observations. Both fail latency; Mac also fails variability
and lacks fresh-candidate preparation throughput. The original Mac preparation
SIGBUS remains retained. There is no same-candidate cross-platform pass.
This work makes no retrieval-quality or token-cost superiority claim.

## Candidate and evidence boundaries

| Work | Candidate | Current disposition |
| --- | --- | --- |
| Linux frozen 100k | `1ce11d00bf8cad949c12a9c496ab7dba3675717f` | [40004812595](https://github.com/burkan2/hSUM/actions/runs/40004812595) completed all 2,250 observations; identifier and ordinary lexical latency fail, fallback/hybrid and CV pass. [Reviewed evidence](results/retrieval-scale-40004812595-1ce11d0/REVIEW.md) |
| Mac M2 / 16 GB frozen 100k | `1ce11d00bf8cad949c12a9c496ab7dba3675717f` | Preparation failed (`prepare.exit = 2`, child SIGBUS/-10); launcher refused measurement. [Crash/lock diagnosis](MACOS_100K_SIGBUS_DIAGNOSIS.md) |
| Native million-passage prior failure | `1ce11d00bf8cad949c12a9c496ab7dba3675717f` | [39981593244](https://github.com/burkan2/hSUM/actions/runs/39981593244) fails lexical recovery after cancellation; raw failure retained |
| Recovery cursor repair | `38272481bfbc9bb831ac05304135f3bdc54dda05` | Focused red/green regression, complete contributor gate, optimized build/smoke, paired results, and small cancellation pass |
| Native million-passage rerun | `38272481bfbc9bb831ac05304135f3bdc54dda05` | [40008791029](https://github.com/burkan2/hSUM/actions/runs/40008791029) passed the frozen report-only Linux protocol; [reviewed raw evidence](results/retrieval-stress-40008791029-3827248/REVIEW.md) |
| WAL descriptor repair | `b6591b7405320c924dc341111006d8d87b1e0c8a` | Product regression fails before and passes after; final full local gate, optimized smoke, paired controls and small real-model cancellation pass |
| Mac recovered 100k | `b6591b7405320c924dc341111006d8d87b1e0c8a` | All 2,250 observations reviewed. Identifier/ordinary/hybrid latency and CV fail; fallback passes. Fresh ingest/inference throughput intentionally unqualified. [Reviewed evidence](results/retrieval-scale-macos-recovery-b6591b7/REVIEW.md) |
| Native standard gates for WAL repair | `99d51f9f928c790a4f968dbc0f33e95e10bc13f0`, identical production sources to `b6591b7` | All seven workflows pass, including the lock regression in both Linux/macOS contributor gates. [Retained logs and source comparison](results/native-ci-99d51f9/REVIEW.md). Author review; independent source review remains open |
| Model portability | `1ce11d00bf8cad949c12a9c496ab7dba3675717f` | [39981587444](https://github.com/burkan2/hSUM/actions/runs/39981587444), retry 2, passed. All eight retained file hashes verified; local numerical recomputation exactly reproduces the retained comparison |

The [repair review receipt](results/lexical-cursor-3827248/review-receipt.json)
binds before/after candidates, optimized binary hash, source hash, fixture
cardinalities, and verification. Raw results and validation logs are separated
under `before-1ce11d0/` and `after-3827248/`. Native SQL probes explicitly label
their reduced/synthetic scope. See the
[Q05 diagnosis](RETRIEVAL_STRESS_Q05_DIAGNOSIS.md#recovery-ranking-repetition--2026-10-23)
for the cause, measured costs, and repair limits. Author source review is
recorded; independent review is still required.

## Frozen 100k review gates

Each platform must independently complete three fresh MCP processes, each
with five unmeasured full-query passes and 30 measured passes of all 25
queries: **750 observations per process, 2,250 per platform**. The nearest-rank
percentiles, fixed query order, corpus fingerprint, model pin, binary/setup
binding, retrieval-class checks, and all stored-field/content integrity checks
remain unchanged.

Every warm run must have server-total class p95 strictly below:

| Class | Threshold |
| --- | --- |
| Identifier | 100 ms |
| Ordinary lexical | 100 ms |
| Exact fallback | 750 ms |
| Hybrid without fallback | 500 ms |

The coefficient of variation of the three total-p95 values must be at most
10%. Review every raw observation and recompute per-class summaries, all three
run p95s, CV, and qualification flags using the existing harness functions.
Report cold process startup, OS-cache lexical search, first model load, and
first hybrid separately. Review peak process-tree RSS and sampler errors,
live/history/vector/managed storage bytes, and ingest/re-embedding throughput.
A written report with failed SLO/CV gates remains a failure. A request timeout
or incomplete sequence cannot supply the missing observations.

Linux now supplies a complete failed report: identifier p95 141/137/129 ms
and ordinary lexical 286/284/283 ms miss their 100 ms gates; exact fallback
147/149/161 ms and hybrid 273/277/264 ms pass. Total-p95 CV is 1.411687427%.
All 2,250 observations, summaries, SLO/CV flags, cold records, RSS, and storage
were reviewed using the existing harness; the artifact ZIP digest matches
GitHub metadata. Full details and provenance limits are in the linked Linux
review. The Mac recovered-index report is compared below as a status record,
with its different source and preparation explicit. No platform speed or
causal repair comparison follows. No pass is inferred from preparation,
portability, SQL probes, or small smoke tests. Keep the prior completed Linux
failure (`39638597258`, all 2,250 observations but every class SLO failed) in the
[Q04 diagnosis](RETRIEVAL_SCALE_Q04_DIAGNOSIS.md).

## Completed Mac recovery protocol — 2026-10-24

The separately bound `b6591b7` recovery passes full Doctor before/after,
verifies the pinned model and commits 100,000 memberships using 100,000 cached
inputs in 39.651 s. That is cache reuse, not fresh inference. Fresh-candidate
ingest seconds and throughput remain null in the setup receipt.

| Gate | Linux baseline `1ce11d0` | Mac recovery `b6591b7` |
| --- | --- | --- |
| Observations | 2,250 complete | 2,250 complete |
| Identifier p95, runs 1/2/3 (<100 ms) | 141 / 137 / 129; fail | 522 / 299 / 211; fail |
| Ordinary lexical p95 (<100 ms) | 286 / 284 / 283; fail | 502 / 400 / 371; fail |
| Exact fallback p95 (<750 ms) | 147 / 149 / 161; pass | 322 / 255 / 225; pass |
| Hybrid p95 (<500 ms) | 273 / 277 / 264; pass | 655 / 549 / 470; fail |
| Total-p95 population CV (<=10%) | 1.411687427%; pass | 18.928468601%; fail |
| Actual cache eviction and separate cold records | recorded | recorded |
| Peak process-tree RSS, runs 1/2/3 | 545,128,448 / 556,302,336 / 536,739,840 bytes | 814,710,784 / 820,150,272 / 815,267,840 bytes |
| Managed index, including WAL/SHM | 2,002,340,264 bytes | 2,002,290,968 bytes |
| Logical live / history / active+cached vectors | 150,000,000 / 0 / 307,200,000 bytes | 150,000,000 / 0 / 307,200,000 bytes |
| Fresh-candidate ingest/inference throughput | recorded | unavailable; recovered cache |

All raw observations, class paths, fixed order, per-run summaries, frozen
SLO/CV flags, setup/binary/source/model/corpus/helper binding, cold records,
RSS and storage were reviewed with the existing harness functions. See each
platform's linked review for exact separate cold measurements and provenance
limits. Mac's three failed flags are exactly `all_class_slos`,
`total_p95_cv_at_most_10_percent`, and
`setup_and_ingest_throughput_recorded`; the other protocol/cold/RSS flags pass.

Mac timing changes within and between runs: the server p95 in consecutive
ten-pass blocks is 645/528/552, 525/442/332, and 329/389/369 ms. All samples
remain included. No CPU frequency, thermal, background-process or page-fault
telemetry exists to establish an environmental cause. Run 3 still fails both
100 ms classes. Another full run without a diagnosed repair is not justified.

## Mac launch and authentication history

Live evidence root:
`/Users/b.k./.cache/hsum-qualification/2026-10-23-100k-lbse5n1f/`.
`RUN-MACOS.md` retains the exact manual command. The launched
`run-macos-qualification.command` uses the frozen checkout and binary,
required successful preparation and used the tracked cold-cache helper under
`caffeinate`. The user completed local sudo authentication, but preparation
failed and the guarded launcher exited without measuring. That Terminal
authorization must not be assumed valid for a future attempt. No credentials
were requested in chat.

The launcher is configured to retain `measurement.started`, `measurement.log`,
and `measurement.exit`, refuse duplicate measurement, and retain failed exits.
None of those measurement files was created because preparation failed.
It never uses `--allow-missing-cold-cache`. The failed baseline binary/index remain retained and were not swapped. Captured launch,
script, and status receipts are in
[the baseline evidence folder](results/lexical-cursor-3827248/before-1ce11d0/).
Preparation ran on AC power. There was no Mac measurement phase to review.

That paragraph describes the failed **baseline** launcher only. The separate
recovery launcher at
`/Users/b.k./.cache/hsum-qualification/2026-10-24-macos-recovery-b6591b7/`
was subsequently authenticated locally by the user and completed all three
processes. Its retained `measurement.exit` is **1**, and its failed report,
launch/setup/recovery receipts and logs are copied unchanged into the linked
Mac review folder. No duplicate launcher or missing-cold-cache override ran.
No future sudo authorization is assumed.

## Remaining work

1. Independent source review of the cursor and descriptor repairs remains
   required. Both native contributor gates now execute and pass the descriptor
   regression on identical production sources. The initial ledger failure is
   retained; the corrected 222/316 (70.3%) headline is separately verified.
2. Any future full qualification needs fresh setup/inference throughput and
   both platforms on one recorded candidate. Cached recovery cannot close
   that gate, and none of these completed failed runs should be repeated just
   to obtain preferred samples.
3. Profile the remaining Linux 100k broad ranking and orchestration cost before
   another expensive 100k rerun. Identifier and ordinary lexical limits remain
   unchanged; the report-only stress pass does not repair those failures.
   A [reduced Mac connection-lifetime profile](results/query-open-profile-b6591b7/REVIEW.md)
   now identifies roughly 80 ms of repeated WAL attachment/reconstruction
   overhead via an A/B/A control. Broad ranking still misses 100 ms even when
   that overhead is removed. This is a repair target, not a product workaround
   or native Linux attribution; snapshot/replacement/lock safety needs a
   focused regression before changing connection lifetimes.
4. Keep candidates separate. Linux report-only stress passes on `3827248`;
   Linux 100k fails and Mac preparation crashes on `1ce11d0`; recovered Mac
   query measurement fails on `b6591b7`. No candidate is
   stable-release qualified; retrieval-quality and token-cost superiority
   remain separate claims with separate evidence requirements.

The finite native-run follow-up has completed its report review. No benchmark
is waiting for authentication or still running. Remaining gates above are
explicit failures or further repair/review work, not pending reports.
