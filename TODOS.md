# hSUM remaining work

This backlog tracks work after the public `0.1.0-alpha.3` filesystem/lexical
release. It does not treat already implemented
init, trust, ingest, exact/BM25 search, immutable `get`, status, read-only
doctor, client configuration, Codex integration management, workspace-dynamic
MCP, or the no-`sudo` release installer as future work.

Completed launch checkboxes summarize the evidence recorded in
`outputs/IMPLEMENTATION_STATUS.md`; later backlog items are not release claims.

## P0 — Public alpha.2 launch gates (completed or remediated)

- [x] Choose and add the software license. Done 2026-07-21: dual
  `MIT OR Apache-2.0` (`LICENSE-MIT`, `LICENSE-APACHE`, Cargo.toml `license`).
- [x] Record the owner's redistribution attestation and residual-risk policy
  for every shipped brand asset. Recorded 2026-07-21 in
  `assets/brand/PROVENANCE.md` for original Photopea composites over heavily
  transformed historical references. This is not blanket clearance of every
  underlying work: they are not individually cataloged, and any asset later
  identified as restrictively sourced must be replaced.
- [x] The public repository, security-reporting policy, versioned documentation
  host, and rollback/compromise procedure exist. Before the tag, the operator
  verified `main` branch protection, required CI checks, private vulnerability
  reporting, and the release GPG variables.
- [x] Run the complete formatting, strict-Clippy, test, doctest, tampered-input,
  targeted rebuild crash/fault, and no-network suites from a clean checkout.
- [x] Prove source and release builds on clean macOS arm64 and Linux x86_64
  machines. Those clean jobs establish the alpha support matrix; other targets
  remain unsupported.
- [x] Run the exact README CLI and MCP paths against the candidate artifact,
  including first-use timing, stdout/stderr separation, generated client
  configuration, cancellation, partial-ingest exits, and recovery of an index
  created by the checksum-pinned published alpha.1 binary.
- [x] Require a byte-for-byte isolated rebuild, then produce checksummed
  archives, per-target Syft SBOMs, GitHub SBOM attestations, and a GPG-signed
  tag. Alpha archives may contain a macOS binary with only a linker-generated
  ad-hoc signature when the Gatekeeper warning is documented; alpha makes no
  Developer ID, installer, or notarization claim and remains unavailable
  through crates.io.
- [x] Generate and validate a deterministic Cargo-declared license snapshot
  against the published alpha.2 `Cargo.toml` and `Cargo.lock` hashes during the
  post-release audit. This snapshot was not attached to the alpha.2 release.
- [x] Freeze release notes and compatibility statements from the artifact actually
  tested. Do not claim benchmark, signing, installer, or support gates before
  their evidence exists.

## P0 — Before the next public release

- [ ] Exercise the prepared draft-first immutable-release path in a real
  tag-triggered run: attach the Cargo license inventory, include it and both
  SBOMs in `SHA256SUMS`, and publish only after every draft asset is present.

## P1 — Alpha hardening and operations

- Make the lower-level prepared-snapshot store mutators crate-private before
  any library or plugin ABI is exposed. `IndexDb::apply_filesystem_snapshot*`,
  the prepared-document types, and the `#[doc(hidden)]` debug helpers
  (including the synthetic snapshot-state writer used by the WAL
  torn-read fixture) are `pub` today only because integration tests link the
  crate as a library; a public ABI needs a dedicated test-support facade
  instead.

- Alpha.2 CI now exercises the checksum-pinned published alpha.1 executable.
  Keep that N-1 fixture and add an explicit diagnosis path for every future
  schema change. Unknown/newer indexes must remain read-only and unmodified.
- Rebuild process-death coverage now exercises trust removal, database
  removal, replacement creation, and replacement registration. Expand fault
  injection across the remaining durable boundaries, including disk-full,
  WAL/checkpoint, pointer rollback, and multi-process reader/writer recovery.
- The verified no-`sudo` installer is part of the alpha.4 release train.
  Before removing the explicit unsigned-alpha acknowledgement on macOS,
  provide Developer ID signing and notarization.
