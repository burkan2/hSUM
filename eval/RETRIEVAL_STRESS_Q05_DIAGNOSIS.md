# Q05 cancellation preflight diagnosis

Date: 2026-10-21. Disposition: local repairs verified; native stress qualification open.

Linux run [39510579822](https://github.com/burkan2/hSUM/actions/runs/39510579822)
at `caf2f19` completed million-passage ingestion, 100 metadata generations, and
re-embedding, then failed with `expected two real model workers, found []`.
The previous invalid 60,000 ms MCP timeout was already repaired. This run
does not have a completed stress report.

Before starting model inference, each semantic request validates the entire
active vector membership. The previous SQL iterated active passages and
opened a sqlite-vec point query for each row. A native SQLite 3.51.3 /
sqlite-vec 0.1.7 probe with 100,000 synthetic vector memberships measured
0.937 seconds for that query and 0.286 seconds for scanning vectors first and
looking up active passages. These isolated, single-request timings establish
the query cost; they do not prove the cause of every worker-start failure or
predict million-passage concurrent latency.

Review of the pinned sqlite-vec source found that point queries eagerly read
vector payloads, while the faster full scan reads them only if requested.
Adding an explicit payload read removes the apparent gain: a repeated 100k
probe measured 0.957 seconds for the original path and 0.956 seconds for the
scan with payload checks. The scan optimization was therefore rejected. The
original point-query path, cardinality checks, and payload-error detection
remain in place. Million-passage preflight cost still needs a safe solution.

The shipped correctness repair changes the source-partition comparison to
reject NULL. The prior SQL `!=` comparison silently accepted it. The regression
fails on NULL before the repair and passes afterward, alongside missing,
extra, replaced, and wrong-source rows. All 25 vector-storage tests pass,
including old-reader snapshots, restore, pruning, and physical forgetting.

The shared search preflight now invokes its connection observer before reading
the snapshot or validating vectors. MCP uses that observer to install the
SQLite interrupt handle, so cancellation and the route deadline can interrupt
this work. Previously only the later retrieval connection was registered.
A regression checks cancellation at the preflight boundary.

The stress harness now reports a response received before both model workers
can be paused, including a bounded error packet. It still requires two real
paused workers, eight cancellations, no late responses, and lexical/hybrid
recovery. Its 15 tests pass. The full Linux stress protocol must be repeated
before Q05 can be marked complete.

The optimized candidate with SHA-256
`1e770f1348321d19e5f0cf4c0a1273d8df47abac1ed44a08e55634ffff924170`
also passes release smoke and a small real-model cancellation run: two workers
paused, eight requests cancelled, no late responses, and successful lexical
and hybrid recovery. The full local contributor gate passed before the scan
proposal was removed; all 25 vector tests were rerun after retaining the
original point-query path. These checks do not qualify million-passage load.

## Native rerun — reviewed 2026-10-22

Run [39654208279](https://github.com/burkan2/hSUM/actions/runs/39654208279)
at `d5cd6b8` again completes million-passage ingest, metadata generations, and
re-embedding, then fails the cancellation-storm setup. The improved diagnostic
now records the actual early MCP response: **TIMEOUT / REQUEST_DEADLINE**
before two real model workers could be paused. The
[unaltered harness log](results/q05-39654208279-harness.log) is retained.
No complete stress JSON report was produced.

This narrows the failure to request work before the required worker state.
The interrupted preflight now returns its typed deadline rather than leaving
the harness with only an empty worker list. It does not resolve preflight
latency or prove million-passage cancellation recovery. The 100k Q04 report
also shows substantial hybrid time outside embedding and vector-search
telemetry. Profile and repair that work without removing payload-integrity
checks before another expensive native run.

## Block-level vector validation — 2026-10-23

The pinned sqlite-vec 0.1.7 implementation opens the vector blob for every
point lookup and prepares a source-partition query for every row. Its shadow
storage groups 1,024 vectors in a block. The repaired membership query reads
each referenced block once, then joins its materialized type/length metadata
to the active-passage, row-location, and source-partition tables. Cardinality,
missing/replaced rows, missing blocks, NULL/wrong partitions, payload types,
and offset bounds remain checked. The physical `_rowid_` is deliberate:
sqlite-vec's declared `rowid` column is not an alias for the SQLite blob rowid.
This query depends on the pinned backend layout and must be reviewed when
that dependency or storage format changes.

`length(substr(vectors, 1))` forces a complete payload read before recording
its byte count. Plain `length(vectors)` would inspect only the stored length
and is insufficient; see SQLite's [function semantics](https://www.sqlite.org/lang_corefunc.html).
A new test breaks an overflow-page link near the end of a stored block:
the header-only length query still succeeds, while membership validation
fails. The corruption regression also rejects NULL, negative, noninteger,
and out-of-range offsets, missing storage, truncated/non-BLOB payloads, and
a changed physical rowid. The NULL-offset case fails against the old code,
which silently interpreted that offset as zero.

The [raw diagnostic report](results/vector-block-2026-10-23-macos-diagnostic.json)
records the exact native SQL, probe source, source hashes, all timings, and
corruption results for 100,000 synthetic normalized vectors. Original-query
times are 911–930 ms; the production query takes 112–118 ms. It also retains
the unsuccessful lexical cache/document-scope experiments; neither is in
production. These in-memory probes are not the frozen retrieval protocol,
do not qualify cold storage or concurrent million-passage requests, and do
not close Q04 or Q05.

The optimized binary
`a3d45f6a5351d2a3d9ccc47994fe9b4aefa9407c353495c45dee0b7a76c88337`
passes CLI/MCP release smoke and the small real-model cancellation protocol:
two workers paused, eight requests cancelled, no late responses, and lexical
and hybrid recovery. Eighteen paired fresh-process lexical/semantic/hybrid
controls preserve ordered citations, scores, and hashes; every returned
content matches its source byte window. The small fixture was explicitly
migrated to schema 5 with a verified schema-4 backup. These controls are
correctness evidence, not latency qualification; they ran alongside local
contributor checks. The million-passage native rerun remains required.
The complete local `cargo xtask check` passes, including all 28 vector-storage
tests, strict Clippy, generated references, and frozen harness checks.

## Native rerun and source-partition work — 2026-10-23

Run [39888790398](https://github.com/burkan2/hSUM/actions/runs/39888790398)
at `bc26f399ae24547b33cac9aec877739536902a8c` completes million-passage
setup, metadata generations, and re-embedding, but still fails cancellation
setup. Request 8 returns **TIMEOUT / REQUEST_DEADLINE** with operation
`blocking_admission` before the harness confirms two paused model workers.
The [unaltered harness log](results/q05-39888790398-harness.log) and
[runner capacity](results/q05-39888790398-runner-capacity.txt) are retained.
No complete stress JSON report exists. The queue error establishes that the
admitted requests did not release capacity in time; it does not identify
which preflight stage consumed their budget or how many workers were paused.

The next bounded optimization moves canonical source UUID formatting out of
the per-passage membership loop. A materialized map uses distinct source IDs
from the existing covering index, and the original strict partition comparison
joins that map. It uses no persistent cache, schema migration, or new dependency.
Block payload reads and all membership/offset checks remain. A two-source test
rejects a block reassigned to the other valid source; malformed uppercase and
unhyphenated partition strings remain rejected.

The [raw diagnostic report](results/vector-source-2026-10-23-macos-diagnostic.json)
contains the before/after SQL, probe source, source hashes, timings, and discarded
lexical/identifier experiments. A counted-function assertion verifies 100,000
UUID conversions before the change and 64 afterward. Native SQLite/sqlite-vec
probes on 64 source partitions measure:

| Synthetic fixture | Before | After |
| --- | --- | --- |
| 100,000 vectors, one connection | 117–127 ms | 55–58 ms |
| 1,000,000 vectors, one connection | 1.29–1.44 s | 0.72–1.09 s |
| 1,000,000 vectors, four concurrent connections, batch completion | 5.38–5.84 s | 2.58–2.62 s |

These use shared in-memory storage on the local Mac. They do not establish
cold-disk behavior, equivalence to independent MCP connections, or completion
of the frozen Q05 workload. Q04/Q05 remain open. The lexical membership-list
experiment would scan all document heads even for selective queries, and the
identifier covered-page rewrite showed only a small warm-cache gain; neither
was added to production.

The full local `cargo xtask check` passes, including all 29 vector-storage
tests and strict Clippy. The optimized binary
`1ccf2ceef458c014c4791b39e44c9b88e228970671a7aa7c52427d85bb52995b`
passes CLI/MCP release smoke. Eighteen paired CLI observations preserve ordered
citations, scores, and hashes, with all 180 returned passages verified against
source byte windows. The small real-model cancellation smoke again pauses two
workers, cancels eight requests without late responses, and recovers lexical
and hybrid search. These are local checks; independent review and native
million-passage qualification remain open.

## Post-cancellation recovery failure — 2026-10-23

Run [39981593244](https://github.com/burkan2/hSUM/actions/runs/39981593244)
at `1ce11d00bf8cad949c12a9c496ab7dba3675717f` completes million-passage
ingestion, metadata generations, and re-embedding, then fails with
`post-storm lexical search failed`: **TIMEOUT / REQUEST_DEADLINE**, operation
`mcp_tool`. The [unaltered harness log](results/q05-39981593244-harness.log)
and [runner capacity](results/q05-39981593244-runner-capacity.txt) are retained.
No complete stress JSON report was produced.

Unlike the previous worker-start failure, this location in the harness is
reached after two real workers have been paused, eight cancellations sent,
and the workers resumed. That ordering is inferred from the frozen harness;
the log does not contain individual worker timestamps. It does not prove the
complete no-late-response interval or hybrid recovery, which are checked later.
Q05 therefore still fails. Diagnose the lexical recovery deadline before
repeating the full million-passage workload; do not raise the frozen deadline
or treat this failure as a completed cancellation qualification.

## Recovery ranking repetition — 2026-10-23

The retained `39981593244` error names the admitted `mcp_tool` deadline, not
`blocking_admission`. Lexical requests skip query embedding and full vector
membership preflight. In `execute_search`, content deduplication expands lexical
candidates from 50 to 500 when fewer than the requested unique results survive.
Each former `fetch_lexical_page` call reran the complete scoped BM25 ranking
with a new `LIMIT 50 OFFSET n`. The stress recovery fixture reaches that cap
with one surviving unique content, so it paid for ten full corpus rankings.
This is a reproduced recovery-path bottleneck; the prior runner log alone
cannot exclude additional cancellation or storage costs.

Candidate **`38272481bfbc9bb831ac05304135f3bdc54dda05`** keeps one transaction-local
500-candidate cursor and consumes it in the same 50-candidate fusion steps.
The SQL, project/head/version filters, scores, durable ties, candidate cap,
body/hash checks, field guards, deadlines, and final stop reasons remain.
The cursor is dropped before transaction commit. There is no schema, model,
dependency, persistent-cache, timeout, or qualification-threshold change.
The stress harness now logs completed scale/metadata/re-embedding/hybrid
observations before cancellation, so a later recovery error retains them.

The [retained evidence](results/lexical-cursor-3827248/review-receipt.json)
separates `before-1ce11d0` and `after-3827248` observations and includes probe
sources, SQL, ordered row hashes, failed regressions, and validation logs:

| Diagnostic scope | Repeated pages | One cursor |
| --- | --- | --- |
| Native SQLite 3.51.3, real reduced 100k index, 64 sources | 2.23–2.36 s; 47,061,271 VM steps; 20 sorts | 0.233–0.243 s; 4,801,786 VM steps; 2 sorts |
| Synthetic million-row short-body ranking, 64 sources | 20.28–22.49 s; 439,040,420 VM steps; 10 sorts | 1.80–1.94 s; 43,909,801 VM steps; 1 sort |
| Optimized product, reduced duplicate 100k index, fresh CLI processes | 3.48–4.82 s client time | 0.75–1.63 s client time |

All 500 ordered native row hashes match in each paired SQL comparison. All
12 reduced product observations preserve full ordered result packets,
including duplicate citations and scores; returned content matches the
shared source byte window and hash. These measurements ran alongside local
preparation/build work. The short-body fixture isolates ranking; it omits
product orchestration, real payloads, vectors, and cancellation. None of the
three comparisons is frozen 100k latency, cold-cache, or stress qualification.

The focused regression fails before repair at **20 sorts**, then passes with
the original 500-candidate tie boundary, one result, and 499 duplicate citations.
It also checks that a malformed field on a later page is rejected when that
page is consumed. The complete `cargo xtask check`, optimized release build,
and CLI/MCP release smoke pass. Eighteen paired small real-model CLI controls
preserve ordered citations/scores/hashes and verify all 180 returned content
windows. Small-model cancellation again pauses two workers, cancels eight
requests, suppresses late responses, and recovers lexical/hybrid search.
The optimized binary SHA-256 is
`9a08baa1c98d65a2628e72b28bc3ee54cc58011ec50f0446cbd8f1a12c7bed10`.

After those checks, native rerun
[40008791029](https://github.com/burkan2/hSUM/actions/runs/40008791029)
was dispatched on that exact repair candidate. It is still running at this
checkpoint. Q05 remains **unqualified** until its complete report is reviewed.
Author review is recorded separately; no independent source review is claimed.
