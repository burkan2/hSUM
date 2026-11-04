# Linux 100k qualification — candidate `5ca552c`

**Disposition: FAIL.** Author and independent AI review recomputed all 2,250
observations, fixed query/pass order, all summaries, class SLOs and qualification
flags against the frozen harness. The candidate source was clean; the setup
and measured binary hashes agree. No samples or thresholds were changed.

| Class p95 (ms) | Run 1 | Run 2 | Run 3 | Exclusive limit |
| --- | ---: | ---: | ---: | ---: |
| Identifier | 71 | 70 | 68 | <100 |
| Ordinary lexical | 118 | 117 | 116 | <100 |
| Exact fallback | 102 | 102 | 101 | <750 |
| Hybrid without fallback | 274 | 274 | 272 | <500 |

Total-p95 values are 261/258/255 ms; population CV is **0.9494%**, below 10%.
Each of three fresh MCP processes used five warmup and 30 measured full-query
passes. The broad `ordinary-02` query remains the failing case. All other
qualification checks pass. Cross-run hosted-runner timing differences do not
establish a regression or improvement; this result fails on its own terms.

Fresh lexical ingest took 39.819644 seconds (2,511.32 passages/s); fresh
re-embedding took 8,984.080027 seconds (11.13 passages/s). Maximum search
process-tree RSS was 546.05 MiB; all samplers completed without error. Final
managed storage is 1,188,073,472 bytes; WAL is zero bytes. The report retains
cold startup, real OS-cache eviction, model load and first-hybrid measurements
separately from warm SLOs.

Candidate: `5ca552ccac73a8e989c2a3a21347e5f6acc5cf8d`.
Binary SHA-256: `6b922d22c578e1d76a02ac279abb77356ffb5ad004b86fbb9fba5832681e025c`.
`audit.json` retains recomputed outcomes; `files-sha256.json` hashes raw evidence.

Workflow: https://github.com/burkan2/hSUM/actions/runs/41410428972
