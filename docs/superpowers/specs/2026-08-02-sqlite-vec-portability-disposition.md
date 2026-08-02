# sqlite-vec portability disposition and storage revision

**Date:** 2026-08-02
**Status:** Accepted canonical Beta.1 storage revision; native evidence passing
**Authority:** The failure branch in `work/local-rust-evidence-bus-design.md`

## Decision

The raw sqlite-vec backend does not satisfy hSUM's deterministic equal-distance
cutoff or typed interruption requirements. Production vector storage may
proceed only through the bounded adapter below and only after the native matrix
passes. No global KNN-then-filter implementation is allowed.

The candidate dependency is pinned to `sqlite-vec 0.1.7`. The newer published
`0.1.10-alpha.4` Rust crate is rejected because its default build references a
missing C source file. This decision does not authorize vendoring or modifying
upstream C code.

## Required adapter contract

For source `S`, active vector slot `V`, requested candidate depth `K = 50`, and
query vector `Q`:

1. Execute vec0 KNN with both `S` and `V` constrained before the search limit,
   requesting `K + 1` rows.
2. Reject non-finite distances before ordering.
3. If row `K + 1` is absent or its distance differs from row `K`, sort the first
   `K` rows by `f32::total_cmp(distance)` and passage row ID ascending.
4. If the two boundary distances are equal, execute an exact scalar distance
   scan constrained to `S` and `V`, order by distance and passage row ID, and
   take `K`.
5. Merge at most 50 returned candidates from each of at most 64 project sources.
   The public initial-candidate bound remains 3,200.

The fallback is a correctness path, not a new ranking signal. Raw distance is
still explanation-only; deterministic integer RRF remains unchanged.

## Interruption contract

sqlite-vec `0.1.7` may surface an interrupted chunk iterator as the generic SQL
error `chunks iter error`. The adapter may translate that result to `CANCELLED`
or `TIMEOUT` only when all of these are true:

- hSUM installed and owns the interrupt handle for this request;
- the request cancellation token or deadline state fired before the SQL result;
- the same prepared query was valid before the interrupt attempt; and
- the connection passes its post-interrupt health check.

Without that owned state, the error remains an integrity failure. Cancellation
and timeout remain distinct public outcomes.

## Evidence and consequences

The opt-in release probe records the raw failure and the revised pass. It covers
the true-nearest-outside-global-top-50 adversary, opposite insertion orders,
WAL reader snapshots, shadow-slot flipping, 64-source fan-out, cancellation,
wrong dimensions, corrupt shadow storage, RSS, database bytes, and native
dynamic dependencies.

Linux x86_64 and macOS arm64 both passed
[workflow run 30732989326](https://github.com/burkan2/hSUM/actions/runs/30732989326),
including its cross-target fan-in. The retained reports live under
`benches/sqlite_vec/results/`; Linux measured 64.54 ms fan-out p95 and 8.01 MB
RSS growth, while macOS measured 46.75 ms and 10.90 MB. Both returned exactly
3,200 candidates with identical revised tie-boundary membership and ordering.

Production vector persistence is therefore unblocked. Implementation must
preserve this adapter as an explicit tested boundary; a future sqlite-vec
upgrade cannot remove it without rerunning the raw cutoff and interruption
probes and publishing a new disposition.
