# Stable v0.1 canonical completion ledger

**Snapshot:** 2026-08-03 after the B1-05 through B1-13 complete local gate
and the frozen held-out promotion decision
**Authority:** `work/local-rust-evidence-bus-design.md`, then the reconciliation
and explicit status contracts named in `TODOS.md`.

This ledger is the denominator for completion claims. Rows are canonical
release requirements, not commits, files, tests, or implementation slices.
A prerelease artifact is valuable evidence, but never substitutes for stable
release qualification of the current candidate.

Status vocabulary:

- `Not started`
- `In implementation`
- `Implemented`
- `Focused tests passing`
- `Complete local gate passing`
- `Native target evidence passing`
- `Documentation complete`
- `Release-qualified`
- `Excluded` only when the canonical plan names a stable-v0.1 non-goal

Evidence columns use `yes`, `no`, or `n/a`. `n/a` is allowed only when a gate
does not apply to that requirement. A row's status is the strongest statement
supported without hiding a missing later gate.

## Canonical in-scope rows

| ID | Canonical requirement | Status | Implementation | Focused tests | Complete local gate | Native targets | Documentation | Release gate | Primary evidence or open proof |
|---|---|---|---:|---:|---:|---:|---:|---:|---|
| A1-01 | Pinned Rust workspace and one contributor check | Native target evidence passing | yes | yes | yes | yes | yes | no | `rust-toolchain.toml`, `src/bin/xtask.rs`, and alpha release evidence; stable candidate rerun remains required |
| A1-02 | Safe initialization, root identity, trust, and project-bound selection | Native target evidence passing | yes | yes | yes | yes | yes | no | `src/app/init.rs`, `src/config/`, and alpha release evidence; stable candidate rerun remains required |
| A1-03 | Capability-root filesystem ingest and deterministic chunking | Native target evidence passing | yes | yes | yes | yes | yes | no | `src/ingest/`, filesystem/chunking suites, and alpha release evidence; stable candidate rerun remains required |
| A1-04 | Immutable SQLite evidence and atomic generations | Native target evidence passing | yes | yes | yes | yes | yes | no | `src/store/`, generation/recovery suites, and alpha release evidence; stable candidate rerun remains required |
| A1-05 | Exact, quoted, and active-only BM25 retrieval | Native target evidence passing | yes | yes | yes | yes | yes | no | `src/search/`, query/search suites, and alpha release evidence; stable candidate rerun remains required |
| A1-06 | Immutable citations, historical Get, and visible source drift | Native target evidence passing | yes | yes | yes | yes | yes | no | citation/Get/drift suites and alpha release evidence; stable candidate rerun remains required |
| A1-07 | Read-only Doctor foundation and actionable errors | Native target evidence passing | yes | yes | yes | yes | yes | no | Doctor/error suites and published alpha docs; stable candidate rerun remains required |
| A1-08 | Project-bound read-only MCP stdio | Native target evidence passing | yes | yes | yes | yes | partial | no | Current read-only reconciliation is unreleased; PR #9 CI is green on both targets |
| A1-09 | CLI/MCP/API Search, Get, Status parity and opaque cursors | Native target evidence passing | yes | yes | yes | yes | partial | no | protocol DTOs, cross-transport fixtures, and both PR #9 targets |
| EVAL-01 | Frozen 25-query lexical control | Complete local gate passing | yes | yes | yes | n/a | yes | no | `benches/agent_ab/`; cross-build variance remains diagnosed but unresolved |
| A2-01 | Strict JSONL snapshot connector and authoritative lifecycle | Native target evidence passing | yes | yes | yes | yes | partial | no | JSONL unit/process suites on both PR #9 targets |
| A2-02 | Named projects and project-local source management | Native target evidence passing | yes | yes | yes | yes | partial | no | project and source process suites on both targets |
| A2-03 | Explicit filesystem-source registration | Native target evidence passing | yes | yes | yes | yes | partial | no | `tests/filesystem_source_cli.rs` on both targets |
| A2-04 | Doctor integrity, bounded repair, and body-free reports | Native target evidence passing | yes | yes | yes | yes | partial | no | Doctor and process suites on both targets |
| A2-05 | Verified backup and managed-backup inventory | Native target evidence passing | yes | yes | yes | yes | partial | no | maintenance and managed-backup suites on both targets |
| A2-06 | Explicit index/config migration plan and apply | Native target evidence passing | yes | yes | yes | yes | partial | no | N-1 and process migration suites on both targets |
| A2-07 | Prune, citation invalidation, and history floor | Native target evidence passing | yes | yes | yes | yes | partial | no | maintenance suites on both targets |
| A2-08 | Durable forget, reader fencing, and guarded restore | Native target evidence passing | yes | yes | yes | yes | partial | no | maintenance and multi-process suites on both targets |
| A2-09 | Confirmed whole-index deletion | Native target evidence passing | yes | yes | yes | yes | partial | no | `tests/index_delete_cli.rs` on both targets |
| B1-01 | Pinned model manifest and explicit install/import lifecycle | Native target evidence passing | yes | yes | yes | yes | yes | no | model suites and PR #9 native CI |
| B1-02 | Verified-byte FastEmbed CPU inference portability | Native target evidence passing | yes | yes | yes | yes | yes | no | v2 native reports from run `30731369195` |
| B1-03 | Cross-architecture numerical compatibility and vector provenance | Native target evidence passing | yes | yes | yes | yes | yes | no | 3,456-component comparison, identical ordering, and full CI fan-in from runs `30731369195` and `30731864477` |
| B1-04 | sqlite-vec static packaging and filtered-KNN portability spike | Native target evidence passing | yes | yes | yes | yes | yes | no | Raw failures retained; bounded storage revision passes both targets and fan-in in run `30732989326` |
| B1-05 | Embedding schema, exact vector provenance, and index pins | Complete local gate passing | yes | yes | yes | no | no | no | Schema v4, immutable profile metadata, canonical provenance/cache, sqlite-vec A/B slots, Doctor tamper coverage |
| B1-06 | Five-state semantic/model lifecycle | Complete local gate passing | yes | yes | yes | no | no | no | Exact pin-at-init and derived lexical/configured/installed/indexed/degraded states in model and process suites |
| B1-07 | Atomic re-embed, unchanged-content reuse, capacity, recovery | Complete local gate passing | yes | yes | yes | no | no | no | Pre-inference cache reuse, bounded batches, capacity planning, atomic shadow-slot flip, rollback and invalidation fixtures |
| B1-08 | Vector-aware prune, forget, backup, and restore guarantees | Complete local gate passing | yes | yes | yes | no | no | no | Exact vector preservation/reclamation/deletion/restoration in vector and maintenance suites |
| B1-09 | Filtered semantic retrieval with project/source scope before KNN | Complete local gate passing | yes | yes | yes | no | no | no | Transaction-scoped active-slot fan-out applies each project source UUID as a sqlite-vec partition predicate before KNN; K+1/exact tie fallback, guarded materialization, compatibility refusal, focused semantic fixtures, and the complete serialized local gate pass |
| B1-10 | Semantic cancellation, timeout, memory, offline, typed model states | Complete local gate passing | yes | yes | yes | no | no | no | Two private model processes, eight queued leader jobs, identical-query coalescing, bounded request/response frames, caller cancellation/deadline discard, overdue child kill/replacement, offline verified-cache-only inference, exact typed failure states, cancel-storm lexical-survival proof, real-process missing-model protocol proof, and the complete serialized local gate pass |
| B1-11 | Weighted RRF across exact/BM25/vector candidates | Complete local gate passing | yes | yes | yes | no | partial | no | Frozen integer-weight, equal-rank, and three-list fixtures plus the complete contributor gate pass at `011d669` |
| B1-12 | Hybrid overlap dedupe, stable ties, and bounded explanations | Complete local gate passing | yes | yes | yes | no | partial | no | Same-content/overlap, exact citation preservation, and bounded explanation fixtures plus the complete contributor gate pass at `011d669` |
| B1-13 | Semantic/hybrid CLI, MCP, API, cursor, and isolation parity | Complete local gate passing | yes | yes | yes | no | partial | no | Shared public modes, typed degradation, cursor execution fingerprint, and CLI/MCP/process parity fixtures plus the complete contributor gate pass at `011d669` |
| EVAL-02 | Stable 100-query, three-corpus, four-grade held-out set | Complete local gate passing | yes | yes | yes | n/a | yes | no | `eval/` freezes 100 tasks, three corpus blob sets, accepted byte spans, labels, query order, migrations, model, and retrieval settings |
| EVAL-03 | At least 30 preregistered semantic/paraphrase queries | Complete local gate passing | yes | yes | yes | n/a | yes | no | 35 semantic/paraphrase tasks validate before a run begins |
| EVAL-04 | Frozen external ripgrep, QMD, and lexical hSUM comparisons | Complete local gate passing | yes | yes | yes | n/a | yes | no | Report-only ripgrep 15.1.0 and QMD 2.5.3 setup/commands/models are frozen and recorded in the raw result |
| EVAL-05 | NDCG@10, MRR@10, exact top-three, and paired bootstrap gates | Complete local gate passing | yes | yes | yes | n/a | yes | no | Four-grade metrics and deterministic 10,000-resample paired bootstrap are unit-tested and persisted in the raw result |
| EVAL-06 | Evidence-based hybrid promotion or lexical-first disposition | Documentation complete | yes | yes | yes | n/a | yes | no | `eval/results/heldout-v1-2026-08-02-macos-arm64.{json,md}` requires `stable-lexical-hybrid-beta`: semantic gain/NDCG pass; MRR lower bound and exact-token top-three gates fail |
| Q-01 | Generation-boundary fault injection and prior-or-new recovery | Implemented | yes | yes | no | no | partial | no | Existing coverage is substantial; remaining durable boundaries are open |
| Q-02 | Multi-process reader/writer and cancellation-versus-timeout qualification | Implemented | yes | yes | no | no | partial | no | Semantic worker and vector replacement cases remain open |
| Q-03 | Same-target and cross-target deterministic ordering | Implemented | yes | yes | no | no | partial | no | Lexical cross-build/cross-target and hybrid proofs remain open |
| Q-04 | 100,000-chunk performance and component latency evidence | Not started | no | no | no | no | no | no | Stable protocol requires 3 runs, warmups, RSS, storage, cold start |
| Q-05 | Report-only 1,000,000-chunk stress and cancel storm | Not started | no | no | no | no | no | no | Required report-only release evidence |
| Q-06 | Security, privacy, supply-chain, and recovery reviews | Implemented | yes | yes | no | no | partial | no | Alpha evidence exists; stable semantic/distribution review remains |
| Q-07 | Pinned Codex, Claude Code, and generic cross-client dogfooding | Not started | no | no | no | no | no | no | Exact client versions and accepted evidence required |
| Q-08 | Clean-machine CLI and MCP DX trials on both targets | Implemented | yes | yes | no | no | partial | no | Alpha paths exist; five-trial stable protocol remains open |
| Q-09 | Generated CLI, MCP, config, and error references plus link checks | Implemented | yes | yes | no | no | partial | no | Stable generated reference tree and link gate remain open |
| Q-10 | Executable stable quickstart and compatibility-policy verification | Implemented | yes | yes | no | no | partial | no | Must run against the stable candidate artifact |
| DIST-01 | crates.io source distribution and locked install smoke | Not started | no | no | no | no | partial | no | `publish = false` today |
| DIST-02 | Supported prebuilt archives, checksums, and installer verification | Implemented | yes | yes | yes | yes | yes | no | Alpha release path exists; stable candidate still required |
| DIST-03 | Detached signatures, SBOM, attestations, license inventory | Implemented | yes | yes | yes | yes | yes | no | Alpha path exists; stable candidate still required |
| DIST-04 | macOS Developer ID signing and notarization | Not started | no | no | no | no | partial | no | Requires stable signing/notarization authority |
| DIST-05 | Stable release smoke, reproducibility, rollback, and publication | Not started | no | no | no | no | no | no | Final stable candidate gate |
| FINAL-01 | TODO/status reconciliation and intended-vs-implemented audit | Not started | no | no | no | n/a | no | no | Runs after all preceding requirements have final dispositions |
| FINAL-02 | Complete code, security, documentation, and release review | Not started | no | no | no | n/a | no | no | Final independent review gate |

