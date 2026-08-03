# Original Plan Reconciliation Design

**Status:** In implementation
**Date:** 2026-07-31
**Canonical architecture:** `work/local-rust-evidence-bus-design.md`

## Purpose

Restore a single plan hierarchy before continuing the original Alpha.2 work.
The local Rust evidence-bus design remains authoritative. Later feature plans
may extend it only when they preserve its identity, citation, scope, transport,
privacy, and release-gate contracts or explicitly replace those contracts in a
reviewed architecture decision.

## Conflict being resolved

The Alpha.4 zero-touch onboarding plan introduced two useful but conflicting
behaviors:

1. an approved workspace could be initialized during its first MCP tool call;
2. an activated repository could be ingested before its first search or get in
   each MCP process.

Those behaviors make calls advertised as read-only create or advance managed
state. The canonical design instead requires the MCP server to perform
retrieval only and reserves mutation for explicit local CLI actions.

## Decision

The original read-only MCP boundary wins.

- `evidence_search`, `evidence_get`, `evidence_project`, and
  `evidence_status` never initialize, ingest, repair, or update policy/trust
  state.
- A repository that has not been activated returns
  `REPOSITORY_NOT_ACTIVATED` with the existing exact `integration activate`
  command. After the user runs that command, the same live MCP process may
  resolve and bind the repository on retry.
- An activated repository is searched at its last explicitly committed
  generation. Source drift remains visible, but drift observation never
  changes the index.
- `integration activate`, `init`, and `ingest` remain the explicit mutation
  paths. The global Codex registration and exact-repository workspace routing
  remain available.
- Existing workspace-authorization records remain readable for compatibility,
  but MCP does not consume them for lazy enrollment. The legacy
  `authorize-workspace` surface will be removed or repurposed only through a
  separate migration decision.

The conflicting auto-enrollment and once-per-task refresh clauses in
`docs/superpowers/plans/2026-07-27-zero-touch-agent-onboarding.md` are therefore
superseded by this decision. Its installation, global registration,
same-process activation retry, isolation, and repository-file-safety clauses
remain valid.

## Observable contract

For every MCP tool call:

1. configuration, trust, policy, and index paths have identical contents before
   and after the call, except for external concurrent activity;
2. the SQLite connection used for retrieval is read-only and query-only;
3. an approved but inactive repository stays inactive;
4. filesystem edits are reported through `source_state` but are not ingested;
5. `freshness.policy` is `manual` and no automatic refresh is claimed.

## Implementation order

1. Lock the behavior with integration tests for approved-but-inactive and
   activated-but-edited repositories.
2. Remove automatic enrollment and refresh from workspace MCP resolution and
   evidence execution.
3. Update README, agent policy, changelog, TODOs, and stale implementation
   evidence.
4. Extract shared Search/Get/Status use-case handlers before adding original
   Alpha.2 connectors and lifecycle operations.
5. Establish the frozen 25-query evaluation schema before ranking changes.

Steps 1–5 are complete in the current checkout: CLI and MCP Search/Get/Status
now use shared read-only application handlers. Process and MCP fixtures prove
shared ranking/evidence fields, cursor snapshot safety, transport bounds,
cancellation, historical citation verification after the indexed head
advances, and Status identity/count/problem parity. Shared protocol DTO
construction has started with the exact Get packet, whose CLI and MCP JSON now
comes from `src/protocol/` and is compared as one complete normalized object.
Search response metadata, passage identity, scores, duplicate citations, and
source-state mapping also come from `src/protocol/`; compatibility projections
preserve the two existing envelopes and span/rank-key layouts, and the process
fixture compares the complete authoritative Search core. Status DTO
construction is also complete: one protocol mapping owns the health/repair
conversion and projects the existing CLI diagnostic and MCP project-health
envelopes, with the complete authoritative core compared by the process
fixture. Protocol convergence is now complete: CLI and MCP also share the
frozen opaque cursor codec, and a process fixture proves one stable ordering
across CLI → MCP → CLI together with query- and snapshot-staleness failures.
The checked-in `hsum-public-gold-25-v1` manifest now freezes 25 balanced tasks,
graded document judgments, answer oracles, a JSON Schema, and a SHA-256 pin.
Its isolated three-build development baseline is intentionally unfavorable
where the product is weak: every retriever misses all five paraphrase and five
multi-evidence tasks. hSUM has the best mean graded quality, but its nDCG@5
varies materially across fresh index identities and it remains much slower
than grep. No labels, ranking weights, or product code were changed in
response. The variance is recorded as a hardening item; the next
implementation-order item remains the original Alpha.2 data lifecycle,
beginning with the strict generic JSONL snapshot connector, not another
Search/Get/Status boundary change or a retrieval quick fix.

## Non-goals

- No citation, chunking, ranking, or database-schema change.
- No removal of already-created indexes or workspace policy files.
- No attempt to hide source drift; immutable stored evidence and live-source
  observation keep their existing meanings.
- No semantic retrieval or original Alpha.2 lifecycle work in this slice.
