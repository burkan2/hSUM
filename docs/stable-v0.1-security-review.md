# Stable v0.1 security, privacy, supply-chain, and recovery review

**Review date:** 2026-08-22

**Candidate line:** `codex/canonical-alpha2-beta1-groundwork`

**Scope:** current stable-v0.1 source and release machinery; no HTTP, browser,
authentication, multi-user server, plugin, or live-connector surface exists.

This is a maintainer release review, not an independent penetration test or a
legal opinion. It closes no signing, notarization, publication, external-client,
or clean-machine gate by itself.

## Disposition

No exploitable critical or high-severity defect was identified in the reviewed
local product boundaries. The dependency graph has one accepted maintenance
risk: RustSec `RUSTSEC-2024-0436` marks `paste 1.0.15` unmaintained. It is a
compile-time proc macro brought in by `tokenizers 0.22.2` through the pinned,
natively qualified `fastembed 5.17.4` stack. RustSec reports no vulnerability
and no safe direct upgrade. `deny.toml` records the exact exception and leaves
all future vulnerabilities, yanked packages, source drift, wildcard
dependencies, and unapproved licenses fail-closed.

The stable release remains blocked on the separate distribution gates: a
stable-capable draft workflow, detached signing authority, macOS Developer ID
signing/notarization, a stable-tag rerun of the now-passing clean-machine
artifact protocol, and final publication and rollback drills.

## Reviewed boundaries

| Boundary | Evidence and disposition |
|---|---|
| Filesystem containment | Unix traversal opens every component relative to an already-open descriptor with `NOFOLLOW`, rejects changed identities, skips symlinks and special files, bounds bytes/depth/files, and rechecks the opened file before and after reading. Filesystem ingest tests cover intermediate/final/root/ancestor symlinks and replacement races. |
| Secret-path and untrusted-content policy | Common secret-bearing paths remain default-denied; sensitive admission requires both an explicit include and the internal allowance. Returned evidence is always marked untrusted. Hardlinks and external filesystem snapshots remain documented limits rather than claimed erasure guarantees. |
| Project and citation isolation | MCP is bound to one selected project, opens retrieval state read-only/query-only, rejects project overrides, and returns non-disclosing failures for citations outside the binding. Shared CLI/MCP protocol fixtures cover Search/Get/Status parity and immutable citation resolution. |
| Untrusted parsers | CLI/query, JSONL, MCP framing, citations, cursors, configuration, and trust inputs have explicit length/cardinality/depth/unknown-field limits and focused/property coverage. Six dedicated libFuzzer targets cover citation, query, cursor, JSONL, MCP-frame, and chunk boundaries; bounded CI smoke catches regressions, while long-running and independent campaigns remain open. |
| SQL and command construction | User values use SQLite parameters and the owned query compiler. Dynamic SQL identifiers are closed internal vector-slot/table names. External client commands use argument arrays rather than a shell. |
| Model supply chain | Model installation is the only runtime network path. It is HTTPS-only with TLS 1.2 minimum, bounded redirects/timeouts/retries/bytes, a pinned upstream revision, exact per-file lengths and SHA-256 values, private staging, and offline refusal through `HSUM_OFFLINE=1`. A custom CA must be a bounded regular PEM file. |
| Local confidentiality and logs | Managed state is created with user-only permissions where supported; Doctor reports permission failures. Normal product paths do not emit indexed bodies or queries to diagnostic logs. Client configuration warns about the downstream cloud-agent boundary before emitting copyable configuration. |
| Recovery and deletion | Atomic generation activation, crash checkpoints, separate-process reader/replacement fencing, body-free forget-ledger replay, managed-backup inventory, guarded restore, prune floors, and whole-index quarantine are covered by focused/native tests. SSD wear levelling, unmanaged copies, and external snapshots are explicitly outside the physical-erasure claim. |
| Release supply chain | The lockfile, exact dependency versions, source-package allowlist, reproducible native build, checksums, SBOMs, attestations, license inventory, draft-first asset allowlist, signed-tag guard, and rollback smokes exist. Stable signing/notarization and a real stable tag remain open gates. |

## Repeatable audit evidence

- `cargo deny check advisories` uses the current RustSec database. The single
  accepted unmaintained advisory remains visible as a note.
- `cargo deny check bans licenses sources` enforces both supported target
  graphs, the explicit permissive-license engineering allowlist, crates.io-only
  dependency sources, and no wildcard requirements.
- `.github/workflows/dependency-policy.yml` runs both checks on dependency
  changes, weekly, manually, and on `main`. Its third-party action is pinned to
  the exact commit currently resolving the upstream `v2` release line.
- The tracked source scan found no common private-key, GitHub token, OpenAI key,
  Slack token, or AWS access-key signature outside ignored build/editor state.
- `cargo +1.91.0 xtask check`, native qualification workflows, release/no-network
  smokes, and the final release review remain the authoritative executable
  evidence; this document does not substitute for their results.
- `cargo +nightly-2026-08-15 fuzz build` compiles all six parser targets. The
  scheduled `Parser fuzz smoke` workflow runs each seeded corpus for 15 seconds
  with a 64 KiB input bound, five-second per-input timeout, and 2 GiB RSS limit.
  Those short runs are regression evidence, not an exhaustive fuzz claim.

## Residual risk register

1. **Accepted maintenance risk — transitive `paste 1.0.15`.** Monitor
   `fastembed`/`tokenizers`; replace only through a new pinned inference-stack
   qualification, not an unreviewed lockfile patch.
2. **Open parser assurance — sustained and independent fuzz campaigns.** The
   citation/cursor/query/JSONL/MCP/chunk corpus and repeatable bounded CI smoke
   now exist. Multi-hour sanitizer campaigns, corpus minimization, crash triage,
   and independent review are still required before final promotion.
3. **Open release authority — signatures and Apple notarization.** Alpha
   compromise procedures exist, but stable publication cannot proceed without
   separate signing custody and the full Apple credential path.
4. **Open external validation.** Exact-head run `32658212667` passes five
   offline clean-machine CLI/MCP trials per target with independently verified
   source/checkout/base/tree provenance. Pinned live-client dogfood, a
   stable-tag artifact rerun, and the final independent security/recovery
   review remain required before stable promotion.