## Explicit canonical exclusions

| ID | Requirement | Status | Authority |
|---|---|---|---|
| X-01 | Optional watch mode | Excluded | Canonical optional/post-v0.1 classification |
| X-02 | HTTP transport | Excluded | Stable v0.1 is MCP stdio only |
| X-03 | Web UI or hosted service | Excluded | Canonical non-goal |
| X-04 | Generic live-connector framework | Excluded | Canonical non-goal |
| X-05 | Plugin ABI or executable hooks | Excluded | Canonical non-goal |
| X-06 | Post-v0.1 transports and interfaces | Excluded | Canonical non-goal |
| X-07 | Additional platforms outside macOS arm64/Linux x86_64 | Excluded | Stable support matrix |
| X-08 | Reranking, recency, query expansion, source diversity, implicit context | Excluded | Evaluation-gated post-v0.1 experiments |

## Percentage rules

Percentages are calculated only from the 54 in-scope rows above; exclusions
never increase completion.

- **Implementation coverage:** rows with `Implementation = yes` divided by 54.
- **Focused-test coverage:** rows with `Focused tests = yes` divided by the
  rows where focused tests apply.
- **Full stable-program evidence:** all earned `yes` cells divided by all
  applicable evidence cells across Implementation, Focused tests, Complete
  local gate, Native targets, Documentation, and Release gate. `n/a` cells are
  removed from the denominator.
- **Release-qualified coverage:** rows with `Release gate = yes` divided by 54.

Mechanically checked snapshot at this revision:

- Stable core implementation coverage (A1/A2/B1 rows): **100.0%** (31/31).
- All in-scope implementation coverage: **85.2%** (46/54).
- Full stable-program evidence: **55.1%** (174/316 applicable evidence cells).
- Release-qualified coverage: **0.0%** (0/54); the published alpha is not a
  stable candidate qualification.

The handoff's approximate 80% core-feature and 65% full-program figures remain
historical estimates calculated before this requirement/evidence denominator
existed; they are not mixed with ledger percentages.

## Current blocker register

No current implementation blocker requires user input. External authority may
later be required for Developer ID/notarization, stable signing/publication,
external label review, and independent dogfooding participants. Those rows
remain incomplete; they do not block the current recovery, determinism,
performance, documentation, and compatibility qualification sequence.
