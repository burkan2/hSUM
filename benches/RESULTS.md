# hSUM benchmark results

The frozen, machine-readable benchmark lives in `benches/agent_ab/`. Reproduce
the checked-in development baseline with:

```bash
python3 benches/agent_ab/benchmark.py validate
python3 benches/agent_ab/benchmark.py stability
```

## Frozen 25-query lexical baseline

- **Date:** 2026-08-01
- **Machine:** Apple M2, macOS 26.4.1
- **hSUM:** 0.1.0-alpha.4 built from the current checkout
- **Corpus:** deterministic isolated scope snapshot, 90 documents / 1,827 passages
- **Corpus commit:** `b556e60d8eee618ffee21deeeac5ef4e23ae405e`
- **Index builds:** 3 independent initializations; means and observed ranges reported
- **Manifest:** `hsum-public-gold-25-v1`, SHA-256
  `e4fe7d6ea861557f9a3c3077ec07b10bee46ef7a57ed54d915b2976301789c5b`
- **Qualification:** dirty working tree; development evidence, not a clean
  release or cross-repository claim

| Retriever | nDCG@5 | Hit@1 | Recall@5 | MRR | Median | Output |
|---|---:|---:|---:|---:|---:|---:|
| ripgrep `-C20`, sorted, one thread | 22.2% | 32.0% | 23.3% | 0.340 | **6.9 ms** | **111.9 KiB** |
| git grep `--untracked --exclude-standard -C20` | 22.5% | 36.0% | 23.3% | 0.360 | 15.8 ms | **111.9 KiB** |
| hSUM | **26.3%** (22.9–28.3) | **40.0%** (36.0–44.0) | **24.7%** (22.7–26.7) | **0.439** | 153.6 ms | 118.7 KiB |

The balanced taxonomy explains the change from the earlier 12-task pilot:

| Query class | hSUM Hit@1 | ripgrep Hit@1 | git grep Hit@1 |
|---|---:|---:|---:|
| Identifier | **100%** | **100%** | **100%** |
| Exact phrase | 60% | 60% | **80%** |
| Concept | **40%** | 0% | 0% |
| Paraphrase | 0% | 0% | 0% |
| Multi-evidence | 0% | 0% | 0% |

## Honest interpretation

hSUM has the strongest mean quality in this internal baseline, but it emits
6.0% more bytes, is 9.8× slower than git grep, and is 22.1× slower than
ripgrep. More importantly, its nDCG@5 varies from 22.9% to 28.3% across fresh index
builds of identical corpus bytes. The benchmark therefore supports a scoped
development result, not a general accuracy or deterministic-ranking claim.
Concept recovery is the clearest lexical differentiator; paraphrase and
multi-evidence remain complete failures.

The strongest current hSUM capability is outside ordinary live grep: immutable
citation resolution plus source-drift reporting. The deterministic drift demo
indexes an uncommitted value, overwrites the live file, and proves that hSUM can
still return the exact earlier bytes while reporting the source as changed.

Do not revive the superseded “91.7% Hit@1” 12-task pilot as a current claim.
Before public comparative claims, rerun from a clean commit, use at least five
external repositories and 100 tasks, obtain blind dual label review, publish
confidence intervals, and keep every raw result.
