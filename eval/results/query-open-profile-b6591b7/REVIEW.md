# Reduced query-open profile — 2026-10-24

Production candidate `b6591b7405320c924dc341111006d8d87b1e0c8a`, unchanged
optimized binary. The already completed frozen Mac failure is untouched.
This uses a disposable APFS clone of the recovered home, the existing MCP
session/packet validator and frozen `identifier-01` / `ordinary-02` queries.
No model inference, rebuild, cache eviction or full qualification rerun occurs.

Each diagnostic process warms both queries five times. Native `sample` captures
8 seconds at 10 ms intervals (5 seconds for the final control). The first and
second phases record 60 repetitions per query; the final control records 20.
Sampling can perturb latency, and this reduced sequence is not the full frozen
query mix. All raw compact samples and sample trees are retained.

| Phase | Identifier server p50 / p95 | Ordinary server p50 / p95 |
| --- | --- | --- |
| Normal connection lifetimes | 121 / 163 ms | 258 / 283 ms |
| External read-only SQLite attachment kept alive | 42 / 45 ms | 177 / 184 ms |
| Attachment released, fresh MCP process | 122 / 131 ms | 259 / 350 ms |

The intervening keeper executes only `PRAGMA application_id` on the disposable
index and remains open without an explicit read transaction until MCP exits.
It is a causal diagnostic control, not a proposed product workaround.

Normal sample stacks show `IndexDb::open_existing -> inspect_connection ->
pragma_query_value -> sqlite3PagerSharedLock -> walTryBeginRead ->
walIndexReadHdr`, including WAL frame reads/checksums and SHM reconstruction.
The keeper phase nearly removes that stack; the final release control restores
it. This localizes about 80 ms of repeated query overhead on this fixture to
WAL attachment/reconstruction under serial connection lifetimes. It does not
attribute the original SIGBUS to a particular process or prove the Linux
wall-time remainder has the same cause. Schema reconstruction is visible but
is a smaller sampled branch; caching the expected schema is not established
as the primary repair.

The shared search handler closes its preflight before opening the retrieval
connection, including in lexical mode. Broad FTS5/BM25 ranking remains visible
with the keeper and ordinary lexical still exceeds 100 ms. Therefore a WAL
lifetime change alone does not establish a passing qualification.

Further product repair must preserve replacement refusal, per-request snapshot
and namespace checks, cancellation/deadlines, reader/writer concurrency and
WAL lock cleanup; an external keeper must not be installed as a benchmark-only
bypass. Establish native Linux evidence and focused regressions before another
expensive full run. No production change or new candidate is made here.

The initial diagnostic failed only while summarizing two classes with the
all-class harness helper. Its exact source, failure log and sample are retained
as `initial-*`; the corrected diagnostic reuses the per-query distribution
helper and asserts counts. The receipts and raw sample trees support author
review only. No retrieval-quality, token-cost or release claim follows.
