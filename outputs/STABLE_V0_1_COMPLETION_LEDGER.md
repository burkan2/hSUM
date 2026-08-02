# Stable v0.1 canonical completion ledger

**Snapshot:** 2026-08-02 after the B1-04 native portability disposition
**Authority:** `work/local-rust-evidence-bus-design.md`, then the reconciliation
and explicit status contracts named in `TODOS.md`.

This ledger is the denominator for completion claims. Rows are canonical
release requirements, not commits, files, tests, or implementation slices.

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
| A1-01 | Pinned Rust workspace and one contributor check | Release-qualified | yes | yes | yes | yes | yes | yes | `rust-toolchain.toml`, `src/bin/xtask.rs`, alpha release evidence |
| A1-02 | Safe initialization, root identity, trust, and project-bound selection | Release-qualified | yes | yes | yes | yes | yes | yes | `src/app/init.rs`, `src/config/`, alpha release evidence |
| A1-03 | Capability-root filesystem ingest and deterministic chunking | Release-qualified | yes | yes | yes | yes | yes | yes | `src/ingest/`, filesystem and chunking suites |
| A1-04 | Immutable SQLite evidence and atomic generations | Release-qualified | yes | yes | yes | yes | yes | yes | `src/store/`, generation and recovery suites |
| A1-05 | Exact, quoted, and active-only BM25 retrieval | Release-qualified | yes | yes | yes | yes | yes | yes | `src/search/`, query/search contract suites |
| A1-06 | Immutable citations, historical Get, and visible source drift | Release-qualified | yes | yes | yes | yes | yes | yes | citation/Get/drift suites |
| A1-07 | Read-only Doctor foundation and actionable errors | Release-qualified | yes | yes | yes | yes | yes | yes | Doctor/error suites and published alpha docs |
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
| B1-05 | Embedding schema, exact vector provenance, and index pins | Not started | no | no | no | no | no | no | Blocked in sequence on B1-04 proof |
| B1-06 | Five-state semantic/model lifecycle | Not started | no | no | no | no | no | no | Blocked in sequence on vector storage |
| B1-07 | Atomic re-embed, unchanged-content reuse, capacity, recovery | Not started | no | no | no | no | no | no | Blocked in sequence on lifecycle schema |
| B1-08 | Vector-aware prune, forget, backup, and restore guarantees | Not started | no | no | no | no | no | no | Blocked in sequence on lifecycle schema |
| B1-09 | Filtered semantic retrieval with project/source scope before KNN | Not started | no | no | no | no | no | no | Blocked in sequence on storage/lifecycle |
| B1-10 | Semantic cancellation, timeout, memory, offline, typed model states | Not started | no | no | no | no | no | no | Blocked in sequence on semantic retrieval |
| B1-11 | Weighted RRF across exact/BM25/vector candidates | Not started | no | no | no | no | no | no | Blocked in sequence on semantic retrieval |
| B1-12 | Hybrid overlap dedupe, stable ties, and bounded explanations | Not started | no | no | no | no | no | no | Blocked in sequence on hybrid retrieval |
| B1-13 | Semantic/hybrid CLI, MCP, API, cursor, and isolation parity | Not started | no | no | no | no | no | no | Blocked in sequence on hybrid retrieval |
| EVAL-02 | Stable 100-query, three-corpus, four-grade held-out set | Not started | no | no | no | n/a | no | no | Requires accepted spans and frozen fingerprints/order |
| EVAL-03 | At least 30 preregistered semantic/paraphrase queries | Not started | no | no | no | n/a | no | no | Part of the stable held-out set |
| EVAL-04 | Frozen external ripgrep, QMD, and lexical hSUM comparisons | Not started | no | no | no | n/a | no | no | Commands, versions, settings, and artifacts must be frozen |
| EVAL-05 | NDCG@10, MRR@10, exact top-three, and paired bootstrap gates | Not started | no | no | no | n/a | no | no | Deterministic seed and 10,000 resamples required |
| EVAL-06 | Evidence-based hybrid promotion or lexical-first disposition | Not started | no | no | no | yes | no | no | Cannot be decided before EVAL-02 through EVAL-05 |
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

- Stable core implementation coverage (A1/A2/B1 rows): **71.0%** (22/31).
- All in-scope implementation coverage: **59.3%** (32/54).
- Full stable-program evidence: **42.6%** (135/317 applicable evidence cells).
- Release-qualified coverage: **13.0%** (7/54).

The handoff's approximate 80% core-feature and 65% full-program figures remain
historical estimates calculated before this requirement/evidence denominator
existed; they are not mixed with ledger percentages.

## Current blocker register

No current implementation blocker requires user input. External authority may
later be required for Developer ID/notarization, stable signing/publication,
external label review, and independent dogfooding participants. Those rows
remain incomplete; they do not block the current sqlite-vec spike and
storage/lifecycle sequence.
