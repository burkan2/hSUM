# Token, cost, and answer-quality pilot — 2026-10-21

hSUM used 4.2% fewer input tokens and 71.3% more output tokens than the
precollected grep condition. Reported cost was 11.0% higher. Strict task success
was 7/25 versus 6/25 after correcting the scorer. This exploratory run does
not establish a cost or quality advantage.

## Measurements

| Metric, all 25 tasks per condition | Grep | hSUM lexical |
|---|---:|---:|
| Input tokens, including cache writes and reads | 78,025 | 74,755 |
| Uncached input | 50 | 50 |
| Cache-write input | 60,911 | 56,930 |
| Cache-read input | 17,064 | 17,775 |
| Output tokens | 7,095 | 12,155 |
| CLI-reported API list cost | $0.6328008 | $0.7022950 |
| Strict task successes | 6/25 | 7/25 |
| Cost divided by strict successes | $0.10547 | $0.10033 |
| Empty evidence bundles | 14/25 | 10/25 |
| Mean model round trip | 5.21 s | 7.12 s |
| Supplied evidence bytes | 81,411 | 79,580 |

The 50 completed calls reported **$1.3350958** total. These are Claude Code's
reported API list costs, including cache pricing, not subscription invoice
charges. Aggregate usage includes cache creation and reads exactly once;
nested per-iteration usage is not added again. Model round trips include
network/model work and are not hSUM search latency.

The same pinned Claude Code 2.1.296 executable and `claude-opus-5-5`, low effort,
answered every task. The frozen author-labeled 25-task manifest was unchanged.
Both conditions used precollected evidence capped at 12,288 bytes, no agent
tools, isolated working directories, and alternating condition order. This
compares one retrieval bundle per task, not an agent's full iterative search
workflow or the cost of an MCP session. Indexing/embedding cost is excluded.

Source corpus: commit `d5cd6b854fb25ffd4f3d2f3a489d81e1793a4385`;
materialized snapshot `8a2d7465fecb860594a434d0f8a99da68f56542d`.
The optimized hSUM binary is the `ba527f9` candidate, SHA-256
`1e770f1348321d19e5f0cf4c0a1273d8df47abac1ed44a08e55634ffff924170`.
The report binds client, runner, shared harness, schema, task, and input hashes.

## Scoring correction and retained evidence

The initial scorer incorrectly hashed Get's returned passage and compared it
with `body_sha256`, which binds the **whole document**. A partial Get can have
a different hash while being entirely correct. The corrected scorer checks
the unchanged live source's whole-document hash, returned-citation identity,
exact returned byte window, supplied-citation membership, and evidence path.
All **30/30 emitted hSUM citations pass**. Five task scores change from fail
to pass, raising hSUM from the invalid initial 2/25 to 7/25. No answer, model
call, fact pattern, or relevance label was changed. Regression tests exercise
partial Get, corrupted bytes/hashes, drift, wrong identities, and unsafe paths.

The experimental collector also read MCP `line_span` from CLI Search, whose
wire format uses `span.start_line/end_line`. Consequently the measured hSUM
headers contain `?-?` rather than line numbers. The collector is restored to
the CLI contract and has a regression test. Saved prompts were not regenerated.
This formatting defect is a limitation of this pilot, not evidence of hSUM
failing to return line numbers.

- [Original report, unchanged](results/token-cost-2026-10-21-original.json).
- [Corrected report, with correction provenance](results/token-cost-2026-10-21-corrected.json).
- [Frozen prompts and evidence](results/token-cost-2026-10-21-inputs.json).
- [Corrected aggregates and paired intervals](results/token-cost-2026-10-21-summary.json).

Raw client streams and the exact executed scripts remain in the private local
qualification directory `~/.cache/hsum-qualification/token-cost-2026-10-21-v2/`;
the report retains their hashes. An earlier startup attempt was rejected by
Claude's JSON Schema validator before inference; its stderr is retained in
the sibling directory without the `-v2` suffix. The runner now adapts only the
schema dialect declaration to draft-07, preserving every answer constraint.

## What to improve next

Both methods fail all five paraphrase and all five multi-evidence tasks under
the frozen strict scorer. hSUM gains three concept-task successes but loses
one identifier and one exact-phrase success relative to grep. Many failures
begin with an empty bundle. Regex fact checks and author-provided path labels
are imperfect proxies for answer quality: old task wording and historical
documents can conflict with current behavior. These labels were preserved,
not adjusted after seeing the answers.

A paired 10,000-resample bootstrap across tasks (seed 20261021) gives a
95% interval of **−12 to +20 percentage points** for the hSUM success-rate
difference. Per-task cost difference is $0.00278, interval **−$0.00394 to
+$0.00951**. Input-token difference is −130.8, interval **−843.6 to +550.3**.
All include zero. These intervals describe this small task sample, not
repeated-run model variance or external repositories.

Before any superiority claim, freeze a new independently reviewed evaluation
with current labels, multiple repositories, repeated paired trials, corrected
prompt formatting, and an iterative native-tool baseline. Preregister any
retrieval change before evaluating it. Keep the existing lexical-first /
hybrid-beta disposition until its separate held-out promotion gates pass.
