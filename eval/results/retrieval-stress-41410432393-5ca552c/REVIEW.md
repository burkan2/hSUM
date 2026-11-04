# Linux million-passage report-only stress — `5ca552c`

**Disposition: PASS for all ten report-only checks.** Independent AI review
recomputed all 80 probe summaries, found no deadlines, and verified one million
active passages, 64 sources, a single deduplicated body, cross-source embedding
reuse, 100 metadata-only generations, four retained old readers, eight
cancellations and successful recovery. This applies no million-passage latency
SLO and does not override the same candidate's failed 100k qualification.

The first observed scaling knee is the maximum-posting literal at eight sources
and 111,160 passages: p50 409.537 ms, 2.8442 times the preceding scale point.
Final managed storage is 4,336,054,272 bytes with a zero-byte WAL. Raw stage
timings, RSS, storage, runner capacity and workflow metadata are retained and
hashed in `files-sha256.json`.

Candidate: `5ca552ccac73a8e989c2a3a21347e5f6acc5cf8d`, recorded clean.
Binary SHA-256: `6b922d22c578e1d76a02ac279abb77356ffb5ad004b86fbb9fba5832681e025c`.

Workflow: https://github.com/burkan2/hSUM/actions/runs/41410432393
