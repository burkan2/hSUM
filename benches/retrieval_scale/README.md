# 100k retrieval qualification

This harness implements the stable-v0.1 performance protocol from
`work/local-rust-evidence-bus-design.md`. It is a release qualification tool,
not a synthetic microbenchmark and not a CI timing gate.

## Frozen protocol

- 10,000 deterministic 15,000-byte text documents produce exactly 10 chunks
  each under the frozen 1,200/1,800/180 chunk contract: 100,000 active chunks.
- The fixed manifest contains 25 queries across identifier, ordinary lexical,
  raw exact-fallback, and hybrid-without-fallback classes.
- Each fresh MCP process performs five unmeasured full-query passes followed by
  30 measured passes, yielding 750 observations per run. The complete sequence
  runs three times.
- Percentiles use nearest-rank `ceil(p*n)`. Each class reports p50, p95, and
  worst for query embedding, exact, exact fallback, lexical, vector, fusion,
  body materialization, server total, and client round trip.
- Cold process startup, cold OS-cache lexical search, first model load, and
  first hybrid search remain separate from warm samples.
- The report includes all raw observations, per-run p95s, process-tree peak
  RSS, logical live/history/vector payload bytes, managed database bytes, and
  lexical ingest/re-embedding throughput.

The harness fails closed when the corpus cardinality, effective mode,
retriever class, timing schema, result presence, SLO, or run-to-run coefficient
of variation does not match the protocol. `--allow-missing-cold-cache` exists
only for harness development and makes the final qualification fail.

## Prepare

Build the release binary, create an isolated `HSUM_HOME`, and install the
pinned model explicitly. Model acquisition is outside ingest timing by design:

```bash
cargo +1.91.0 build --locked --release
mkdir -p /absolute/path/to/scale-home
HSUM_HOME=/absolute/path/to/scale-home \
  target/release/hsum model install embedding bge-small-en-v1-5-fp32

python3 benches/retrieval_scale/harness.py prepare \
  --hsum "$PWD/target/release/hsum" \
  --corpus /absolute/path/to/scale-corpus \
  --home /absolute/path/to/scale-home \
  --model-id bge-small-en-v1-5-fp32
```

`prepare` refuses a non-empty corpus directory and never deletes an existing
index. It generates roughly 150 MB of source text, verifies the already-staged
model offline, initializes the real filesystem product path, checks for
exactly 100,000 active passages, runs the real bounded re-embedding path, and
writes `.hsum-retrieval-scale-setup.json` in the corpus root.

## Cold-cache helper

Full qualification requires an executable helper that drops the operating
system file cache. The helper receives the absolute index database path as its
only argument. Cache eviction is necessarily platform/authority specific, so
the harness records both its path and SHA-256 instead of pretending a portable
userspace operation is equivalent.

For example, an operator-owned macOS helper can run `sudo purge`; a Linux CI
helper can run `sync` and write `3` to `/proc/sys/vm/drop_caches` through its
approved privilege boundary. Authenticate or provision that privilege before
starting the benchmark—an interactive prompt would contaminate the result.

## Run

```bash
python3 benches/retrieval_scale/harness.py run \
  --hsum "$PWD/target/release/hsum" \
  --corpus /absolute/path/to/scale-corpus \
  --home /absolute/path/to/scale-home \
  --cold-cache-helper /absolute/path/to/drop-hsum-cache \
  --output benches/retrieval_scale/results/macos-arm64-m2-run.json
```

Run the canonical command on Apple M2/16 GB and the release Linux x86_64
runner. Do not compare or merge observations across machines. Commit a reviewed
summary and the immutable raw reports only after both native runs complete;
mere harness availability is implementation progress, not performance evidence.

Fast input checks and unit tests are part of `cargo xtask check`:

```bash
python3 benches/retrieval_scale/harness.py validate
python3 -m unittest -v benches/retrieval_scale/test_harness.py
```
