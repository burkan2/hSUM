# Performance implementation diagnostics — November 1, 2026

These are development diagnostics, **not a 100k qualification pass**. Starting
commit is `83b4d4c`; candidate changes are uncommitted at this checkpoint. Prior
native failures remain valid historical evidence and are not overwritten.

## Retained fixture and limits

All SQL and reduced MCP probes use a disposable APFS clone under
`~/.cache/hsum-qualification/2026-10-31-performance`. The original crash snapshot
and recovered `b6591b7` home remain unchanged. The clone's 813,934,872-byte WAL
was explicitly checkpointed for diagnosis; this took approximately 5.90 seconds.
That operation is not a fresh ingest measurement or a release workaround.

Reduced MCP A/B/A observations use two frozen queries, five warmups and twenty
measured passes, with exact result/explanation comparisons. Before checkpoint,
identifier p50 was 123/82/125 ms for old/request-reuse/old control; ordinary p50
was 260/242/262 ms. Variability was high. After checkpoint, identifier p50 was
44/39/44 ms and ordinary p50 181/176/180 ms. These diagnose separate opening and
ranking costs; they do not establish p95 or run-to-run qualification.

`ranking-probe.log` measures standalone diagnostic C variants. Warm baseline
SQL took approximately 179–204 ms, custom scoring with a preliminary global
frontier 65–69 ms, and custom scoring with all metadata joins 126–133 ms. The
first baseline sample was 684 ms and remains retained. Comparing
`ranking-probe-rows.log` established equal IDs/order and score bits for the
500 returned rows in this one fixture. The prototype's printed `mismatches=0`
was a placeholder, **not an executed comparison**.

The C prototypes are retained only to reproduce the investigation. Their
parsers and preliminary frontier are unsafe for production use: the standalone
frontier can lose project-scoped results and the size parser lacks validation.
They are neither compiled nor shipped by hSUM. Production Rust code uses a
checked bounded cache, lazy activation, error propagation, interruption checks,
and complete scope filtering with conditional remaining-candidate evaluation.

## Independent review and local tests

The user authorized an independent review subagent. It reviewed the prior WAL
descriptor/cursor repairs, request connection reuse, explicit checkpoint, and
new ranking changes. It identified repeated auxiliary registration as a real
memory-growth issue; a regression failed with 100 registrations instead of one
before the per-connection flag fix. Other review concerns informed the final
scope, cache, cancellation, and floating-point design. No outstanding source
blocker remained for native diagnostics after this fix. This is independent AI
review, not external security audit evidence.

Initial tests caught missing registration and 1–2 ULP differences between Rust
and native C BM25 arithmetic on Apple Silicon. The final arithmetic helper is
compiled with the same C toolchain/profile as bundled SQLite. Separate native
and custom SQL statements now compare every score bit. Equivalence must still
be verified in release builds on both supported platforms and after toolchain
or SQLite changes.

The final full `cargo xtask check` passed: formatting, clippy, Rust tests and
doctests, Python harness tests and frozen evaluation manifests. Search tests
cover foreign projects with better global scores, removed sources, inactive or
replaced heads, ties, offsets, empty/small result sets, sparse/large rowids,
malformed/missing sizes, and interruption during statistics, positions and
cache loading followed by successful connection reuse. Existing guarded body
materialization and 500-candidate paging regressions also pass.

The Mac release build passed all 21 search tests and the release CLI/MCP smoke
test. `candidate-receipt.json` binds the checked source files and binary.
`ranking-product-ab.json` retains a reduced A/B/A product run over all 25 frozen
queries, two warmup passes and five measured passes per binary. Every returned
result and explanation matched. On the disposable checkpointed fixture,
`ordinary-02` median server latency fell from 182 ms to 88 ms; the optimized
five-sample p95 was 89 ms. This small sample is diagnostic only. It neither
replaces fresh preparation nor establishes the canonical 2,250-sample SLO/CV.

The actual CLI `ingest --reembed` then reused 100,000 cached inputs on the
disposable clone, committed generation 3, and returned success with empty
stderr and a zero-byte WAL. `checkpoint-product.json` records 48.06 seconds,
file sizes and binary hash. This checks post-commit checkpoint integration;
cached inputs do not count as fresh inference throughput. Independent review
subsequently approved candidate freeze and native qualification after verifying
the per-connection registration flag and both-platform release-test CI step.

Still required: Linux release-profile checks, fresh Mac
ingest throughput, full same-candidate Mac/Linux 100k protocol, and native
million-passage memory/temp-storage checks. Stable signing authorities remain
unavailable. No stable release is authorized by these diagnostics alone.
