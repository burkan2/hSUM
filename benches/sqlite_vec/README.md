# sqlite-vec portability and filtered-KNN spike

This opt-in probe resolves the canonical storage prerequisite before hSUM adds
any production vector schema or semantic search surface. It compiles sqlite-vec,
bundled SQLite/FTS5, FastEmbed, and ONNX Runtime into one release executable and
exercises the two stable release architectures.

No product vector is persisted by this spike. Its temporary database is removed
when the process exits, and the `vector-portability` feature is disabled by
default.

## Dependency disposition

The current crates.io alpha, `sqlite-vec 0.1.10-alpha.4`, is not a usable Rust
package: its default C amalgamation includes `sqlite-vec-diskann.c`, but that
file is absent from the published crate. The exact compiler failure is retained
in [the build-failure record](results/sqlite-vec-0.1.10-alpha.4-build-failure.md).
hSUM does not vendor or silently repair the missing third-party source.

The behavioral spike therefore pins the latest non-alpha release,
`sqlite-vec 0.1.7`. It builds successfully as a statically registered SQLite
extension and declares `MIT/Apache-2.0` licensing. The locked dependency remains
subject to the stable supply-chain and license reviews.

## Preregistered protocol

- Vector dimension: 384 float32 components, cosine distance
- Scope: 64 source partition keys, 64 rows per source
- Candidate path: per-source filtered KNN, 50 returned rows per source, exactly
  3,200 merged initial candidates
- Filter adversary: the selected source's true nearest row is outside the
  global unfiltered top 50
- Cutoff adversary: 64 equal-distance rows around a depth-50 cutoff, rebuilt in
  opposite insertion orders
- Concurrency: a WAL reader spans a complete shadow-slot build and metadata flip
- Cancellation: 32,768-row vector scan interrupted from an owned request signal
- Failure paths: absent extension, wrong dimension, corrupted vec0 shadow data
- Benchmark: five warmups and 30 measured 64-source fan-outs; nearest-rank p95
- Blocking ceilings: 500 ms fan-out p95 and 1 GiB RSS growth

## Raw backend result and required storage revision

Raw `sqlite-vec 0.1.7` passes static registration, filter-before-limit, the
global-top-50 adversary, WAL reader visibility, shadow-slot flipping, fan-out,
memory, dimension refusal, and corruption refusal. It does **not** provide the
canonical total order at an equal-distance cutoff: selected membership and
order change with insertion order. It also converts an interrupted chunk scan
to the generic error `chunks iter error` instead of preserving
`SQLITE_INTERRUPT`.

The canonical plan expressly requires a storage revision when either property
is unavailable. The accepted narrow revision is:

1. Query each source and active vector slot for `K + 1` rows.
2. If distances at positions `K` and `K + 1` differ, sort the first `K` rows by
   finite distance and passage row ID.
3. If those boundary distances are equal, run a source- and slot-scoped exact
   `vec_distance_cosine` query ordered by distance and passage row ID, then take
   `K`. Global KNN followed by filtering remains forbidden.
4. Treat sqlite-vec's generic interrupted-scan error as `CANCELLED` or `TIMEOUT`
   only when hSUM's owned cancellation/deadline state fired for that operation
   and the same query was healthy before interruption. Otherwise retain the
   error as an integrity failure.

This adapter produces the required cutoff membership and order across opposite
rebuild orders. The normal 64-source workload includes one real tie fallback on
every pass. An all-64-sources-tied development stress run measured 674.68 ms p95
and is retained as a duplicate-heavy knee; it is not blended into the normal
fan-out result.

## Local evidence

The Apple M2 development checkout produced three retained reports:

| Evidence | Result | Important observation |
|---|---|---|
| [Raw backend](results/macos-arm64-local-raw-backend-v1.json) | Fail | insertion-order-dependent cutoff; generic interrupt error |
| [All-source tie stress](results/macos-arm64-local-all-sources-tied-stress-v2.json) | Report-only fail | revised correctness passes; fan-out p95 674.68 ms |
| [Storage revision](results/macos-arm64-local-storage-revision-v2.json), [dependencies](results/macos-arm64-local-storage-revision-v2-dependencies.txt) | Pass | 3,200 candidates, fan-out p95 54.43 ms, 9.32 MB RSS growth |

The composite macOS executable is arm64 Mach-O and links only Apple system
libraries/frameworks plus the standard C++ runtime; neither sqlite-vec nor ONNX
Runtime needs a separately installed dynamic library.

## Reproduce

Choose a new output path because reports refuse to overwrite evidence:

```bash
cargo +1.91.0 test --locked \
  --features vector-portability \
  --example sqlite-vec-portability

cargo +1.91.0 build --locked --release \
  --features vector-portability \
  --example sqlite-vec-portability

HSUM_OFFLINE=1 target/release/examples/sqlite-vec-portability \
  --output "$PWD/target/sqlite-vec-portability.json"
```

`.github/workflows/vector-portability.yml` repeats the optimized probe and
dynamic-dependency inspection on Linux x86_64 and macOS arm64, then requires an
identical revised cutoff result and dependency identity in a final fan-in job.
