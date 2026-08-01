# FastEmbed CPU portability probe

This opt-in probe answers one narrow question before hSUM stores vectors or
exposes semantic search: can the exact manifest-verified BGE-small bytes load
through FastEmbed and produce bounded, deterministic CPU embeddings on the two
supported release architectures?

The probe is deliberately outside the public `hsum` command surface. The
`model-portability` Cargo feature is disabled by default, FastEmbed's Hugging
Face feature is absent, and the adapter accepts only bytes read from an hSUM
artifact that has passed the embedded manifest, exact-tree, size, and SHA-256
checks. The adapter re-hashes every opened byte buffer before ONNX
initialization and fixes BGE-small's CLS pooling contract explicitly.

No vectors are persisted, no SQLite vector extension is linked, and no
semantic or hybrid mode is added by this probe.

## Preregistered protocol

- Model: `bge-small-en-v1-5-fp32`, manifest SHA-256
  `b01ac36f13dbf65c0ecbd96c0025d20a7442a248704d7f43a553861d93ba833f`
- Runtime: FastEmbed `5.17.4`, CPU execution provider, four intra-op threads
- Inputs: the eight checked-in strings in `inputs.json`
- Token limit: 512
- Warmup: three calls per workload
- Measurement: 20 calls per workload
- Workloads: one interactive document and one eight-document ingest batch
- Memory: current process RSS sampled every 20 ms from `/proc/self/status` on
  Linux and `ps` on macOS
- Output checks: count, dimension 384, finite values, unit L2 norm, and one
  stable SHA-256 digest across warmups and measured iterations

The portability ceiling is intentionally looser than the eventual hybrid
search SLO because this slice isolates runtime viability rather than retrieval
quality:

| Check | Blocking ceiling |
|---|---:|
| ONNX initialization | 10,000 ms |
| Interactive batch-1 p95 | 500 ms |
| Batch-8 p95 | 1,500 ms |
| Peak RSS growth | 1 GiB |

## Reproduce locally

From the repository root, choose a new absolute `HSUM_HOME` and output path;
reports refuse to overwrite existing evidence:

```bash
export HSUM_HOME="$PWD/target/model-portability-home"

cargo +1.91.0 build --locked --release --bin hsum
target/release/hsum model install embedding bge-small-en-v1-5-fp32 \
  --no-progress --json

cargo +1.91.0 build --locked --release \
  --features model-portability --example model-portability

HSUM_OFFLINE=1 target/release/examples/model-portability \
  --output "$PWD/target/model-portability-result.json"
```

`HSUM_OFFLINE=1` is defense in depth. The inference feature graph contains no
`hf-hub`; the only FastEmbed route in the adapter is
`try_new_from_user_defined` over verified in-memory bytes.

## Current evidence

The checked-in [macOS arm64 report](results/macos-arm64-local.json) is local
development evidence from a dirty checkout, not clean-runner or release
qualification.

| Platform | Result | Initialize | Batch-1 p95 | Batch-8 p95 | Batch-8 throughput | Peak RSS growth |
|---|---|---:|---:|---:|---:|---:|
| Apple M2, macOS arm64 | Pass | 149.4 ms | 7.79 ms | 90.42 ms | 96.9 docs/s | 394.3 MB |
| Ubuntu 24.04, Linux x86_64 | Pending clean-runner execution | — | — | — | — | — |

The macOS release-mode probe is a 31 MB arm64 Mach-O whose `otool -L` output
contains only Apple system libraries and frameworks; it does not require a
separate ONNX Runtime dylib. Linux remains explicitly unqualified until the
manual `Model portability` workflow runs on GitHub's native x86_64 runner and
publishes its JSON plus `ldd` evidence.
