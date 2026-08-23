# hSUM evidence benchmark

This suite measures three separate questions:

1. **Retrieval quality:** does the tool rank a gold-relevant document first?
2. **Agent answer quality:** does the same model produce correct facts backed by
   relevant, resolvable evidence?
3. **Source-drift recovery:** can the workflow recover exact uncommitted bytes
   after the live file changes?

The benchmark is dependency-free Python. Agent trials additionally require an
authenticated Codex CLI. Results are JSON first; HTML and SVG are derived
views.

## Reproduce

From the repository root:

```bash
python3 -m unittest -v benches/agent_ab/test_benchmark.py

python3 benches/agent_ab/benchmark.py validate

python3 benches/agent_ab/benchmark.py stability

python3 benches/agent_ab/benchmark.py agent \
  --condition both --agent-mode precollected --max-tasks 4

python3 benches/agent_ab/benchmark.py drift

python3 benches/agent_ab/benchmark.py render \
  benches/agent_ab/results/retrieval.json \
  --output benches/agent_ab/results/retrieval-dashboard.html
```

Use `agent --agent-mode tool` only as a secondary adoption test. The primary
`precollected` protocol gives the same model one capped, tool-free evidence
bundle produced by ripgrep or hSUM. This isolates context quality from the
agent's willingness and ability to operate a tool.

`stability` performs three independent `baseline` builds. Each build copies the
manifest's frozen corpus scope into the same deterministic clean Git snapshot,
initializes a fresh isolated hSUM home, and runs hSUM, ripgrep, and git grep
against exactly those files. Benchmark data and prior result files are never
indexed as query-bearing distractors. The result publishes the mean and min/max
range across builds because hSUM currently changes some equal-score ordering
when fresh index identities are generated. The lower-level `baseline` and
`retrieval` commands exist for diagnosis, not headline numbers.

## Metrics

| Metric | Meaning |
|---|---|
| Hit@1 | The first deduplicated document is gold-relevant. |
| Precision@5 | Relevant documents among five available ranking slots. |
| Recall@5 | Gold-relevant documents recovered in the top five. |
| MRR | Reciprocal rank of the first relevant document. |
| nDCG@5 | Primary ranking metric using grade 2 for a direct authoritative answer, grade 1 for supporting context, and grade 0 otherwise. |
| Fact accuracy | Required answer patterns present, scored independently. |
| Evidence precision | Submitted evidence paths that are gold-relevant. |
| Citation validity | Submitted hSUM citations that resolve successfully. |
| Grounded task success | All required facts, relevant evidence, and every required citation are valid. |
| Context bytes | Evidence payload supplied to the model, capped before inference. |
| End-to-end latency | Retrieval plus model response time. |

Duplicate passages from the same document are collapsed before retrieval
scoring. A correct guess without relevant evidence is not a successful agent
task. An otherwise correct hSUM answer with a malformed citation also fails the
strict success metric.

## Frozen local lexical baseline (2026-08-01)

Machine: Apple M2, macOS 26.4.1. Corpus: a clean, deterministic snapshot of the
frozen scope from the current working tree, 90 indexed documents / 1,827
passages. The source working tree was dirty; the isolated corpus commit was
`b556e60d8eee618ffee21deeeac5ef4e23ae405e`. The manifest contains 25 tasks:
five each for identifiers, exact phrases, concepts, paraphrases, and
multi-document questions. This is development evidence, not a clean-release
or cross-repository claim.

The manifest is pinned by `tasks.sha256`. `benchmark.py validate` checks the
task count, balanced taxonomy, graded labels, answer regexes, repository paths,
and checksum before a result is accepted.

The values below are means across three independent index builds; parentheses
show the observed build range.

| Retriever | nDCG@5 | Hit@1 | Recall@5 | MRR | Median | Output |
|---|---:|---:|---:|---:|---:|---:|
| ripgrep `-C20`, sorted, one thread | 22.2% | 32.0% | 23.3% | 0.340 | **6.9 ms** | **111.9 KiB** |
| git grep `--untracked --exclude-standard -C20` | 22.5% | 36.0% | 23.3% | 0.360 | 15.8 ms (15.4–16.0) | **111.9 KiB** |
| hSUM | **26.3%** (22.9–28.3) | **40.0%** (36.0–44.0) | **24.7%** (22.7–26.7) | **0.439** (0.400–0.473) | 153.6 ms (152.2–156.0) | 118.7 KiB (116.5–120.7) |

hSUM has the best mean quality in this scoped baseline, but not the best speed,
payload size, or rebuild stability. It is the only retriever with
concept-query success (40% Hit@1); it still scores 0% for both paraphrase and
multi-evidence tasks. Identifier nDCG remains lower than both grep baselines.
Those failures and the 5.4-point hSUM nDCG build range are frozen evidence for
later query expansion, stable tie-breaking, reranking, and semantic experiments.

### Historical same-model answer pilot, 4 pre-freeze tasks

This July 31 pilot predates the 25-task manifest. It remains useful for testing
the harness but must not be combined with the frozen retrieval baseline or
presented as a current 25-task agent result. Conditions used capped,
precollected evidence and no model tool calls.

| Condition | Fact accuracy | Strict grounded success | Mean end-to-end time |
|---|---:|---:|---:|
| ripgrep evidence | 25.0% | 25.0% | 7.6 s |
| hSUM evidence | **75.0%** | **50.0%** | 9.8 s |

One otherwise correct hSUM answer failed strict success because the model
corrupted one copied citation. Keep this failure in published raw results. It
is a useful hSUM UX/product metric, not noise to remove. Machine-local home
paths in captured process diagnostics are normalized to `<HOME>`; metrics,
answers, and citations are otherwise unchanged.

### Deterministic drift demo

```text
BEFORE: RETRY_LIMIT = 3 (uncommitted)
EDIT: RETRY_LIMIT = 9
WITHOUT hSUM: current = 9; previous value = unknown
WITH hSUM: previous = 3; current = 9; source = changed
```

This is the strongest demo because it has an objective oracle and does not
need an LLM judge.

## Claim rules

Safe wording for the current frozen development baseline:

> Across three fresh index builds of one frozen 98-document hSUM-repository
> snapshot and 25 author-labeled tasks, hSUM averaged 26.3% nDCG@5 and 40.0%
> Hit@1, versus 22.2% / 32.0% for ripgrep and 22.5% / 36.0% for git grep. hSUM
> returned 6.0% more bytes, and median lookup time was 153.6 ms versus 6.9 ms
> and 15.8 ms. hSUM's nDCG@5 ranged from 22.9% to 28.3% across fresh builds,
> and all retrievers failed the five paraphrase and five multi-evidence tasks.

Do not shorten this to “hSUM is more accurate” or “uses fewer tokens.” It is an
internal, author-labeled development result with unresolved rebuild variance,
not a general code-search claim. The strongest current product demo remains
deterministic source-drift recovery: hSUM can resolve exact indexed bytes after
an uncommitted live file changes, while live grep cannot recover overwritten
bytes.

Do not remove the task count, repository scope, independent-build range,
source-working-tree qualification, failed query classes, baseline
configuration, or latency trade-off from the development statement.

Before a launch-wide retrieval claim, rerun from a clean commit and expand to at
least 100 tasks across 5 or more external repositories. Have relevance labels
reviewed blind by two people, report bootstrap confidence intervals, and
publish every task and raw response.
