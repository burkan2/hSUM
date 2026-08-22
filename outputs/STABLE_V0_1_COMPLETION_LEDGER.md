# Stable v0.1 canonical completion ledger

**Snapshot:** 2026-08-22 after the B1-05 through B1-13 native product gate,
generation crash and multi-process cancellation/replacement qualification,
same-index lexical determinism qualification, source-package qualification,
100k/one-million scale harness groundwork, bounded parser fuzz smoke,
client/clean-machine qualification groundwork, and the generated local
reference/README contract gates
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
| B1-05 | Embedding schema, exact vector provenance, and index pins | Native target evidence passing | yes | yes | yes | yes | yes | no | Schema-v4 and tamper suites on both CI targets; real verified-artifact pin and indexed state in run `32542799076` |
| B1-06 | Five-state semantic/model lifecycle | Native target evidence passing | yes | yes | yes | yes | yes | no | State suites on both targets plus real installed/indexed product state in run `32542799076` |
| B1-07 | Atomic re-embed, unchanged-content reuse, capacity, recovery | Native target evidence passing | yes | yes | yes | yes | yes | no | Lifecycle suites on both targets plus real offline product re-embed in run `32542799076` |
| B1-08 | Vector-aware prune, forget, backup, and restore guarantees | Native target evidence passing | yes | yes | yes | yes | yes | no | Exact preservation/reclamation/deletion/restoration suites pass on both targets in run `32542799062` |
| B1-09 | Filtered semantic retrieval with project/source scope before KNN | Native target evidence passing | yes | yes | yes | yes | yes | no | Scoped/tie suites pass on both targets; real CLI and MCP vector retrieval passes in run `32542799076` |
| B1-10 | Semantic cancellation, timeout, memory, offline, typed model states | Native target evidence passing | yes | yes | yes | yes | yes | no | Worker/state suites on both targets, bounded native probe metrics, and real offline CLI/MCP inference in run `32542799076` |
| B1-11 | Weighted RRF across exact/BM25/vector candidates | Native target evidence passing | yes | yes | yes | yes | yes | no | Frozen fusion suites on both targets plus real CLI/MCP hybrid behavior and exact fan-in comparison in run `32542799076` |
| B1-12 | Hybrid overlap dedupe, stable ties, and bounded explanations | Native target evidence passing | yes | yes | yes | yes | yes | no | Dedupe/tie/explanation suites on both targets plus explained real hybrid product calls in run `32542799076` |
| B1-13 | Semantic/hybrid CLI, MCP, API, cursor, and isolation parity | Native target evidence passing | yes | yes | yes | yes | yes | no | CLI/MCP/process suites on both targets plus real model CLI/MCP behavior match in run `32542799076` |
| EVAL-02 | Stable 100-query, three-corpus, four-grade held-out set | Complete local gate passing | yes | yes | yes | n/a | yes | no | `eval/` freezes 100 tasks, three corpus blob sets, accepted byte spans, labels, query order, migrations, model, and retrieval settings |
| EVAL-03 | At least 30 preregistered semantic/paraphrase queries | Complete local gate passing | yes | yes | yes | n/a | yes | no | 35 semantic/paraphrase tasks validate before a run begins |
| EVAL-04 | Frozen external ripgrep, QMD, and lexical hSUM comparisons | Complete local gate passing | yes | yes | yes | n/a | yes | no | Report-only ripgrep 15.1.0 and QMD 2.5.3 setup/commands/models are frozen and recorded in the raw result |
| EVAL-05 | NDCG@10, MRR@10, exact top-three, and paired bootstrap gates | Complete local gate passing | yes | yes | yes | n/a | yes | no | Four-grade metrics and deterministic 10,000-resample paired bootstrap are unit-tested and persisted in the raw result |
| EVAL-06 | Evidence-based hybrid promotion or lexical-first disposition | Documentation complete | yes | yes | yes | n/a | yes | no | `eval/results/heldout-v1-2026-08-02-macos-arm64.{json,md}` requires `stable-lexical-hybrid-beta`: semantic gain/NDCG pass; MRR lower bound and exact-token top-three gates fail |
| Q-01 | Generation-boundary fault injection and prior-or-new recovery | Native target evidence passing | yes | yes | yes | yes | yes | no | Six process-death checkpoints prove exact prior-or-next recovery with a fresh Doctor/search process locally and in run `32543715602`; stable-candidate rerun remains open |
| Q-02 | Multi-process reader/writer and cancellation-versus-timeout qualification | Native target evidence passing | yes | yes | yes | yes | yes | no | Exact commit `e415dba` passes the complete local gate and both CI targets in run `32564056058`; real installed-model workers prove queued timeout, explicit cancellation without late responses, and recovery on both targets in run `32564056061`, while a separate-process vector reader proves bounded replacement refusal, retry, and stale-inode citation rejection |
| Q-03 | Same-target and cross-target deterministic ordering | Native target evidence passing | yes | yes | yes | yes | yes | no | Stable lexical qualification run `32565388856` reuses index `0891488d…` for five fresh CLI and five fresh MCP processes on each supported target; every ordered citation, duplicate set, degradation flag, and explanation matches, with `2.22e-16` maximum backend-score delta; independent-build UUID variance remains diagnosed, and hybrid remains beta under EVAL-06 |
| Q-04 | 100,000-chunk performance and component latency evidence | Focused tests passing | yes | yes | no | no | yes | no | `benches/retrieval_scale/` freezes the exact 100k corpus, 25-query/750-observation protocol, cold/warm separation, stage timings including body materialization, RSS/storage/throughput evidence, SLOs, and CV gate; Apple M2 and Linux native executions remain open |
| Q-05 | Report-only 1,000,000-chunk stress and cancel storm | Focused tests passing | yes | yes | no | no | yes | no | Frozen corpus/probe manifest, deadline-reporting harness, cancel storm, exact-SHA workflow, and 13 harness regressions pass; native run `32569278903` remains in progress |
| Q-06 | Security, privacy, supply-chain, and recovery reviews | Focused tests passing | yes | yes | no | no | partial | no | Locked dependency policy passes; six bounded parser fuzz targets pass Linux smoke run `32569324813`; sustained campaigns and independent semantic/distribution/recovery review remain open |
| Q-07 | Pinned Codex, Claude Code, and generic cross-client dogfooding | Focused tests passing | yes | yes | no | no | partial | no | Checksum-pinned protocol and generic search/get round trip pass; exact Codex `0.149.0-alpha.4.1` is usage-limited until 2026-08-29 and Claude Code `2.1.212` requires OAuth refresh before live tool-event acceptance |
| Q-08 | Clean-machine CLI and MCP DX trials on both targets | Native target evidence passing | yes | yes | yes | yes | yes | no | Run `32571063965` builds checksummed candidates separately, then passes five fresh CLI/full-MCP trials per supported target and cross-target fan-in; enhanced timing schema passes locally and awaits current-head native rerun |
| Q-09 | Generated CLI, MCP, config, and error references plus link checks | Native target evidence passing | yes | yes | yes | yes | partial | no | Eight implementation-derived pages, four generator tests, byte-drift/local-link/runtime-URL mapping gate, and canonical deployed URL base pass on both targets in run `32549550396`; remote audit passes 65/77 alpha.4 pages and reports 12 undeployed subcodes |
| Q-10 | Executable stable quickstart and compatibility-policy verification | Focused tests passing | yes | yes | no | no | yes | no | README version/command/privacy and 27-row controlled capability policy are executable gates; five-trial clocks pass locally, but the stable candidate artifact rerun remains open |
| DIST-01 | crates.io source distribution and locked install smoke | Native target evidence passing | yes | yes | yes | yes | yes | no | Explicit Cargo allowlist plus `scripts/package-smoke.sh` prove the locked archive boundary and extracted install on both targets in run `32546275484`; no crate has been published |
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
- All in-scope implementation coverage: **88.9%** (48/54).
- Full stable-program evidence: **66.8%** (211/316 applicable evidence cells).
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
