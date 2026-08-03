# hSUM remaining work

This backlog tracks work after the public `0.1.0-alpha.4` filesystem/lexical
release. It does not treat already implemented
init, trust, ingest, exact/BM25 search, immutable `get`, status, read-only
doctor, client configuration, Codex integration management, workspace-dynamic
MCP, or the no-`sudo` release installer as future work.

Completed launch checkboxes summarize the evidence recorded in
`outputs/IMPLEMENTATION_STATUS.md`; later backlog items are not release claims.

## Plan authority and execution order

The canonical roadmap is `work/local-rust-evidence-bus-design.md`. The
read-only MCP reconciliation is recorded in
`docs/superpowers/specs/2026-07-31-original-plan-reconciliation-design.md`.
Continue in this order:

1. extract shared Search/Get/Status application handlers and close complete
   CLI/MCP evidence-packet parity;
2. establish the frozen 25-query evaluation schema and baseline;
3. preserve the completed original Alpha.2 data-lifecycle guarantees below;
4. evaluate semantic retrieval only after the lexical baseline is frozen.

Optional watch mode and additional integrations are not on the critical path.

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

- [x] Finish shared Search/Get/Status use-case handlers over narrow store and
  drift-observation APIs. Completed 2026-07-31: CLI and MCP now execute all
  three read-only evidence paths through `src/app/`, including one-snapshot
  Status counts/health, post-transaction storage diagnostics, bounded source
  probing, transport-specific field/page limits, and shared CLI/MCP
  cursor-snapshot validation.
- [x] Move CLI and MCP onto shared protocol DTOs, complete status packet
  parity, and implement the advertised CLI cursor contract. Completed
  2026-08-01: exact Get, both Search and Status compatibility envelopes, and
  the opaque snapshot/query-bound cursor codec now live in `src/protocol/`.
  The shared process fixture compares the complete Get object and the complete
  normalized authoritative Search and Status cores, proves historical-citation
  verification, and pages one frozen ordering across CLI → MCP → CLI. Frozen
  cursor-vector, malformed/query mismatch, ingest-staleness, field-bound, and
  transport-trimming tests lock the protocol and adapter boundaries.
- [x] Add the checked-in 25-query public gold set, graded labels, and frozen
  evaluation manifest before changing retrieval weights or making comparative
  quality claims. Completed 2026-08-01: `hsum-public-gold-25-v1` freezes five
  tasks in each of five query classes, 0/1/2 document judgments, answer
  patterns, a JSON Schema, a SHA-256 pin, an isolated corpus builder, and three
  independent index builds. hSUM has the best mean quality in this internal
  baseline but is much slower than grep and varies materially across fresh
  builds; it is the frozen lexical control for later experiments. See
  `benches/agent_ab/README.md`.
- Preserve the frozen gold labels while diagnosing fresh-index ranking
  variance. Independent builds of identical corpus bytes produced materially
  different hSUM ordering while grep quality was unchanged. Do not claim
  cross-build deterministic ranking until the identity-dependent tie path is
  isolated and fixed without changing the intended lexical scoring contract;
  keep exact measurements in `benches/agent_ab/` outside the evaluated corpus.
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

- [x] Add a strict generic JSONL snapshot connector with bounded records,
  canonical source identity, authoritative deletion semantics, and fuzzed
  parsing. Completed 2026-08-01 for the connector core: whole-file UTF-8 JSONL
  validation, duplicate-key/ID rejection, decoded-content citation coordinates,
  stable `(source UUID, id)` identity, explicit and authoritative tombstones,
  deletion confirmations, atomic multi-JSONL-source carry-forward, strict
  pre-mutation abort, one generation/epoch switch per successful batch,
  schema/doctor coverage, immutable Get with `snapshot_only` freshness,
  disk/quota preflight and quota status, and property tests are verified.
- [x] Expose selected-project JSONL source add/list/confirmed-remove and atomic
  mixed filesystem/JSONL ingest. Completed 2026-08-01 with repeatable source
  selectors, strict pre-mutation abort, default partial exit `6`, soft source
  detachment, active-search exclusion, and historical citation reads.
- [x] Support named project create/list/use, project-local JSONL attach/detach,
  and filesystem-root replacement without weakening project filtering or MCP
  binding isolation. Completed 2026-08-01 with stable binding UUID retargeting,
  scope-revision-only membership changes, broad-root refusal, compensating
  trust rollback, orphan-source retirement/reactivation, historical Get, and a
  two-root real-process lifecycle test.
- [x] Add Alpha.2 Doctor integrity, bounded repair, and support-report modes.
  Completed 2026-08-01 by retaining the full read-only scan for bare Doctor and
  `--integrity`, limiting confirmed repair to abandoned generation rows under
  the writer lock with full pre/post validation, and writing private,
  non-overwriting canonical reports that exclude bodies, queries, source
  identities, connector keys, and paths.
