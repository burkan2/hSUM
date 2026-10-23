# Performance qualification checkpoint — 2026-10-23

**Q04 remains failed; Linux report-only Q05 now passes on `3827248`.**
Linux 100k completed all 2,250 observations but fails identifier and ordinary
lexical latency. Mac preparation crashed with SIGBUS before measurement;
there is no complete Mac report or cross-platform qualification.
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
| Mac recovered 100k | `b6591b7405320c924dc341111006d8d87b1e0c8a` | Separate clone passes full Doctor before/after and commits complete vectors using 100,000 cached inputs. Frozen query launcher awaits local sudo; fresh ingest/inference throughput intentionally unqualified |
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
review. Mac preparation failed before measurement, so no cross-platform pass or
comparison is claimed. No pass is inferred from preparation, portability, SQL probes, or
small smoke tests. Keep the prior completed Linux
failure (`39638597258`, all 2,250 observations but every class SLO failed) in the
[Q04 diagnosis](RETRIEVAL_SCALE_Q04_DIAGNOSIS.md).

## Mac launch and authentication

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

## Remaining work

1. Descriptor repair `b6591b7` passes the final full local gate and focused
   cross-process regression. Review the fresh native CI result separately;
   the initial CI run failed on the previously stale ledger percentage before
   Rust tests, and that documentation metric is now corrected and verified.
   Full details and retained evidence are in the Mac SIGBUS diagnosis.
2. Authenticate in the newly opened recovery Terminal and keep it open. The
   copied index has passed full Doctor and committed complete memberships.
   Review its 2,250-observation cold/warm report after completion, preserving
   all failed gates. Its correctly bound recovery setup leaves fresh ingest
   throughput null, so the unchanged harness must not mark full qualification
   true. Keep `b6591b7` recovery measurements separate from `1ce11d0` baseline
   and `3827248` Linux stress. Do not manufacture fresh throughput or bypass
   the cold-cache helper.
3. Profile the remaining Linux 100k broad ranking and orchestration cost before
   another expensive 100k rerun. Identifier and ordinary lexical limits remain
   unchanged; the report-only stress pass does not repair those failures.
4. Keep candidates separate. Linux report-only stress passes on `3827248`;
   Linux 100k fails and Mac preparation crashes on `1ce11d0`. No candidate is
   stable-release qualified; retrieval-quality and token-cost superiority
   remain separate claims with separate evidence requirements.

An active follow-up named **Finish hSUM performance qualification** is attached
to this chat. It continues the Mac lock-hazard diagnosis and focused repair before another
large run, preserves completed Linux reviews, stays quiet while nothing
actionable changes, and finishes the outstanding Mac protocol with the existing
harness.
Automation ID: `finish-hsum-performance-qualification`.
