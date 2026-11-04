# Stable-v0.1 held-out retrieval evaluation

This directory owns the public, frozen retrieval gate for hSUM stable-v0.1.
It is separate from `benches/agent_ab/`, which remains historical development
evidence and must not be used for stable promotion. The historical fresh-index
rank variance is closed as a diagnosis, without a scoring change, in
`LEXICAL_VARIANCE_DIAGNOSIS.md`.

## Frozen contract

`manifest.toml` binds the evaluation to one full Git commit, every corpus blob,
all four migrations, the embedded model manifest, query and label order, FTS
tokenizer, chunk settings, retrieval weights, candidate limits, tool versions,
metric definitions, and bootstrap parameters.

The public set contains exactly 100 author-labeled tasks across three
independently structured corpora:

- 35 exact-token tasks;
- 35 semantic/paraphrase tasks; and
- 30 lifecycle, failure, scope, deduplication, and adversarial scenarios.

Accepted evidence is labeled as an exact byte span. Grades are `0` irrelevant,
`1` supporting context, `2` partial answer, and `3` direct answer. Grades 2 and
3 count as relevant for recall and MRR. NDCG uses gain `2^grade - 1`, discount
`log2(rank + 1)`, and every accepted label in the ideal ranking.

The harness validates the required quoted-error, identifier, paraphrase,
duplicate-content, rename, deletion, stale/current, project-scope, and
true-neighbor-outside-global-top-50 cases before any tool runs.

## Promotion rule

Promotion requires all of the following:

1. The lower bound of a deterministic 10,000-resample paired 95% bootstrap for
   hybrid NDCG@10 and MRR@10 versus the better single hSUM mode is at least
   `-0.02`.
2. On the semantic/paraphrase subset, hybrid improves mean NDCG@10 over lexical
   by at least `0.05` and the paired lower bound is at least zero.
3. Every exact-token query has accepted evidence in hybrid's top three, and
   hybrid exact-token top-3 recall does not regress from lexical.

If any condition fails, the canonical disposition is
`stable-lexical-hybrid-beta`. ripgrep and QMD are report-only comparisons and
cannot promote a product mode.

## Reproduce

The validator and metric tests require Python 3.11 or newer and use only its
standard library. Repository CI and release jobs pin Python 3.13:

```bash
python3 -m unittest -v eval/test_harness.py
python3 eval/harness.py validate
```

Build the binary from the manifest's `binary_commit`, then explicitly prepare
the frozen hSUM model in a new directory. This is the only hSUM evaluation step
that may use the network:

```bash
cargo +1.91.0 build --locked --release --bin hsum
python3 eval/harness.py prepare-model \
  --hsum target/release/hsum \
  --home target/eval-model-seed
```

For the QMD report, install its pinned CLI into an ignored local directory:

```bash
npm install --prefix target/eval-tools @tobilu/qmd@2.5.3
```

Run from empty work and output paths:

```bash
python3 eval/harness.py run \
  --hsum target/release/hsum \
  --model-home target/eval-model-seed \
  --qmd target/eval-tools/node_modules/.bin/qmd \
  --work target/eval-work \
  --output target/eval-result.json \
  --runs 1
```

The run copies the verified hSUM model into its isolated home and sets
`HSUM_OFFLINE=1` before initialization, ingestion, and retrieval. It
materializes corpus bytes directly from Git and creates only an empty `.git`
repository boundary beside them. QMD uses an isolated config/cache home and the
manifest-pinned
`hf:ggml-org/embeddinggemma-300M-GGUF/embeddinggemma-300M-Q8_0.gguf` embedding
model and
`hf:tobil/qmd-query-expansion-1.7B-gguf/qmd-query-expansion-1.7B-q4_k_m.gguf`
generation model; its first setup may download QMD-owned runtime/model
artifacts.

Result files include every ranked top-10 response, per-task metrics and
latencies, context bytes, observed hSUM citation round-trip correctness,
retriever aggregates, setup time, exact tool versions, binary checksum, and the
promotion disposition. Outputs are never overwritten. `compare` refuses two
results unless their manifest, commit, protocol, task order, and labels match:

```bash
python3 eval/harness.py compare first.json second.json
```

Render a deterministic Markdown summary without recalculating metrics:

```bash
python3 eval/harness.py render target/eval-result.json \
  --output target/eval-result.md
```

## Published result

[`results/heldout-v1-2026-08-02-macos-arm64.json`](results/heldout-v1-2026-08-02-macos-arm64.json)
is the complete raw local run over binary commit
`011d669e99935e5a0e6d8624002ebe97de4ab5c8` and manifest
`a7771fac85a90450c4510846e001cd7f81d3c9b083f745addb8cb567756d4af1`.
Its deterministic [Markdown projection](results/heldout-v1-2026-08-02-macos-arm64.md)
records the required lexical-first / hybrid-beta disposition: semantic value
and NDCG non-inferiority pass, while MRR non-inferiority and exact-token
top-three non-regression fail. The result is promotion evidence only; it is not
cross-target or stable-release qualification.
