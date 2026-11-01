# Linux 100k qualification — candidate `41af5c7`

**Disposition: FAIL.** All 2,250 raw observations were checked against the frozen query order, classes, pass numbering, candidate bounds, summaries and exclusive SLOs. The three summary distributions and qualification flags were recomputed from the raw records. `audit.json` retains the recomputation summary beside the report.

| Class p95 (ms) | Run 1 | Run 2 | Run 3 | Exclusive limit |
| --- | ---: | ---: | ---: | ---: |
| identifier | 53 | 53 | 53 | <100 |
| ordinary_lexical | 104 | 104 | 104 | <100 |
| exact_fallback | 85 | 85 | 84 | <750 |
| hybrid_no_fallback | 213 | 214 | 212 | <500 |

Total-p95 population CV: **0.233%**, below 10%. Each run used a fresh process, five warmup passes and 30 measured passes of the same 25 queries. No thresholds, queries, or observations were discarded.

Fresh lexical ingest: 40.74 seconds (2454.30 passages/s). Fresh re-embedding: 6015.59 seconds (16.62 passages/s). Model acquisition was outside timed preparation. The index contains 100,000 cached vectors; the final WAL is zero bytes.

| Separate cold measurements (ms) | Run 1 | Run 2 | Run 3 |
| --- | ---: | ---: | ---: |
| Process startup | 3.67 | 3.66 | 3.72 |
| OS-cache lexical, server total | 418.00 | 355.00 | 358.00 |
| First model-load search, server total | 1391.00 | 1324.00 | 1322.00 |
| First hybrid search, server total | 687.00 | 691.00 | 689.00 |

Maximum measured process-tree RSS: 553.99 MiB; all RSS samplers completed without error. Managed index: 1,188,147,200 bytes, including 150,000,000 logical live-body bytes and 307,200,000 logical vector-payload bytes. The separate cold observations are not mixed into warm SLOs.

Commit: `41af5c711f2bacfd84f68b28b0d8c4ab47711ca6`; recorded source checkout was clean. Binary SHA-256: `76a7baa6c3362939b62f776d6b880eb7487d0ac27c10abe7bc78496bc4ad89e7`. Frozen manifest and corpus hashes match the checked-in harness. File hashes are retained in `files-sha256.json`.

Linux ordinary lexical p95 is 104 ms in every run, above the unchanged <100 ms requirement. The broad `ordinary-02` query accounts for the remaining failure. All other class SLOs, cold/setup/RSS requirements and consistency pass. Preserve this failure; investigate before selecting another candidate.

Workflow: https://github.com/burkan2/hSUM/actions/runs/41267701304
