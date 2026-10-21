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
