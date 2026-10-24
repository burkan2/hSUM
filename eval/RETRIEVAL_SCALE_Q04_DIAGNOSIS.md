# Q04 100k retrieval-scale diagnosis

Original failure: 2026-08-23; local repair: 2026-10-21

Disposition: join-plan regression repaired locally; native Q04 qualification remains open

## Immutable execution

The exact-source Linux execution is GitHub Actions run
[`32655822954`](https://github.com/burkan2/hSUM/actions/runs/32655822954), job
`97234280959`, at commit
`437ca35b47c75305a18def31728d3e6666641262`. The candidate build and pinned
model installation passed. The frozen setup then completed with exactly:

- 10,000 documents;
- 100,000 active passages;
- 150,000,000 corpus bytes;
- corpus fingerprint
  `33d81ffd4df2ae20fb8b67257b1a559c05c8589133e51e32aef856df4034e287`;
- query-manifest SHA-256
  `71a55f62cf6a316ca8ec28ef2a5e9f64c9c1e6955cac50150560ccee0fd7f84b`;
- 80.793 seconds ingest time, or 1,237.727 passages/second;
- 8,377.005 seconds re-embed time, or 11.937 passages/second; and
- a 1,621,612,376-byte healthy, query-only managed index with no reported
  Doctor problems.

Artifact `9499378134` (`retrieval-scale-linux-x86_64`) contains the setup
report only. Its independently downloaded `setup-linux-x86_64.json` has
SHA-256
`748c84b9d13aa113b8006e0ec6519959112954887d25504af67a9da8acb75ac9`.
The setup report binds candidate binary SHA-256
`9a30c99760c3e5095c1702e7b9c4e3844e8ebe3f95b411c9eaea997f985f22e9`.

## Failure

The first fresh MCP process reached the frozen warmup protocol, then query
`ordinary-02` (`immutable citation evidence`, lexical mode) exceeded the
10,000 ms request deadline. The harness reported `REQUEST_DEADLINE` and
uploaded diagnostics, but it could not emit the required 750-observation
qualification report. No Q04 latency, CV, or SLO pass is therefore claimed.

Every passage contains all three `ordinary-02` terms in the frozen corpus
filler. This deliberately exercises a high-document-frequency BM25 boundary;
it is not an ingest-integrity or query-parser failure. Plain words do not enter
the identifier-literal exact path, so the request reaches the FTS5 lexical
retriever.

## Initial query-plan hypothesis (superseded by the local reproduction below)

Both the lexical and quoted-phrase candidate queries order by
`bm25(passages_fts)` and then by durable source ID, document ID, and start
byte. For the all-match query, SQLite must score and sort all 100,000 rows
before returning the bounded candidate page.

SQLite's FTS5 documentation notes that ordering by the hidden `rank` column is
faster than calling `bm25()` in the `ORDER BY`, particularly with `LIMIT`:
<https://www.sqlite.org/fts5.html#sorting_by_auxiliary_function_results>.
Local SQLite 3.50.4 and 3.51.0 plan probes confirmed that hSUM's project-scope
joins retain the optimized FTS5 `ORDER BY rank LIMIT` plan. Adding hSUM's
durable identity tie columns, however, reintroduces a temporary full sort.

## Rank-only proposal disposition

A rank-only top-500 preselection would avoid the full sort, but it would allow
FTS row order to decide membership when more than 500 passages share the
boundary score. That can change public lexical rankings and contradicts the
frozen source-ID, document-ID, start-byte tie contract described in
[`LEXICAL_VARIANCE_DIAGNOSIS.md`](LEXICAL_VARIANCE_DIAGNOSIS.md).

The stable-v0.1 candidate therefore does not silently adopt that optimization.
A change to candidate membership requires a separately preregistered retrieval
revision that:

1. defines deterministic candidate membership at the work-budget boundary;
2. updates and freezes the retrieval configuration fingerprint;
3. reruns held-out lexical/hybrid baselines and same-index determinism; and
4. reruns Q04 on Linux x86_64 and the canonical Apple M2/16 GB target.

Until those gates pass, Q04 remains failed and stable publication remains
unqualified. The successful corpus construction is retained as setup and
throughput evidence only.

## 2026-10-21 root cause and behavior-preserving repair

A reduced product fixture using the first 1,000 frozen corpus documents
(10,000 passages) reproduced `REQUEST_DEADLINE` after 10 seconds for both
`immutable citation evidence` and `"immutable generation"`. A process sample
located the work in `fetch_lexical_page` → `fts5Bm25Function` →
`fts5ApiQueryPhrase`, rather than in sorting alone.

The same query against the same index takes about 30 ms in Python's SQLite
3.50.4. An optimized standalone probe compiled from the product's exact
SQLite 3.51.3 amalgamation exposes the difference: the planner starts with
`project_sources`, looks up `active_passages` by source, and repeatedly opens
the FTS cursor with both rowid and MATCH constraints (`INDEX 0:=M3`). BM25's
per-query statistics are consequently recomputed for each passage. The
unchanged full query took about 7.5 seconds on this development fixture.

Both `fetch_lexical_page` and `fetch_phrase_candidates` now use `CROSS JOIN`
between `passages_fts` and `active_passages`. This keeps FTS outside the
passage-row lookups; the probe uses `INDEX 0:M3` and completes in about 32 ms.
The full sort, every project/active-head filter, BM25 scores, durable identity
tie order, candidate limits, field guards, and deadline handling remain in
place. This is an execution-plan repair, not the rejected rank-only
preselection. It does not revise the retrieval configuration fingerprint.

Verification:

- The new 10,000-document regression fails with `DeadlineExceeded` before the
  repair and passes afterward for both ordinary and quoted queries. It checks
  the first 50 durable document identities across equal scores and excludes a
  better-scoring document in another project.
- All 17 search-contract tests pass, including adaptive paging, duplicate
  citations, source scope before LIMIT, and tampered/oversized field refusal.
- A repeated standalone comparison returns exactly the same ordered passage
  IDs and BM25 scores before and after the repair (9.07 seconds versus 35 ms
  in that run). This is a reduced SQL probe, not a product latency claim.
- Three fresh product processes per query complete on the reduced frozen
  fixture with identical ordered citations. These are unoptimized development
  runs; their timing is not release performance evidence.
- The complete local `cargo xtask check` gate passes with
  `CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0
  CARGO_BUILD_JOBS=2`; these build settings keep the check within the available
  disk space. The rebuilt binary also passes `scripts/release-smoke.sh`
  (initialization, CLI search/Get, Doctor, and all four MCP tools).

The original 100k failure is retained. The full 25-query, three-process Q04
protocol, optimized candidate measurements, and Linux/Apple M2 qualification
must still run; neither the reduced probe nor the regression test proves the
100 ms lexical SLO.

## Follow-up run and remaining local bottleneck

Linux run [39510576430](https://github.com/burkan2/hSUM/actions/runs/39510576430)
at `caf2f19` completed the 100k setup and progressed past `ordinary-02`, then
failed because `fallback-02` did not exercise exact fallback. The query
`"x::y"` uses only tokenizer characters and correctly takes the phrase path.
The harness now uses `"x^y"` and checks every lexical query class before
re-embedding. `benches/retrieval_scale/README.md` records the revised manifest
and corpus fingerprints; historical setup receipts are not reused with them.
All 19 corrected lexical paths pass on a small real-product corpus, and that
same preflight rejects the original `"x::y"` fixture.

The Apple M2 preparation was interrupted before completing its vector
generation. It has no setup receipt or qualified latency report. Its partial
index is retained only for diagnosis. Native SQLite 3.51.3 probes on its
100,000 lexical passages show an additional chunk-lookup cost: the isolated
FTS/active-passage join is subsecond, while adding chunk row lookups takes
several seconds. A process sample places the work in B-tree page traversal.
Checkpointing a disposable copy, increasing the page cache, and memory mapping
did not establish a reliable fix. These development probes have different
cache and resource conditions from the frozen protocol and are not SLO data.

Full native qualification remains open. No ranking, identity tie-break,
candidate limit, field-validation rule, or performance threshold was changed
to turn these failures into a pass.

## Completed Linux measurement — reviewed 2026-10-22

Run [39638597258](https://github.com/burkan2/hSUM/actions/runs/39638597258)
at `ba527f9` completes the corrected setup and all **2,250 observations**:
750 observations in each of three fresh processes. The
[unaltered report](results/q04-39638597258-linux-x86_64-100k.json) and
[setup receipt](results/q04-39638597258-setup-linux-x86_64.json) are retained.
All protocol-completeness checks pass, including cold-cache methodology,
RSS, throughput, and total-p95 CV (0.149%, below the 10% maximum).
Every latency class fails its unchanged SLO:

| Class | p95 across three runs | Required p95 |
|---|---:|---:|
| Identifier | 3421–3433 ms | <100 ms |
| Ordinary lexical | 574–582 ms | <100 ms |
| Exact fallback | 3759–3781 ms | <750 ms |
| Hybrid without fallback | 3981–3988 ms | <500 ms |

The first run's identifier time is dominated by the exact stage (3268 ms
mean), while exact fallback spends 3524 ms there. Hybrid averages 3944 ms
total despite 7 ms query embedding and 110 ms vector search. This implicates
work outside those measured components, consistent with full vector preflight;
it is not proof of a complete profile. The next repair must isolate the
candidate-query plan and preflight cost while preserving corruption detection,
scope checks, candidate membership, and deterministic ordering. Another long
qualification run without a verified repair would add little evidence.

Setup ingests 100,000 passages in 81.45 seconds and embeds them in 9584.19
seconds. This is successful setup and a completed **failed performance
qualification**, not a release pass. Apple M2 qualification remains open.

## Exact-candidate paging repair — 2026-10-22

On the retained Apple M2 100k diagnostic index, the bundled SQLite 3.51.3
plan for `fetch_project_passages` starts with `project_sources` and sorts
all joined rows before returning 50 passages. The fully guarded SQL query
takes 13.18 seconds in the isolated reproduction. Adding the equivalent
`ap.source_id IN (SELECT source_id FROM project_sources WHERE project_id = ?
AND removed_at IS NULL)` constraint exposes the leading column of the existing
`active_passages_source_idx`. SQLite can then seek the selected sources in
identity order and sort only the final start-byte term within a document.
The same probe returns identical ordered results in 9.57 ms.

The production change adds only that predicate to raw candidate paging. It
retains the existing project/head/version joins, full field bounds, body hash
and Bloom verification, ordering, limit, offset, and query deadline. It does
not force a scan through other projects or introduce a cache, schema migration,
or new ranking rule. SQLite documents this
[partial ORDER BY optimization](https://www.sqlite.org/optoverview.html#partial_order_by_via_index).

The regression uses real stored snapshots with two selected sources, a removed
source, and an unrelated project. It checks exact ordered IDs across the
source boundary and bounds SQLite VM work independently of host timing. The
old query fails at 40,846 VM steps; the repaired query passes the 20,000-step
ceiling. The complete local `cargo xtask check` passes with the repair. Full
native performance qualification is still required. This repair
does not address hybrid vector preflight or the ordinary lexical SLO failure.

The optimized CLI comparison uses three fresh-process repetitions per binary
and query, alternating binary order on the same retained index. The raw quote
`"a+b"` times out at 10 seconds in all three previous-build runs. With the
repair it returns ten results in 263–331 ms wall time (median 286 ms), with
232–253 ms server total and 57–82 ms exact-stage time. All 30 returned contents
match their live source byte windows and content hashes; ordered citations and
scores agree across the three repaired runs. The previous CLI times out, so
cross-build result equality is established only by the isolated SQL probe
and regression, not by completed CLI responses.

The identifier query `scale_identifier_000001` still reaches the 10-second
deadline in all six before/after runs. This narrows the repair to raw paging;
identifier candidate work remains unresolved. The retained corpus predates
the corrected frozen fixture and has incomplete embedding preparation, so
these observations are diagnostic, not a native qualification pass. Binary
and source hashes, command arguments, responses, and verification details are
in [the diagnostic report](results/exact-page-2026-10-22-macos-diagnostic.json).
The release build and release smoke also pass.

## Identifier-candidate join-order repair — 2026-10-22

The next isolated query probe reproduces the remaining identifier timeout.
The literal lookup itself uses `passage_literals_lookup_idx`, but SQLite
places the outer project/passage joins ahead of the `ranked` CTE. It reads
every project passage, including document/chunk lookups, before testing
whether the passage is among the literal matches. The fully guarded query
returns 50 rows in 13.70 seconds on the retained Apple M2 100k index.

Changing only `JOIN active_passages` after `FROM ranked` to `CROSS JOIN`
keeps ranked literal matches outside passage materialization. The isolated
query returns the same ordered column bytes and scores in 97.38 ms. Forcing
literal-first order inside the earlier matching CTE does not fix the outer
join problem; that control still takes 9.95 seconds. SQLite documents
[CROSS JOIN as explicit loop-order control](https://www.sqlite.org/optoverview.html#manual_control_of_query_plans_using_cross_join).

The production repair retains every existing join predicate, bound argument,
field/hash/Bloom check, rank component, identity tie-break, and page boundary.
The existing real-snapshot regression now also checks identifier results and
pagination across two selected sources while excluding removed and unrelated
sources. It fails before the fix at 28,400 VM steps and passes below 5,000
afterward. The complete local `cargo xtask check` passes. The previous
raw-paging commit `95784f0` also passes all seven standard remote workflows,
including CI run `39806626604`; that remote result does not qualify this new
repair or close Q04.

The optimized CLI now completes the previously timing-out identifier query
in all three fresh-process runs: 139–247 ms wall time, median **190 ms**,
with 113–213 ms server total. The previous `95784f0` binary times out at
10 seconds in all three alternating before runs. The ordinary all-match
control still takes 2.28–8.44 seconds server time; its bottleneck is separate
from identifier ranking. The raw quote control completes in 166–205 ms server
time (218 ms median wall time).

All 90 successful result contents match their live source byte windows and
content hashes. Ordered citations and scores agree within each query across
the three repaired runs. The before binary's identifier responses time out,
so cross-build equality comes from the isolated SQL probe. The complete raw
[identifier diagnostic report](results/identifier-page-2026-10-22-macos-diagnostic.json)
binds binary/source hashes, commands, query plans, responses, and summaries.
The optimized release build and release smoke pass. These retained-index
observations do not follow the frozen native qualification protocol and do
not meet the identifier 100 ms target; Q04 remains open.

## Covered lexical ranking — 2026-10-22–23

The all-match ordinary query still reads large chunk rows while ranking every
matching passage. Deferring the full guarded projection alone is insufficient:
SQLite chooses chunk rowid lookups even when a compact `(id, start_byte)` index
exists. The bundled SQLite probe takes 11.25 seconds for the original query,
9.21 seconds with deferred projection alone, and 9.12 seconds with an unforced
index. Requiring the covering index and materializing the ranked page reduces
the isolated query to 194 ms. All variants produce the same ordered 50 rows,
including scores and every guarded column, with SHA-256
`9da8895c9539061a0f0a51f95d15fdc47840fc9177991c8975eb3925cfdb63ec`.

Alternating probes with fresh SQLite connections, run under local build load,
take 7.50–12.82 seconds before and 0.62–1.13 seconds after. These are diagnostic
measurements, not cold-cache or qualification observations. SQLite documents
the [materialization fence](https://www.sqlite.org/lang_with.html#materialization_hints)
and [covering-index plan](https://www.sqlite.org/eqp.html#table_and_index_scans).

Both lexical and quoted-phrase candidate queries now share this SQL. Every
project/head/version/chunk join remains inside the ranking stage, and the
outer stage retains field bounds, byte/hash/Bloom validation, scores, durable
identity tie order, and the same limit/offset. There is no rank-only top-500
shortcut. The query-plan regression fails before the change; the real-snapshot
regression also verifies lexical and phrase pagination across selected sources
while excluding removed and unrelated sources.

Schema 5 adds only `chunks_order_idx`. The schema-4 migration chain remains
byte-for-byte frozen. Existing indexes require the explicit hash-confirmed
plan/apply ceremony and verified backup. Migration inspection now validates
schema-4 semantic generations, provenance, cached vector payloads, and active
vector membership. Tests preserve exact semantic results through migration,
reject corrupt plan/apply inputs before mutation, and prevent a pinned schema-4
rebuild from silently becoming lexical-only. This is development compatibility
work; the installed binary and active user indexes were not upgraded.

The first optimized CLI trial forced the new index in every passage join.
It preserved all 170 successful result contents, citations, and scores, and
reduced the ordinary query from two 8.7–8.9-second responses plus a timeout to
177–2267 ms server time. However, the identifier control regressed to
49–581 ms, and raw quote to 57–255 ms. These observations are retained in
[the initial trial](results/lexical-cover-2026-10-22-macos-diagnostic.json).
That broader index requirement was rejected. The final query requires the
covering index only inside lexical/phrase ranking; all body-loading, exact,
and vector passage lookups retain the prior rowid join. The query-plan test
also requires the outer page to use rowid lookups.

The actual 100k schema-4 migration was exercised on a private disposable
snapshot through `migrate plan` and hash-confirmed `migrate apply`. Both
completed, the verified pre-migration backup was retained, and the resulting
index passed Doctor. The recorded plan took 83.59 seconds and apply took
80.71 seconds, including full integrity verification; those are migration
diagnostics, not search timings. The original diagnostic index remains at
schema 4. Historical held-out manifests still identify their original frozen
schema-4 candidate and are not rewritten as evidence for schema 5.

The final optimized build (`435de117b570d30fd063f605a9ce87b7782852f6103aa0e7908cf83f38218618`)
passes the complete local `cargo xtask check` and CLI/MCP release smoke. The
source-package listing includes the new migration and shared schema-4 fixture.
The [final paired CLI report](results/lexical-cover-2026-10-23-macos-diagnostic.json)
records three fresh-process repetitions per query/build, alternating build
order against the original schema-4 index and its migrated schema-5 snapshot:

| Query | Before server total | After server total |
|---|---|---|
| Ordinary all-match | 9332, 8733 ms; one 10-second deadline | 2331, 184, 2147 ms |
| Identifier control | 281, 168, 156 ms | 828, 42, 854 ms |
| Raw-quote control | 207, 151, 129 ms | 234, 53, 227 ms |

All 170 successful returned passages match source byte windows and hashes;
ordered citations and scores match across every successful before/after
response. Ordinary-query median client time falls from 9055 ms among the two
successful before runs to 2203 ms after. The timeout is retained, not excluded
from the report. Neither the warm nor slower after observations meet the
ordinary 100 ms target.

The control timings depend strongly on the preceding broad query and cache
state. The exact identifier query has an identical SQLite plan on both
schemas. A separate recorded warmup followed by three paired fresh-process
control repetitions, without intervening broad scans, yields identifier
42/43/42 ms before and 42/41/44 ms after; raw quote yields 52/53/54 ms before
and 51/53/56 ms after. Their ordered citations, scores, and hashes still match.
This supports unchanged warm control behavior; it does not establish a
cold-cache non-regression or native qualification pass. Both sets of raw
observations are retained. Q04, the remaining identifier budget, and vector
preflight qualification remain open.

## Completed native protocol on `1ce11d0` — 2026-10-23

[Run 40004812595](https://github.com/burkan2/hSUM/actions/runs/40004812595)
completes the corrected frozen corpus, all three fresh processes and **2,250
measured queries**, then exits 1 because identifier and ordinary lexical p95
still exceed 100 ms. Exact fallback and hybrid now pass their class limits.
The [retained review](results/retrieval-scale-40004812595-1ce11d0/REVIEW.md)
includes the unchanged artifact ZIP, GitHub artifact digest/metadata, failed
step log, setup/report JSON, review source, and exact recomputed results.

| Warm class | Run 1 p95 | Run 2 p95 | Run 3 p95 | Strict limit | Result |
| --- | --- | --- | --- | --- | --- |
| Identifier | 141 ms | 137 ms | 129 ms | < 100 ms | fail |
| Ordinary lexical | 286 ms | 284 ms | 283 ms | < 100 ms | fail |
| Exact fallback | 147 ms | 149 ms | 161 ms | < 750 ms | pass |
| Hybrid without fallback | 273 ms | 277 ms | 264 ms | < 500 ms | pass |

Total p95s are 277/275/268 ms; population CV **1.411687427%** passes the <= 10%
gate. All other frozen qualification flags pass. Cache eviction and separate
cold observations are recorded for each process, with peak process-tree RSS
545,128,448 / 556,302,336 / 536,739,840 bytes and no sampler errors. Managed
storage is 2,002,340,264 bytes including WAL/SHM; logical live content is
150,000,000 bytes, history zero, and active plus cached vector payload
307,200,000 bytes. Initialization/ingest takes 32.594 s, re-embedding 4,659.190 s.
These recorded memory/storage values do not introduce an additional SLO.

The slow `ordinary-02` samples examine only 50 lexical candidates. Its first
warm sample records 199 ms lexical and 282 ms total; `identifier-01` records
31 ms exact, 7 ms lexical, and 122 ms total. Per-query distributions retain the
roughly 80 ms wall-time remainder outside the non-overlapping retriever
stages. That remainder needs profiling; these timers do not establish which
connection-open, snapshot, or other orchestration operation caused it.
`body_materialization` and `exact_fallback` overlap with other stage timers and
must not be added twice. The separate adaptive-cursor repair `3827248` targets
repeated ranking through 500 candidates in stress recovery; it does not prove
that these 50-candidate Q04 failures are repaired. Preserve all checks and
ranking while profiling the broad ranking and orchestration before another
expensive 100k rerun. The separate recovered Mac protocol is now complete,
as recorded below; no retrieval-quality or token-cost claim follows.

## Completed recovered Mac protocol on `b6591b7` — 2026-10-24

After the separately diagnosed WAL descriptor repair, the frozen native M2 /
16 GB query protocol completes all **2,250 observations** on a recovered index.
The [review and raw receipts](results/retrieval-scale-macos-recovery-b6591b7/REVIEW.md)
retain the failed exit and every sample. Identifier p95 522/299/211 ms and
ordinary lexical 502/400/371 ms fail <100 ms; hybrid 655/549/470 ms fails the
every-run <500 ms condition. Fallback 322/255/225 ms passes <750 ms.
Total-p95 CV **18.928468601%** fails <=10%. The explicitly cached recovery
leaves fresh ingest throughput null, and its frozen setup gate correctly fails.
Actual cache eviction, separate cold observations, RSS and storage are reviewed.

The [status comparison](RETRIEVAL_QUALIFICATION_2026-10-23.md)
keeps this `b6591b7` recovery separate from Linux baseline `1ce11d0` and Linux
stress `3827248`. It is not a causal repair estimate or platform speed result.
Within-run timing also shifts; no thermal/frequency/background/page-fault
telemetry establishes why. No warm samples may be removed to close the CV gate.

## Reduced connection-lifetime profile — 2026-10-24

The [native Mac A/B/A diagnostic](results/query-open-profile-b6591b7/REVIEW.md)
uses the same `b6591b7` binary on a disposable recovered clone, two frozen
lexical queries and the existing MCP validator. It does not repeat the full
qualification. With normal connection lifetimes, identifier server p50 is
121 ms and ordinary lexical 258 ms. Keeping a separate read-only SQLite
attachment alive reduces these to 42 / 177 ms. Releasing it restores
122 / 259 ms. Native sample stacks show repeated WAL-index reconstruction in
`IndexDb::open_existing`/`inspect_connection`; the keeper nearly removes that
stack and release restores it. This identifies about 80 ms of connection/WAL
overhead on this fixture. The shared handler closes preflight before opening
retrieval, even in lexical mode. Expected-schema reconstruction is visible but
is a smaller sampled branch, so caching it is not justified as the main fix.

Broad FTS5/BM25 ranking remains and ordinary lexical still exceeds 100 ms with
the keeper. No production keeper, checkpoint, cached validation or threshold
change is introduced. A future connection-lifetime repair must retain index
replacement, namespace/snapshot/corruption checks, cancellation, reader/writer
behavior and final lock cleanup, with focused regression evidence. Native
Linux attribution remains unmeasured; similar wall-time remainders alone do
not prove the same mechanism. Preserve this diagnosis before any expensive
same-candidate two-platform run.
