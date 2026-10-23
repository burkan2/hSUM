# Performance qualification checkpoint — 2026-10-23

**Q04 and Q05 remain unqualified.** Existing native runs continue; no new
completed 100k or million-passage report is available at this checkpoint.
This work makes no retrieval-quality or token-cost superiority claim.

## Candidate and evidence boundaries

| Work | Candidate | Current disposition |
| --- | --- | --- |
| Linux frozen 100k | `1ce11d00bf8cad949c12a9c496ab7dba3675717f` | [40004812595](https://github.com/burkan2/hSUM/actions/runs/40004812595) is preparing embeddings; no uploaded report yet |
| Mac M2 / 16 GB frozen 100k | `1ce11d00bf8cad949c12a9c496ab7dba3675717f` | Preparation is active; no setup/exit receipt yet. Local sudo authentication completed; Terminal launcher automatically waits for preparation and runs measurement |
| Native million-passage prior failure | `1ce11d00bf8cad949c12a9c496ab7dba3675717f` | [39981593244](https://github.com/burkan2/hSUM/actions/runs/39981593244) fails lexical recovery after cancellation; raw failure retained |
| Recovery cursor repair | `38272481bfbc9bb831ac05304135f3bdc54dda05` | Focused red/green regression, complete contributor gate, optimized build/smoke, paired results, and small cancellation pass |
| Native million-passage rerun | `38272481bfbc9bb831ac05304135f3bdc54dda05` | [40008791029](https://github.com/burkan2/hSUM/actions/runs/40008791029) is running; no qualification pass claimed |
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

Neither platform currently supplies those completed artifacts. No class
latency, CV, cold, memory, or storage pass is inferred from preparation,
portability, SQL probes, or small smoke tests. Keep the prior completed Linux
failure (`39638597258`, all 2,250 observations but every class SLO failed) in the
[Q04 diagnosis](RETRIEVAL_SCALE_Q04_DIAGNOSIS.md).

## Mac launch and authentication

Live evidence root:
`/Users/b.k./.cache/hsum-qualification/2026-10-23-100k-lbse5n1f/`.
`RUN-MACOS.md` retains the exact manual command. The launched
`run-macos-qualification.command` uses the frozen checkout and binary,
requires successful preparation, keeps its own Terminal authorization valid,
and runs the tracked cold-cache helper under `caffeinate`. The user completed the local Terminal sudo prompt; process inspection confirms
the authorization keepalive is active. No credentials were requested in chat.

The launcher retains `measurement.started`, `measurement.log`, and
`measurement.exit`, refuses duplicate measurement, and keeps failed report/exit
status. It never uses `--allow-missing-cold-cache`. The source/binary are not
swapped to the repair while this baseline is in progress. Captured launch,
script, and status receipts are in
[the baseline evidence folder](results/lexical-cursor-3827248/before-1ce11d0/).
The Mac is on AC power. Avoid heavy local work during the measurement phase;
all local build/probe work at this checkpoint finished during preparation.

## Remaining work

1. Keep the authenticated Mac Terminal open; finish baseline preparation and
   both 2,250-observation runs. Retain failed exits and complete reports.
2. Download immutable Linux artifacts and review both platforms independently
   against every frozen gate above; publish the reviewed comparison only after
   both full reports exist. Do not cancel the current run to replace its candidate.
3. Review native stress rerun `40008791029`, including eight cancellations,
   two real paused workers, the complete no-late-response interval, lexical and
   hybrid recovery, all scale probes, memory, and storage amplification.
4. Keep repair-candidate evidence separate. The repair has no frozen 100k
   qualification yet; any subsequent measurements need their own correctly
   bound setup/report and unchanged integrity checks. Neither candidate is
   qualified for stable publication from this checkpoint.

An active follow-up named **Finish hSUM performance qualification** is attached
to this chat. It checks the long native runs periodically, stays quiet while
nothing actionable changes, reviews completed artifacts with the existing
harness, and pauses once the requested reports and remaining gates are recorded.
Automation ID: `finish-hsum-performance-qualification`.
