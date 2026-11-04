# Linux million-passage report-only stress — `41af5c7`

**Disposition: PASS for all ten report-only checks.** The independent review
recomputed all 80 retained probe summaries and qualification checks. Every
probe returned evidence; no deadline occurred. Old-reader snapshots, 100
metadata generations, cross-source embedding reuse and cancellation recovery
pass. No million-passage latency SLO is applied.

Peak ingest process-tree RSS is 3,143,757,824 bytes (2.93 GiB); re-embed peak
is 595,599,360 bytes and cancel-storm peak is 696,172,544 bytes. Final managed
storage is 4,335,984,640 bytes (4.04 GiB), with a zero-byte WAL. The first
observed scaling knee is the maximum-posting literal at eight sources and
111,160 passages (2.863x the previous scale's median); this remains report-only.

Candidate: `41af5c711f2bacfd84f68b28b0d8c4ab47711ca6`. Raw report, harness log,
runner capacity and workflow metadata are retained with SHA-256 hashes.
The same candidate still fails Linux 100k ordinary lexical latency.

Workflow: https://github.com/burkan2/hSUM/actions/runs/41267703086
