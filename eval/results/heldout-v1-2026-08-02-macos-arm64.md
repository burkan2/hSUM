# hSUM stable-v0.1 held-out retrieval result

- Manifest: `a7771fac85a90450c4510846e001cd7f81d3c9b083f745addb8cb567756d4af1`
- Binary commit: `011d669e99935e5a0e6d8624002ebe97de4ab5c8`
- Queries: 100
- Disposition: **stable-lexical-hybrid-beta**
- External comparison complete: true

## Aggregate retrieval evidence

| Retriever | NDCG@10 | MRR@10 | Recall@10 | Exact top-3 recall | Median latency | Context bytes | Citation correctness | Setup |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| hsum-hybrid | 0.5241 | 0.4247 | 0.7300 | 0.4400 | 554.46 ms | 2367355 | 100.0% | 507730.45 ms |
| hsum-lexical | 0.2672 | 0.2389 | 0.3100 | 0.2600 | 116.20 ms | 606410 | 100.0% | 507730.45 ms |
| hsum-semantic | 0.4612 | 0.3860 | 0.6000 | 0.4200 | 452.48 ms | 2336799 | 100.0% | 507730.45 ms |
| qmd | 0.4475 | 0.4076 | 0.5700 | 0.5000 | 6897.06 ms | 340968 | n/a | 329395.15 ms |
| ripgrep | 0.2997 | 0.2895 | 0.3300 | 0.3100 | 8.53 ms | 84399 | n/a | 0.00 ms |

## Promotion gates

| Gate | Baseline | Estimate | 95% lower | 95% upper | Pass |
|---|---|---:|---:|---:|---:|
| Overall hybrid ndcg@10 non-inferiority | hsum-semantic | 0.0629 | -0.0114 | 0.1389 | true |
| Overall hybrid mrr@10 non-inferiority | hsum-semantic | 0.0387 | -0.0312 | 0.1113 | false |
| Semantic-subset hybrid NDCG@10 gain | hsum-lexical | 0.4371 | 0.3124 | 0.5666 | true |
| Exact-token hybrid top-3 | hsum-lexical | 0.6571 | n/a | n/a | false |

Grades 2 and 3 are relevant. NDCG gain is `2^grade - 1`; discount is `log2(rank + 1)`. Confidence intervals use the manifest's deterministic 10,000-resample paired bootstrap. ripgrep and QMD are report-only; QMD is path-scored because it does not expose hSUM byte-span citations.
