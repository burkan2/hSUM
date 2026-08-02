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
- Evidence protocol v2 additionally records every reference `f32` component
  from the final deterministic call. These values are portability evidence,
  not production vector persistence. They allow component-wise, vector-norm,
  cosine, distance, and ordering comparisons instead of treating a digest
  mismatch as a numerical conclusion.
- Every v2 report carries `hsum.embedding-provenance.v1`: model ID, exact
  upstream repository and revision, canonical manifest hash, every artifact
  file checksum and byte length, dimension, CLS pooling, post-pooling L2
  normalization, IEEE-754 binary32 components, FastEmbed and `ort` crate
  versions, the linked ONNX Runtime version and build identity, CPU execution
  provider configuration, target OS/architecture/endianness, maximum length,
  graph optimization level, and intra-op thread count.

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

GitHub Actions run
[`30712610005`](https://github.com/burkan2/hSUM/actions/runs/30712610005)
executed the protocol from a clean pull-request merge checkout. The reports and
native dependency manifests are checked in alongside the earlier dirty-checkout
Apple M2 development report.

| Platform | Evidence | Result | Initialize | Batch-1 p95 | Batch-8 p95 | Batch-8 throughput | Peak RSS growth |
|---|---|---|---:|---:|---:|---:|---:|
| AMD EPYC 7763, Linux x86_64 | [JSON](results/linux-x86_64-gha-30712610005.json), [dependencies](results/linux-x86_64-gha-30712610005-dependencies.txt) | Pass | 282.4 ms | 10.26 ms | 72.69 ms | 112.0 docs/s | 313.1 MB |
| Apple M1 virtual, macOS arm64 | [JSON](results/macos-arm64-gha-30712610005.json), [dependencies](results/macos-arm64-gha-30712610005-dependencies.txt) | Pass | 212.2 ms | 15.44 ms | 144.63 ms | 65.0 docs/s | 362.9 MB |
| Apple M2, macOS arm64 | [dirty-checkout JSON](results/macos-arm64-local.json) | Pass | 149.4 ms | 7.79 ms | 90.42 ms | 96.9 docs/s | 394.3 MB |

The Linux executable is an x86-64 ELF dynamically linked only to the standard
GNU C/C++ runtime libraries. The macOS executable is an arm64 Mach-O linked
only to Apple system libraries and frameworks; neither requires a separately
installed ONNX Runtime library.

Outputs are deterministic across all 23 calls within each workload and runner,
but the embedding digests differ between x86_64 and arm64. This probe therefore
qualifies runtime portability, latency, and memory ceilings—not bit-for-bit
cross-architecture vector reproducibility. Before hSUM persists vectors, the
next slice must define a numerical compatibility tolerance or record enough
runtime provenance to regenerate an index consistently.
