# Linux million-passage stress review

Candidate: `38272481bfbc9bb831ac05304135f3bdc54dda05`.
Run [40008791029](https://github.com/burkan2/hSUM/actions/runs/40008791029).

**Pass: the frozen report-only Linux stress workload.** This does not close
100k latency qualification, qualify macOS, or imply quality/token-cost superiority.

The GitHub archive digest matches the retained ZIP. The exact clean candidate,
manifest/corpus, 75 scale observations and summaries, five final vector-hybrid
observations, bounded candidate counts, finite telemetry, metadata/readers,
storage arithmetic and pre-cancellation log were verified. The review script
reuses the unchanged harness and includes assertions for every check.

## Cancellation and recovery

- Exactly two real model workers were paused and eight requests cancelled.
- The frozen same-process harness completed lexical and hybrid recovery and
  its remaining no-late-response checks through clean MCP process exit.
- Lexical recovery examines 500 lexical candidates; hybrid examines 500 vectors.
- Cancellation process-tree peak RSS is 654,983,168 bytes (624.641 MiB).
- No scale-probe or final-hybrid deadline was recorded.

The upload includes a final summary rather than per-worker timestamps or a
full MCP transcript. The no-late-response conclusion is therefore bound to
the completed frozen harness; no stronger independently timestamped claim
is made.

## Scale, memory, and storage

| Sources | Active passages | Literal client p50 | Duplicate lexical client p50 | Punctuation fallback client p50 |
| --- | --- | --- | --- | --- |
| 2 | 15,880 | 174.475 ms | 198.788 ms | 281.362 ms |
| 8 | 111,160 | 597.205 ms | 784.001 ms | 857.311 ms |
| 16 | 238,200 | 1234.941 ms | 1607.078 ms | 1698.082 ms |
| 32 | 492,160 | 2491.329 ms | 3290.150 ms | 3368.530 ms |
| 64 | 1,000,000 | 5200.481 ms | 6843.127 ms | 6905.261 ms |

The first >2x knee is the literal at eight sources / 111,160 passages:
174.475 to 597.205 ms, ratio 3.422862162. JSON serializes query-object keys
alphabetically; the review restores the manifest query order before invoking
the existing knee helper, matching execution order. An initial assertion
failure from overlooking this ordering is retained. No probe deadline occurs.

Five final vector-hybrid probes have client p50 6,088.520 ms and p95
6,103.379 ms. The million-passage suite is report-only; the 100k class
thresholds are not applied to it.

The index has 1,000,000 active memberships, 64 sources, 100,000 documents,
100,100 document versions, 106 generations, one shared content blob, ten
chunk bodies and 15,880 cached embedding inputs. All 100 metadata operations
are recorded; four old readers retain generation 104 while a fresh reader
sees generation 105. Re-embedding commits generation/epoch 106.

| Metric | Value |
| --- | --- |
| Initial ingest total | 1030.652 s |
| Initial ingest peak process-tree RSS | 3,144,593,408 bytes |
| Re-embedding | 2971.521 s; 336.528 passages/s |
| Re-embedding peak process-tree RSS | 600,858,624 bytes |
| Metadata total | 6984.806 s |
| Metadata maximum process-tree RSS | 172,032,000 bytes |
| Managed index, WAL and SHM | 7,305,599,456 bytes |
| Logical live/history content | 15,000 / 0 bytes |
| Logical active/cached vectors | 1,536,000,000 / 24,391,680 bytes |
| Amplification versus encoded snapshots | 4.819951x |
| Amplification versus unique body | 487039.963733x |

Memory observations describe this run, not a constant-memory proof. Metadata
timing summaries cannot be recomputed per operation because individual
metadata-operation timings are not uploaded. The full index and binary are
also absent, so physical integrity and result-byte checks cannot be repeated
independently from the report. Candidate-bound product integrity checks remain
in force throughout the original run.

Prior stress failures remain retained. This pass is specific to Linux and
`3827248`; the failed Linux 100k report and failed Mac preparation belong to
`1ce11d0`. A reviewed two-platform 100k comparison is still unavailable.
