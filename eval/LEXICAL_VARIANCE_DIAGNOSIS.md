# Diagnosis of the frozen lexical cross-build variance

This note closes the diagnosis required by the canonical stable-v0.1 plan. It
does not change the frozen retrieval formula, tie order, labels, or historical
benchmark result.

## Observation

`benches/agent_ab/results/retrieval.json` contains three independent hSUM index
builds over the same Git corpus and the same 25 public tasks. Comparing each
task's `ranked_paths` across those builds gives:

| Retriever | Tasks with a changed ranked-path order |
|---|---:|
| hSUM | 7 / 25 |
| ripgrep (`--sort path`) | 0 / 25 |
| git grep | 0 / 25 |

The affected hSUM tasks are `mcp-response-cap`, `mass-delete-guard`,
`query-byte-limit`, `citation-grammar`, `sqlite-query-only-hardening`,
`retrieval-signals`, and `mcp-tool-surface`. Aggregate hSUM NDCG@5 ranges from
0.2292 to 0.2828, matching the previously published 5.4-point range.

## Cause

The evidence supports one direct causal chain:

1. A fresh source registration receives `SourceId::new_v4()` in
   `src/app/source_management.rs` (the same policy is used by root replacement
   in `src/app/project_management.rs`).
2. A newly observed document receives `Uuid::new_v4()` in each ingestion path
   in `src/store/generation.rs`.
3. `compare_fused_candidates` in `src/search/retrieval.rs` first compares the
   integer fusion score and matched exact-byte count. Equal candidates then
   fall through to `compare_passage_identity`, which orders by source ID,
   document ID, and start byte.
4. The IDs are durable inside one index, so repeated queries and cursor pages
   are deterministic. Independent indexes of identical bytes receive different
   IDs, so candidates tied on the public scoring signals can exchange order.

The seven changed task rankings are therefore not evidence of mutable corpus
bytes, SQLite row-order leakage, floating-point score drift, or non-determinism
inside one index. They are cross-index identity variance at an explicitly
defined final tie-break boundary.

## Stable-v0.1 disposition

No scoring change is made in response to this diagnosis. Replacing the durable
identity tie-breaker would change public rankings, cursor configuration
fingerprints, and the already frozen exact/lexical contract. The stable
held-out evaluation instead binds one binary commit, exact corpus blobs,
retrieval settings, labels, and query order, and reports paired query-level
confidence intervals.

Any future attempt to make independent rebuilds byte-order-equivalent should
be a separately preregistered retrieval revision—for example, by introducing a
deterministic immutable source/document identity before indexing—and must
re-freeze the cursor fingerprint and held-out baseline. Until then,
cross-build studies must continue to publish independent-build ranges rather
than selecting one favorable build.
