# Beta.1 model-artifact lifecycle checkpoint

**Date:** 2026-08-01
**Status:** Implemented in the current checkout; vector inference remains gated

## Decision

The first embedded profile is `bge-small-en-v1-5-fp32`, backed by
`Xenova/bge-small-en-v1.5` at commit
`ea104dacec62c0de699686887e3f920caeb4f3e3`. It is a 384-dimensional,
FastEmbed-compatible ONNX export of `BAAI/bge-small-en-v1.5` under the MIT
license. The five required files total 133,806,060 bytes.

This profile is the first portability candidate, not evidence that semantic
retrieval is ready. BGE-small was selected over multilingual E5-small for this
checkpoint because both expose 384-dimensional embeddings, while the BGE
artifact is materially smaller and is FastEmbed's default English profile.
Multilingual behavior remains a product and benchmark decision before a stable
default is declared.

## Contract

- The binary embeds schema version, model ID, artifact kind, exact upstream
  repository and revision, vector dimension, SPDX license identifier, HTTPS
  source, expected relative paths, byte lengths, and SHA-256 checksums.
- The canonical manifest SHA-256 addresses the cache directory. A matching
  human model ID with different metadata or bytes is not interchangeable.
- `model install embedding <id>` is the only network path. It uses HTTPS-only
  requests, bounded redirects and timeouts, platform certificate verification,
  standard system proxies, optional PEM roots, three transient retries, and a
  same-filesystem staging directory.
- `model import` accepts either an artifact directory containing
  `hsum-model.json` or that receipt file. It verifies the same embedded
  contract and exact file tree before copying and atomically publishing.
- `model list` does not hash the 134 MB artifact; it validates receipt, exact
  tree, and sizes. `model verify` is the explicit full checksum pass.
- Removal scans every discoverable managed index and refuses a matching or
  conservatively unreadable non-lexical pin unless `--force` is explicit.

## Still gated

- FastEmbed/ONNX initialization from these verified bytes
- macOS arm64 and Linux x86_64 inference latency and memory measurements
- sqlite-vec packaging and filtered-scope correctness
- deterministic equal-distance ordering and cancellation
- index embedding pins and the five-state semantic lifecycle
- `--embedding-model`, `--reembed`, `semantic`, and `hybrid` CLI/API exposure
