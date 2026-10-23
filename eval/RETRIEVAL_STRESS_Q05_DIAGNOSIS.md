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
