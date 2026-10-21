# Candidate verification on 2026-10-21

The local cross-client protocol passes. The fresh lexical benchmark completes,
with better mean ranking quality than grep on this small internal set, slower
lookups, and remaining cross-index ranking variation. These are macOS arm64
development results, not stable release qualification or a general superiority
claim.

Candidate source: `ba527f96d583724a0cb9c1635f1c93ff0d33b6cf`.
Optimized binary SHA-256:
`1e770f1348321d19e5f0cf4c0a1273d8df47abac1ed44a08e55634ffff924170`.
The [receipt](results/verification-2026-10-21-receipt.json) binds the reports,
raw client stream hashes, and verified Get packets to this candidate.

## Real MCP clients

The [unaltered client report](results/client-dogfood-v2-2026-10-21-macos-arm64.json)
passes protocol `cross-client-citation-round-trip-v2`, manifest SHA-256
`5605ee6af83018606801d3de1d0bbcb431aa4e5a036c0e4790a6ead09f5fc727`.

| Client | Version | Result | Agent round trip |
|---|---|---|---:|
| Generic stdio MCP | Protocol 2025-11-25 | Pass | n/a |
| Codex | 0.149.0-alpha.4.1 | Pass | 20.19 s |
| Claude Code | 2.1.296 | Pass | 13.54 s |

All three searched and fetched the same citation. Both agent event streams
contain the actual tool calls and results, followed by the required structured
answer. Additional inspection verifies the returned body hash, fixture token,
unchanged live source, and agreement between the fetched and final citations.
Agent times include model/network work and are not hSUM search latency.
Claude reports $0.0839228 usage for the frozen run, below its $0.25 cap.

Protocol v1's Claude 2.1.212 failure remains historical evidence. A separate
2.1.296 diagnostic showed an initial negotiation probe closing the rmcp stdio
server, followed by successful client retry using protocol 2025-11-25. This
establishes that the installed version works; it does not isolate the old
client's root cause. V2 changes only the Claude version pin and protocol ID.
Persistent client configuration was not changed. Raw JSONL files remain in
the local qualification evidence directory; their hashes are in the receipt.

## Fresh lexical development control

The [unaltered benchmark](results/lexical-control-2026-10-21-macos-arm64.json)
uses all 25 author-labeled tasks, five query repetitions per retriever, and
three independently initialized indexes. Each index contains the same clean
corpus snapshot `b25276532c9f6608960dac0fb5d08fa0b4b36fc6`: 134 documents and
3,096 passages. The source checkout is recorded as dirty. The preserved
uncommitted harness edit affects only the separate drift demo; this run uses
the unchanged stability path. Python is 3.14.2 on macOS 26.4.1 arm64.

Values are means of the three per-build aggregates; parentheses show the
observed build range.

| Retriever | NDCG@5 | Hit@1 | Recall@5 | MRR | Median lookup | Output bytes |
|---|---:|---:|---:|---:|---:|---:|
| ripgrep, sorted, one thread, 20 context lines | 20.4% | 28.0% | 21.3% | 0.320 | 6.7 ms | 142,000 |
| git grep, 20 context lines | 20.7% | 32.0% | 23.3% | 0.340 | 18.0 ms | 142,000 |
| hSUM lexical | 25.3% (24.4–26.4%) | 45.3% (40–52%) | 26.7% | 0.490 | 45.7 ms (40.9–54.6 ms) | 138,219 (134,819–140,663) |

hSUM's five multi-document tasks still have zero Hit@1. Paraphrase Hit@1 is
20%, concept Hit@1 is 60%, and exact-phrase Hit@1 ranges across builds.
Fresh index identities still change tied rankings, as explained in
[the variance diagnosis](LEXICAL_VARIANCE_DIAGNOSIS.md). The corpus differs
from the historical August baseline, so these numbers do not establish a
before/after improvement. This development control cannot promote hybrid;
the frozen 100-query held-out disposition remains lexical-first/hybrid-beta.

## Remaining qualification

- Linux 100k [run 39638597258](https://github.com/burkan2/hSUM/actions/runs/39638597258)
  is still preparing embeddings at this checkpoint; no scale pass is claimed.
- The Apple M2 100k and million-passage stress protocols still lack complete
  passing reports. The retained partial/failed runs remain diagnostic evidence.
- The complete local contributor check and seven ordinary CI workflows passed
  for the candidate source before this verification-only update.
- Stable signing, notarization, independent review, and final release gates
  remain open. No stable tag or publication was performed.
