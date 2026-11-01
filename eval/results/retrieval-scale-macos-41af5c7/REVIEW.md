# Darwin 100k qualification — candidate `41af5c7`

**Disposition: PASS.** All 2,250 raw observations were checked against the frozen query order, classes, pass numbering, candidate bounds, summaries and exclusive SLOs. The three summary distributions and qualification flags were recomputed from the raw records. `audit.json` retains the recomputation summary beside the report.

| Class p95 (ms) | Run 1 | Run 2 | Run 3 | Exclusive limit |
| --- | ---: | ---: | ---: | ---: |
| identifier | 39 | 39 | 39 | <100 |
| ordinary_lexical | 84 | 85 | 84 | <100 |
| exact_fallback | 47 | 49 | 47 | <750 |
| hybrid_no_fallback | 182 | 184 | 180 | <500 |

Total-p95 population CV: **0.269%**, below 10%. Each run used a fresh process, five warmup passes and 30 measured passes of the same 25 queries. No thresholds, queries, or observations were discarded.

Fresh lexical ingest: 25.62 seconds (3902.95 passages/s). Fresh re-embedding: 9725.92 seconds (10.28 passages/s). Model acquisition was outside timed preparation. The index contains 100,000 cached vectors; the final WAL is zero bytes.

| Separate cold measurements (ms) | Run 1 | Run 2 | Run 3 |
| --- | ---: | ---: | ---: |
| Process startup | 8.78 | 9.69 | 9.34 |
| OS-cache lexical, server total | 467.00 | 562.00 | 501.00 |
| First model-load search, server total | 1270.00 | 1220.00 | 1368.00 |
| First hybrid search, server total | 423.00 | 426.00 | 429.00 |

Maximum measured process-tree RSS: 783.34 MiB; all RSS samplers completed without error. Managed index: 1,188,032,512 bytes, including 150,000,000 logical live-body bytes and 307,200,000 logical vector-payload bytes. The separate cold observations are not mixed into warm SLOs.

Commit: `41af5c711f2bacfd84f68b28b0d8c4ab47711ca6`; recorded source checkout was clean. Binary SHA-256: `693242b7a89ddb559198aa48503ef942853ea715bcb7483cb2c8eb7c874b42e7`. Frozen manifest and corpus hashes match the checked-in harness. File hashes are retained in `files-sha256.json`.

This fresh Mac setup replaces the missing-throughput limitation of the older recovered-index diagnostic. The older failure/crash evidence remains intact. Linux on this same candidate still fails ordinary lexical latency, so this is not a cross-platform release pass.