- [x] Add explicit `backup`, `prune`, and migration plan/apply ceremonies with
  capacity estimates, immutable plan hashes, confirmation, rollback guidance,
  and released N-1 fixtures. Completed 2026-08-01 with verified sidecar-free
  online backups, schema-2-to-3 fixture coverage, exact live re-planning under
  the writer lock, non-overwriting outputs, and post-apply Doctor validation.
- [x] Define prune citation impact by immutable document-revision namespace. The
  manifest must enumerate affected version identities and their canonical
  stored-chunk citations; every resolvable composite `get` citation under an
  affected index/source/document/revision tuple is invalidated by that same
  namespace without requiring read-time citation issuance tracking. Completed
  2026-08-01 with canonical JSON manifests stored in the schema-3 audit ledger,
  a monotonic history-floor model, and real-process citation invalidation.
- [x] Add durable `forget` and guarded `restore` only after body-free
  tombstones, copied-database behavior, old-reader fencing, backup interaction,
  and physical-rewrite crash recovery are proven. Completed 2026-08-01 with a
  pruned-baseline requirement, connector-key suppression across every future
  revision, an external body-free hash chain and replacement epoch, shared
  reader/exclusive replacement fencing, atomic compacted replacement,
  recovery/safety backups, a forget-emitted exact-state restore plan,
  idempotent resume, raw-byte/copy/process coverage, and checkpoint crash tests.
- [x] Add hashed user-configuration and trust-registry migration without
  implicit mutation. Completed 2026-08-01 with current-plus-N-1 diagnosis,
  schema/epoch validation, exact source and target hashes, private pre-change
  backups, two-file writer locking, stale/confirmation refusal, structural
  plan validation, atomic per-file replacement, interrupted-apply resume, and
  real-process proof that ordinary context reports `MIGRATION_REQUIRED`
  without rewriting either file.
- [x] Add explicit filesystem-source registration without implicit attachment
  or ingest. Completed 2026-08-01 with canonical-directory and broad-root
  safety checks, conservative persisted discovery policy, exact
  name/root/config idempotency, collision refusal, transactional capacity
  enforcement, orphan reactivation with stable UUID, and process proof that
  registration leaves project scope, trust selection, generations, epochs,
  and evidence unchanged until confirmed `project set-root` activation.
- [x] Add canonical confirmed whole-index deletion. Completed 2026-08-01 with
  exact logical-name targeting, Doctor and UUID/name validation, writer and
  reader fencing, deterministic config/trust locking, configured-default and
  all-binding cleanup, fixed-path quarantine commit/resume, parent-directory
  durability reporting, stale-pointer non-authorization, other-index
  preservation, and real-process lock/failure/recovery proof.
- [x] Finish the remaining canonical Alpha.2 management surface before local
  semantic retrieval. Completed 2026-08-01 with a bounded private registry for
  every CLI-created index backup, lossless path identity, selected/all-index
  inventory, exact receipt revalidation, crash-resumable reservations, and a
  required `--keep-managed-backups` or `--purge-managed-backups` forget choice.
  Purge preflights every entry before evidence mutation, deletes only unchanged
  private single-link sidecar-free files, clears missing records idempotently,
  and leaves user-created copies and forensic storage remnants explicitly out
  of scope. Real-process tests cover missing-choice refusal, keep/purge,
  changed-backup preflight, untracked-copy preservation, and inventory cleanup.

## P2 — Local semantic retrieval

- Evaluate an explicit, pinned local model install/import workflow; alpha.1 has
  no model or network path.
  - Current-checkout checkpoint: the manifest-backed artifact lifecycle is
    implemented for `bge-small-en-v1-5-fp32`. Install is the sole HTTPS path;
    air-gapped import, inventory, full verification, and pin-aware removal use
    the same content-addressed cache. An opt-in FastEmbed `5.17.4` adapter now
    re-hashes those bytes, pins CLS pooling, validates normalized 384-value
    outputs, and records fixed CPU latency/RSS evidence without storing
    vectors. Native Linux x86_64 and macOS arm64 v2 probes pass, record all
    3,456 reference components plus exact model/runtime/input provenance, and
    pass the preregistered cross-architecture component, vector, cosine,
    distance, and deterministic-ordering contract. Production schema-v4 vector
    storage now passes the complete local gate; native storage and stable
    release qualification remain open.
