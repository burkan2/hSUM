# Fresh Mac 100k qualification — candidate `5ca552c`

**Disposition: PASS.** All 2,250 raw observations, fixed query and pass order,
summaries, exclusive SLOs and qualification flags were recomputed against the
frozen candidate harness by the author and an independent AI reviewer. The clean source checkout, binary, setup, model,
query manifest and real cold-cache helper remain bound to retained evidence.
The user launched the reviewed measurement script in Terminal for local sudo
authentication; measurement completed with exit 0. No thresholds, query order,
samples or candidate bounds changed.

| Class p95 (ms) | Run 1 | Run 2 | Run 3 | Exclusive limit |
| --- | ---: | ---: | ---: | ---: |
| Identifier | 40 | 70 | 50 | <100 |
| Ordinary lexical | 71 | 77 | 74 | <100 |
| Exact fallback | 49 | 62 | 51 | <750 |
| Hybrid without fallback | 184 | 248 | 200 | <500 |

Total-p95 values are 177/194/187 ms; population CV **3.7506%**, below 10%.
All three processes are fresh, with five warmup passes and 30 measured passes
of 25 queries. Peak measured process-tree RSS is 822,312,960 bytes; no sampler
errors occurred. Managed index storage is 1,188,003,840 bytes. Cold startup,
OS-cache eviction, model load and first hybrid are retained separately.

Preparation created a new corpus and index without cached chunk embeddings.
Verified pinned model files were cloned outside timed preparation. Lexical
ingest took 35.890087 seconds (2,786.28 passages/s); fresh re-embedding took
9,708.027323 seconds (10.30 passages/s). Original crash and recovered-index
evidence were untouched.

Candidate: `5ca552ccac73a8e989c2a3a21347e5f6acc5cf8d`.
Binary SHA-256: `8f6867f4ede78ae89c6baa6e5183910a8a260908d7ca2f642dda1ba10ad40b59`.
`audit.json` contains recomputed outcomes and `files-sha256.json` hashes all
retained raw reports, launch/preparation receipts and logs. This Mac pass does
not overcome the same candidate's Linux ordinary lexical failure.
