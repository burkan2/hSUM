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

The v2 cross-architecture comparator also preregisters these blocking numerical
ceilings. It compares all 3,456 recorded components, every corresponding
vector, the full batch pairwise squared-L2 matrix, and each query-to-batch
ranking:

| Check | Blocking ceiling |
|---|---:|
| Maximum component absolute delta | `1e-5` |
| Maximum corresponding-vector L2 delta | `2e-4` |
| Maximum cosine distance | `1e-6` |
| Maximum pairwise squared-L2 distance delta | `2e-6` |
| Deterministic ordering | Identical |

Reports must also have compatible provenance. Target OS and architecture are
expected to differ; model identity, artifact checksums, pooling, normalization,
precision, runtime release/build identity, execution provider, endianness,
maximum length, and thread configuration must match.

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

Compare two new v2 reports with the same preregistered gate:

```bash
cargo +1.91.0 run --locked --release \
  --example model-portability-compare -- \
  /path/to/linux-x86_64.json \
  /path/to/macos-arm64.json \
  --output "$PWD/target/model-numerical-compatibility.json"
```

`HSUM_OFFLINE=1` is defense in depth. The inference feature graph contains no
`hf-hub`; the only FastEmbed route in the adapter is
`try_new_from_user_defined` over verified in-memory bytes.

## Current evidence

GitHub Actions run
[`30731369195`](https://github.com/burkan2/hSUM/actions/runs/30731369195)
executed the v2 protocol from a clean pull-request merge checkout. Both reports,
their native dependency manifests, and the preregistered cross-architecture
comparison are checked in alongside the earlier reports.

| Platform | Evidence | Result | Initialize | Batch-1 p95 | Batch-8 p95 | Batch-8 throughput | Peak RSS growth |
|---|---|---|---:|---:|---:|---:|---:|
| AMD EPYC 7763, Linux x86_64 | [JSON](results/linux-x86_64-gha-30731369195.json), [dependencies](results/linux-x86_64-gha-30731369195-dependencies.txt) | Pass | 259.7 ms | 10.29 ms | 72.08 ms | 112.6 docs/s | 319.5 MB |
| Apple M1 virtual, macOS arm64 | [JSON](results/macos-arm64-gha-30731369195.json), [dependencies](results/macos-arm64-gha-30731369195-dependencies.txt) | Pass | 237.2 ms | 9.00 ms | 130.73 ms | 90.0 docs/s | 493.2 MB |
| Apple M2, macOS arm64 | [dirty-checkout JSON](results/macos-arm64-local.json) | Pass | 149.4 ms | 7.79 ms | 90.42 ms | 96.9 docs/s | 394.3 MB |

The [cross-architecture report](results/cross-architecture-gha-30731369195.json)
passes every preregistered gate: maximum component delta `1.00e-7`, vector L2
delta `6.94e-7`, cosine distance `2.32e-13`, and pairwise squared-L2 distance
delta `2.89e-7`, with zero ordering mismatches. The smallest adjacent ranking
gap was `1.16e-3`.

The Linux executable is an x86-64 ELF dynamically linked only to the standard
GNU C/C++ runtime libraries. The macOS executable is an arm64 Mach-O linked
only to Apple system libraries and frameworks; neither requires a separately
installed ONNX Runtime library.

Outputs are deterministic across all 23 calls within each workload and runner.
The embedding digests differ between x86_64 and arm64 because digest equality
is bit-for-bit, but every observed numerical difference is inside the frozen
compatibility contract and preserves deterministic ranking. The reports also
carry enough provenance to detect incompatible model/runtime/index inputs.
This completes the numerical prerequisite for the sqlite-vec portability spike;
it does not itself persist vectors or expose semantic retrieval.