- Prove filtered vector scope correctness, deterministic equal-distance
  ordering, portable SQLite/vector packaging, memory bounds, cancellation, and
  air-gapped model import before exposing semantic or hybrid modes.
  - Current-checkout checkpoint: raw `sqlite-vec 0.1.7` proves filtered scope,
    WAL reader visibility, shadow-slot flips, bounded memory, and corruption
    refusal, but its cutoff membership depends on insertion order and it masks
    `SQLITE_INTERRUPT` as `chunks iter error`. The canonical storage revision
    uses filtered `K+1` plus a source/slot-scoped exact tie fallback and maps the
    generic error only from owned cancellation/deadline state. The complete
    local gate passes. Native workflow run `30732989326` passes on Linux x86_64
    and macOS arm64 with identical revised tie membership and dependency
    identity, closing the portability prerequisite for production vector
    storage.
  - Current-checkout storage/lifecycle checkpoint: schema v4 pins one immutable
    embedding profile per index, stores canonical 25-field provenance and
    content-addressed vectors, and maintains active membership in sqlite-vec
    A/B shadow slots. `init --embedding-model` is offline and pins without a
    download; `ingest --reembed` verifies the exact installed artifact, skips
    cached inputs before inference, preflights capacity, batches at most eight,
    and atomically flips a complete generation. Doctor validates provenance,
    cache, and slot parity. Backup preserves vectors exactly; prune reclaims
    only historical cache/provenance; physical forget removes affected vector
    evidence; guarded restore recovers it byte-for-byte. Focused suites, the
    serialized complete local test gate, formatting, and strict all-feature
    Clippy pass. Native target proof remains open.
  - Current-checkout filtered-retrieval checkpoint: semantic core requests now
    accept exactly one validated, finite, normalized 384-value query vector and
    refuse absent or model-incompatible active vector generations. One SQLite
    read snapshot selects the active A/B slot, enumerates at most 64 active
    project sources, and applies each source UUID as the sqlite-vec partition
    predicate before KNN. Each source uses filtered K+1 at K=50 and an exact
    scalar cosine/rowid fallback only for cutoff ties; merged candidates use
    distance, source UUID, document UUID, and start-byte ordering before the
    existing guarded passage decoder and deterministic dedupe path. Adversarial
    source-isolation and reverse-insertion tie tests, semantic lifecycle and
    validation fixtures, all lexical search contracts, the serialized complete
    local suite, formatting, and strict Clippy pass.
  - Current-checkout semantic-worker checkpoint: query inference is isolated in
    exactly two private child processes with at most eight queued leader jobs;
    identical in-flight requests coalesce without weakening that admission
    bound. Requests are limited to 4,096 UTF-8 bytes and bounded JSON-lines
    frames, use only the verified local artifact cache, and return exact typed
    missing, unverified, incompatible, busy, restarting, cancelled, deadline,
    and protocol states. Callers discard cancelled or late results; an overdue
    child is killed after a two-second grace period and replaced before later
    work. Focused tests cover cancellation/deadline discard, hard admission,
    coalescing, replacement, frame/output validation, real child termination,
    a cancel storm that leaves lexical SQLite search responsive, and a real
    offline missing-model process exchange. The serialized complete local
    suite, formatting, and strict all-feature Clippy pass.
  - Current-checkout hybrid checkpoint: one bounded loop adaptively combines at
    most three independently ranked exact, BM25, and filtered-vector lists at
    depth increments of 50. The frozen integer weighted reciprocal-rank formula
    gives exact evidence 1.5 times the lexical/vector contribution, then uses
    deterministic source/document/span tie order. Same-content and inclusive
    50%-overlap suppression preserve duplicate citations, rank explanations are
    bounded to the contributing lists, and explicit lexical/semantic modes do
    not acquire unintended retrievers. Frozen-score, equal-rank, overlap, and
    three-list integration fixtures pass without changing the 15-query lexical
    contract suite.
  - Current-checkout public-transport checkpoint: CLI and project-bound
    read-only MCP expose `auto`, `lexical`, `hybrid`, and `semantic` through the
    shared application use case. Exactly one two-worker service is shared per
    process; model inference occurs before SQLite retrieval and a one-statement
    snapshot preflight plus deadline-bounded retry prevents generation mixing.
    Auto reports typed degraded/hint state and falls back only under the
    documented model/queue conditions; explicit vector modes return typed
    failures. CLI/MCP share mode, retriever counts, component timings,
    explanations, degradation, hints, and an effective-retriever cursor
    fingerprint. The 22-case vector suite, CLI/MCP/process parity suites, and
    the repository-owned `cargo xtask check` gate pass. Native product-mode
    evidence and stable release qualification remain open.
- [x] Build and freeze the canonical held-out evaluation: 100 queries over
  three independently structured corpora, accepted byte spans, four-point
  labels, 35 semantic/paraphrase cases, report-only ripgrep/QMD comparisons,
  NDCG@10, MRR@10, exact-token top-three recall, and deterministic
  10,000-resample paired bootstrap intervals. The frozen harness, manifest,
  labels, variance diagnosis, raw result, and rendered projection live in
  `eval/`; the measured macOS arm64 result is
  `eval/results/heldout-v1-2026-08-02-macos-arm64.{json,md}`.
  Hybrid passed the semantic positive-value (+0.4371 NDCG@10; 95% lower
  +0.3124) and NDCG non-inferiority gates, but failed MRR non-inferiority
  (lower -0.0312 versus the required -0.02) and the exact-token top-three
  gate (0.6571 versus lexical 0.7429). The required disposition is therefore
  stable lexical-first with hybrid explicitly beta; no held-out labels or
  retrieval weights were changed.

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