- Add parser fuzz targets for citations, MCP frames and cursors, queries,
  configuration, trust files, and raw connector keys.
- Generate CLI, MCP schema, error-catalog, and configuration reference pages
  from the implementation, then link-check all emitted versioned URLs.
- Add an explicit source-configuration command before exposing include,
  exclude, file-size, or sensitive-path overrides to end users. The current
  CLI intentionally persists conservative defaults.
- Add privacy-safe diagnostic export only after preview/redaction rules and a
  no-automatic-upload guarantee are tested.

## P1 — Optional watch mode

- **What:** Add a long-running local watcher that requests a full authoritative
  rescan when configured filesystem sources change.
- **Why:** Result-level drift is visible today, but freshness still requires
  explicit `hsum ingest`.
- **Constraint:** Filesystem events are hints, never the source of truth.
  Daemon lifecycle, coalescing, missed-event recovery, resource ceilings, and
  lock behavior need their own threat and recovery model.
- **Depends on:** Published generation-recovery evidence and measured demand
  after dogfooding.

## P1 — Alpha.2 data lifecycle

- Add a strict generic JSONL snapshot connector with bounded records,
  canonical source identity, authoritative deletion semantics, and fuzzed
  parsing.
- Support multiple projects and multiple sources per index without weakening
  project filtering or MCP binding isolation.
- Add explicit `backup`, `prune`, and migration plan/apply ceremonies with
  capacity estimates, immutable plan hashes, confirmation, rollback guidance,
  and released N-1 fixtures.
- Add durable `forget` and guarded `restore` only after body-free tombstones,
  copied-database behavior, old-reader fencing, backup interaction, and
  physical-rewrite crash recovery are proven.

## P2 — Local semantic retrieval

- Evaluate an explicit, pinned local model install/import workflow; alpha.1 has
  no model or network path.
- Prove filtered vector scope correctness, deterministic equal-distance
  ordering, portable SQLite/vector packaging, memory bounds, cancellation, and
  air-gapped model import before exposing semantic or hybrid modes.
- Evaluate recency, source diversity, rare-token statistics, query expansion,
  neighboring context, and reranking one experiment at a time against a frozen
  exact/BM25 baseline.
- Promote a signal only when predefined retrieval and task metrics improve
  without regressing exact-token quality, explainability, determinism, or
  latency.

## P2 — External agent-task evaluation

- Run held-out tasks from several external developers and corpora, comparing
  hSUM-assisted agents with native agent search.
- Keep recruitment, labels, source corpora, privacy protocol, and frozen task
  baselines separate from the product test suite.
- Require beta-quality artifacts and an approved privacy-safe study protocol
  before collecting user data.

## P2 — One evidence-driven live connector

- Choose at most one SaaS source from repeated user evidence rather than
  building a generic connector framework.
- Define a separate network and credential threat model covering
  authentication, authorization, rate limits, polling/webhooks, revocation,
  and deletion.
- Preserve the immutable evidence and project-scope contract across remote
  revisions.

## P2 — Backend-neutral evidence adapters

- Revisit adapters for ripgrep, QMD, lore, or other engines only if dogfooding
  shows that the evidence packet is more valuable than one native store.
- Publish a capability matrix for provenance, immutable revision lookup,
  project filtering, ranking explanations, cancellation, and deterministic
  pagination. Do not hide missing guarantees behind a lowest-common-denominator
  adapter.

## P2 — Additional release targets and package managers

- Consider macOS x86_64, Linux arm64, Windows, Homebrew, and other package
  managers only after one signed macOS arm64/Linux x86_64 release train has
  completed.
- Each new target must pass filesystem containment, storage-locality, locking,
  SQLite/WAL durability, terminal escaping, MCP framing, deterministic search,
  signing, installer, and clean-machine smoke tests.
- “Builds with Cargo” is not equivalent to a supported platform.

## P3 — Additional transports and interfaces

- HTTP MCP requires authentication, authorization, origin policy,
  rate-limiting, denial-of-service controls, and a new network threat model.
- A web UI, hosted service, editor extension, executable plugin ABI, and custom
  agent orchestration remain outside the evidence-engine wedge until usage
  justifies their operational and security surface.
