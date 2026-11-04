# Native standard gates — 2026-10-24

All seven recorded workflows succeed at `99d51f9f928c790a4f968dbc0f33e95e10bc13f0`.
The production Rust sources, Cargo manifests/lockfile, build script, migrations
and Cargo configuration have no diff from repair candidate
`b6591b7405320c924dc341111006d8d87b1e0c8a`.

[CI 40040184801](https://github.com/burkan2/hSUM/actions/runs/40040184801)
passes its Linux x86_64 and macOS arm64 contributor gates. Both retained job
logs explicitly contain the passing product cross-process WAL lock regression.
Vector portability, model portability, clean-machine candidate trials,
retrieval determinism, parser fuzz smoke and dependency policy also succeed;
`workflows.json` retains exact run IDs and head hashes.

The initial `40039660073` failure remains recorded in the repair diagnosis:
it stopped on the stale ledger headline before Rust tests. This success
includes the separate 222/316 = 70.3% correction. No scale or fresh-preparation
qualification, independent source review, or release pass follows from CI.

The GitHub CLI combined log fetch returned empty with exit zero. Direct
per-job API logs are retained as `linux.log` and `macos.log`; no missing combined
log is used as evidence. Assertions and source comparison are recorded in
`review-receipt.json`.
