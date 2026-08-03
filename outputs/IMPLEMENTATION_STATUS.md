# hSUM current checkout implementation status

**Snapshot:** 2026-08-03
**Scope:** Current source checkout after the public `0.1.0-alpha.4` prerelease;
published alpha.2 evidence is retained below as a historical release record.

This matrix maps the published alpha.2 surface to implementation, test, and
release evidence. “Implemented” means a code path and focused tests exist in
this checkout; the sections below separately record clean-runner, signed-tag,
artifact, and production-documentation evidence.

The canonical stable-v0.1 evidence states and remaining qualification work are
tracked explicitly in `outputs/STABLE_V0_1_COMPLETION_LEDGER.md`.

| Surface | Checkout status | Primary implementation | Focused evidence |
|---|---|---|---|
| CLI grammar and stable exits | Implemented | `src/cli.rs`, `src/runtime.rs` | `tests/cli_contract.rs`, `tests/runtime_process.rs` |
| Safe init and idempotent root identity | Implemented | `src/app/init.rs` | `tests/init_smoke.rs` |
| Pipeline-fingerprint recovery | Implemented | `src/app/init.rs`, `src/store/doctor.rs`, `src/store/open.rs` | `tests/init_smoke.rs`, `tests/runtime_process.rs`, subprocess crash tests, released-alpha.1 upgrade smoke |
| Trust registry, pointer hint, selection precedence | Implemented | `src/config/`, `src/app/context.rs` | `tests/trust_registry.rs`, `tests/context_resolution.rs`, `tests/config_paths.rs` |
| Bounded filesystem discovery | Implemented for Unix alpha target | `src/ingest/filesystem.rs` | `tests/filesystem_ingest.rs` |
| Deterministic chunking and literal extraction | Implemented | `src/ingest/chunk.rs`, `src/ingest/literals.rs`, `src/ingest/quote_bloom.rs` | `tests/chunking.rs`, `tests/literals.rs`, `tests/quote_bloom.rs` |
| Atomic generations and deletion guards | Implemented | `src/store/generation.rs`, `src/store/lock.rs` | `tests/ingest_generations.rs`, `src/app/tests.rs`, `tests/multiprocess.rs` |
| Immutable SQLite evidence store | Implemented at schema 4 | `src/store/open.rs`, `src/store/schema.rs`, migrations `0001` through `0004` | `tests/store_foundation.rs`, `tests/store_doctor.rs`, `tests/vector_storage.rs` |
| Exact/quoted/BM25 retrieval and deterministic fusion | Implemented | `src/search/query.rs`, `src/search/retrieval.rs` | `tests/query_contract.rs`, `tests/search_contract.rs` |
| Canonical citation and historical `get` | Implemented | `src/domain/citation.rs`, `src/search/get.rs` | `tests/get_contract.rs` |
| Shared read-only Search/Get/Status application use cases | Implemented | `src/app/search_evidence.rs`, `src/app/evidence.rs`, `src/app/status_evidence.rs`, adapters in `src/runtime.rs` and `src/mcp.rs` | CLI/MCP ranking, packet, cursor, bounds, cancellation, historical-citation, and Status-field parity in `tests/runtime_process.rs` and `tests/mcp_contract.rs` |
| Shared `hsum.api.v1` protocol DTOs and cursor | Implemented for Search, Get, and Status | `src/protocol/evidence.rs`, `src/protocol/status.rs`, `src/protocol/cursor.rs`, adapters in `src/runtime.rs` and `src/mcp.rs` | Complete normalized CLI/MCP Get-object, Search-core, and Status-core equality plus CLI → MCP → CLI pagination in `tests/runtime_process.rs`; frozen cursor vector and MCP schema/transport contracts in module tests and `tests/mcp_contract.rs` |
| Frozen lexical retrieval evaluation | Implemented as development evidence | `benches/agent_ab/tasks.json`, `tasks.schema.json`, `tasks.sha256`, `benchmark.py` | 25 balanced queries, graded document labels, deterministic validation, isolated equal-corpus snapshots, 20 harness tests, three raw index builds plus aggregate JSON/HTML/SVG; hSUM leads mean quality but is slower and has unresolved cross-build rank variance |
| Bounded source-drift observation | Implemented for filesystem evidence on the Unix alpha target; JSONL evidence is explicit `snapshot_only` | `src/status.rs`, `src/app/stored_source.rs` | `tests/source_drift.rs`, `tests/runtime_process.rs`, `tests/jsonl_connector.rs` |
| Status and actionable storage/source problems | Implemented | `src/status.rs`, `src/store/capacity.rs` | `tests/store_foundation.rs`, `tests/runtime_process.rs` |
| Full Doctor plus bounded abandoned-row repair and body-free report | Implemented; repair/report unreleased | `src/store/doctor.rs`, Doctor CLI/runtime adapters, private canonical report writer | invariant/heap/body-free/idempotent-repair coverage in `tests/store_doctor.rs`; grammar and real-process report/non-overwrite/repair coverage in `tests/cli_contract.rs` and `tests/runtime_process.rs` |
| Capacity, quota, locality, and durability preflight | Implemented | `src/store/capacity.rs`, `src/store/open.rs` | module tests plus init/ingest process tests |
| Four-tool, project-bound, read-only MCP stdio | Implemented; tool calls do not initialize or ingest | `src/mcp.rs` | `tests/mcp_contract.rs`, `tests/runtime_process.rs` |
| Generated client snippets and local client probe | Implemented | `src/runtime.rs` | `tests/runtime_process.rs` |
| Shared public error taxonomy | Implemented | `src/domain/error.rs`, transport mappings in `src/runtime.rs` and `src/mcp.rs` | domain, CLI, MCP, and process contract tests |
| Completion and man generation | Implemented | `src/cli.rs` | `tests/cli_contract.rs` |
| Strict JSONL snapshot connector core | Implemented and exposed through selected-project source commands | `src/ingest/jsonl.rs`, `src/app/jsonl_connector.rs`, source-kind-aware generation/schema/doctor/Get/status paths | parser property tests plus `tests/jsonl_connector.rs` for decoded offsets, identity, deletion guards, no-prefix failure, default carry-forward, strict abort, one-generation multi-source commit, immutable Get, `snapshot_only`, and preflight/quota reporting |
| Selected-project JSONL source management and mixed ingest | Implemented; unreleased | `src/cli.rs`, `src/runtime.rs`, `src/app/source_management.rs`, `src/app/project_ingest.rs`, `src/store/source.rs`, `src/store/generation.rs` | CLI grammar in `tests/cli_contract.rs`; real-process add/list/idempotency/conflict/default/strict/remove/re-add lifecycle in `tests/jsonl_cli.rs` |
| Named multi-project and filesystem-source management | Implemented; unreleased | `src/app/project_management.rs`, `src/store/project.rs`, trust-binding retargeting in `src/config/trust.rs`, CLI/runtime adapters | grammar and confirmation contracts in `tests/cli_contract.rs`; two-root create/list/use/attach/detach/set-root/isolation/history/init lifecycle in `tests/project_cli.rs`; collision-safe retargeting in `tests/trust_registry.rs` |
| Explicit filesystem-source registration | Implemented; unreleased | `source add fs` in `src/cli.rs` and `src/runtime.rs`; validation in `src/app/source_management.rs`; transactional catalog registration/reactivation in `src/store/source.rs` | grammar in `tests/cli_contract.rs`; real-process canonicalization, metadata-only registration, exact idempotency, conflict/broad-root refusal, activation UUID reuse, no implicit ingest, and stable-UUID reactivation in `tests/filesystem_source_cli.rs` |
| Confirmed whole-index deletion | Implemented; unreleased | `index delete NAME --confirm` in `src/cli.rs` and `src/runtime.rs`; fenced reference cleanup and quarantine removal in `src/app/index_management.rs`; identity-checked bulk trust cleanup in `src/config/trust.rs` | grammar in `tests/cli_contract.rs`; real-process missing/malformed/reader-busy refusal, byte-preserving pre-commit failures, configured-default/all-binding cleanup, other-index isolation, stale-pointer safety, name reuse, and quarantine resume in `tests/index_delete_cli.rs`; epoch/idempotency coverage in `tests/trust_registry.rs` |
| Verified backup, prune/migration, durable forget, and guarded restore | Implemented; unreleased | `src/store/maintenance.rs`, `src/store/forget_ledger.rs`, `migrations/0003_maintenance.sql`, shared reader/exclusive replacement fences, CLI/runtime adapters | store-level explicit prune-selector/backup/stale-plan/history-floor/N-1 plus forget/copy/raw-byte/old-reader/reingest/old-backup/restore/checkpoint-resume fixtures in `tests/maintenance.rs`; real-process prune/migration/forget/restore in `tests/maintenance_cli.rs`; replacement-lock process coverage in `tests/multiprocess.rs` |
| Managed-backup inventory and forget disposition | Implemented; unreleased | bounded global registry in `src/store/managed_backup.rs`; lossless path encoding, pending/completed receipts, exact classification and keep/purge in `src/runtime.rs`; `backup list` plus mutually exclusive required forget flags in `src/cli.rs` | grammar in `tests/cli_contract.rs`; real-process inventory, missing-choice non-mutation, keep/purge, changed-backup preflight, untracked-copy preservation, and cleanup in `tests/managed_backup_cli.rs` |
| Pinned local model and five-state index lifecycle | Implemented; complete local gate passing, unreleased | embedded exact BGE-small manifest and content-addressed artifact cache in `assets/models/` and `src/model/`; immutable index profile, offline pin-at-init, explicit install/import/list/verify/remove, and configured/installed/indexed/degraded state derivation in `src/cli.rs` and `src/runtime.rs` | model unit coverage plus grammar, pin-without-download, missing-artifact, exact-profile, and state process coverage in `tests/cli_contract.rs`, `tests/model_cli.rs`, and `tests/vector_storage.rs` |
| FastEmbed CPU portability probe | Native prerequisite passes; verified inference is used by explicit product re-embed and private query workers | verified-byte adapter in `src/model/inference.rs`; fixed release-mode harness and inputs in `examples/` and `benches/model_portability/`; native matrix in `.github/workflows/model-portability.yml` | v2 reports from run `30731369195`: Linux x86_64 259.7 ms init/10.29 ms batch-1 p95/72.08 ms batch-8 p95/319.5 MB peak RSS growth; macOS arm64 237.2 ms/9.00 ms/130.73 ms/493.2 MB; native public-mode and stable evaluation evidence remain open |
| Cross-architecture embedding numerical contract | Native target evidence passing; still an opt-in development gate | `hsum.embedding-provenance.v1` in `src/model/inference.rs`; raw-float v2 probe and preregistered comparator in `examples/`; fan-in CI job in `.github/workflows/model-portability.yml` | all 3,456 components from run `30731369195` pass: `1.00e-7` max component delta, `6.94e-7` vector L2, `2.32e-13` cosine distance, `2.89e-7` pairwise distance delta, zero ordering mismatches, compatible model/runtime/input provenance |
| sqlite-vec portability and filtered-KNN disposition | Native target prerequisite passing; accepted backend is now used by product storage | pinned `sqlite-vec 0.1.7`, composite probe in `examples/sqlite-vec-portability.rs`, accepted bounded adapter in `docs/superpowers/specs/2026-08-02-sqlite-vec-portability-disposition.md`, native workflow | run `30732989326` passes the revised contract on Linux x86_64 at 64.54 ms p95/8.01 MB RSS growth and macOS arm64 at 46.75 ms/10.90 MB for 3,200 candidates; raw cutoff/interrupt failures, broken `0.1.10-alpha.4` package, and 674.68 ms all-source-tie knee remain retained |
| Product embedding storage and re-embed lifecycle | Implemented; complete local gate passing, unreleased | schema-v4 immutable profile and generation pins, canonical provenance/cache, native sqlite-vec A/B membership, single-writer bounded re-embed and capacity preflight in `src/store/vector.rs`, `src/store/doctor.rs`, `src/runtime.rs`, and `migrations/0004_vector_storage.sql` | 22 storage/lifecycle/retrieval cases in `tests/vector_storage.rs`; model/init process coverage; complete repository-owned contributor gate; strict all-target/all-feature Clippy |
| Vector-aware maintenance | Implemented; complete local gate passing, unreleased | vector-aware backup, prune, physical forget, guarded restore, and ordinary-ingest invalidation in `src/store/maintenance.rs`, `src/store/generation.rs`, and `src/store/vector.rs` | exact preservation/reclamation/removal/recovery fixtures in `tests/vector_storage.rs`, `tests/maintenance.rs`, and `tests/maintenance_cli.rs` |
| User config and trust-registry migration | Implemented; unreleased | schema-2 config/trust loaders and epochs in `src/app/context.rs` and `src/config/trust.rs`; hashed two-file ceremony in `src/config/migration.rs`; CLI/runtime adapters | library refusal/exact-backup/structural-plan/resume coverage in `tests/config_migration.rs`; N-1 non-mutation and complete process ceremony in `tests/config_migration_cli.rs`; CLI grammar and schema diagnosis fixtures |
| Remaining canonical Alpha.2 management surfaces | Complete in the current checkout; unreleased | No intentionally absent Alpha.2 management surface remains | Full local gate must continue passing before semantic retrieval work begins |
| Semantic/hybrid retrieval | Implemented through the public CLI/MCP/API boundary; complete local gate passing, unreleased and not yet promoted | filtered semantic KNN and deterministic weighted exact/BM25/vector reciprocal-rank fusion in `src/search/retrieval.rs`; bounded explanations and overlap dedupe; two-process/eight-queue offline inference in `src/model/worker.rs`; snapshot-bound orchestration in `src/app/search_evidence.rs`; CLI/MCP modes and shared protocol fields in `src/cli.rs`, `src/runtime.rs`, `src/mcp.rs`, and `src/protocol/` | frozen integer fusion example, overlap/equal-rank fixtures, 22 vector cases, 15 lexical contracts, seven worker cases plus real child exchange, 26 CLI contracts, 31 MCP contracts, 13 process contracts, typed missing-model/degradation parity, execution-fingerprint cursor test, and the complete repository-owned contributor gate |
| Held-out retrieval promotion evaluation | Complete; stable lexical-first and hybrid beta | frozen 100-query / three-corpus schema, strict standard-library harness, raw tool outputs, deterministic renderer, and lexical cross-build diagnosis in `eval/` | macOS arm64 result `eval/results/heldout-v1-2026-08-02-macos-arm64.json` binds manifest `a7771fac…`; semantic gain and NDCG non-inferiority pass, but MRR lower bound (-0.0312 < -0.02) and exact-token top-three non-regression fail, so the canonical gate forbids promotion |
| Watcher and HTTP | Not implemented; canonical stable-v0.1 exclusions | Intentionally absent from the current command surface | Rejected-surface assertions in `tests/cli_contract.rs` |
| GitHub release archives | Alpha.4 published | `.github/workflows/release.yml`, `scripts/release-smoke.sh` | Checksums, SPDX SBOM attestations, signed-tag guard, Linux/macOS release jobs |
| Installer | Available in alpha.4 | `scripts/install.sh`, `scripts/installer-smoke.sh` | Checksum verification plus isolated/network-denied smoke |
| crates.io package | Not available | `Cargo.toml` sets `publish = false` | Explicitly deferred; no crates.io claim |

## Current invariants

- Runtime ingest and retrieval are local. The sole network path is the explicit
  `model install embedding` command; `HSUM_OFFLINE=1` disables it too. MCP uses
  the process's stdin and stdout.
- Each selected context remains bound to exactly one named project and one
  active filesystem authority, with zero or more attached JSONL snapshot
  sources. One managed index can contain multiple bounded projects and sources;
  binding-based selection remains exact and project-scoped.
- MCP opens the selected database read-only and query-only, exposes no mutation
  tool, and does not initialize or ingest as a side effect of retrieval.
- Indexed content is untrusted and is marked as such in CLI JSON and MCP
  evidence.
- Generations activate atomically; an all-failed scan does not advance the
  active generation or epoch.
- Pinned indexes bind one exact model revision, artifact fingerprint,
  dimension, input contract, and provenance schema. Explicit re-embed builds a
  complete shadow sqlite-vec slot from canonical cached inputs before one
  generation/epoch/slot transaction publishes it; ordinary ingest invalidates
  active vector membership until the next complete re-embed.
- Confirmed pruning preserves the monotonic index epoch while moving the
  retained-history floor forward; its canonical manifest and affected revision
  namespaces remain recorded in the index.
- Canonical citations bind index, source, document, revision hash, and byte
  span.
- CLI `get` and MCP `evidence_get` resolve evidence, capture the cited drift
  target, and derive source/hash state through one read-only application
  handler, then construct the complete wire object through one shared protocol
  DTO. Live verification compares against the immutable cited revision,
  including when the indexed document head has advanced.
- CLI `search` and MCP `evidence_search` execute ranking, bounded page
  selection, cursor snapshot validation, and live-source observation through
  one read-only application handler, then derive response metadata, passage
  identity, score, duplicate citations, and typed source state through one
  protocol mapping. Compatibility projections preserve the CLI
  combined-span/`name` diagnostics layout and MCP split-span/`retriever`
  transport layout. For pinned indexes, query inference runs before the SQLite
  retrieval transaction and is accepted only when a one-statement preflight
  snapshot matches the response snapshot; concurrent generation changes retry
  within the original deadline. Both adapters use one opaque query/snapshot-
  and effective-retriever-bound cursor, while MCP retains final wire-size
  trimming as a transport responsibility.
- CLI `status` and MCP `evidence_status` read index identity, project counts,
  source health, and shared actionable problems through one read-only
  application handler and one SQLite snapshot, then construct health issues,
  repair objects, and their compatibility envelopes through one protocol
  mapping. Storage inspection happens only after that snapshot closes; the
  adapters retain their public packet shapes.
- Storage preflight reserves the greater of 64 MiB or 10% of managed index
  bytes in addition to the estimated write.

## Current-checkout qualification

- The public alpha.4 tag contains the original zero-touch auto-enrollment and
  once-per-task refresh behavior. Current source restores the canonical
  read-only MCP boundary; that change is unreleased until a later tag.
- Existing alpha.4 workspace-policy files remain parseable, but their roots are
  not consumed by MCP. Explicit `integration activate`, `init`, and `ingest`
  remain the state-changing paths.
- The B1-05 through B1-13 product storage/lifecycle, filtered-semantic,
  bounded semantic-worker, deterministic hybrid-fusion, overlap-dedupe,
  explanation, and public transport slice passes its 22-case vector
  integration suite, raw-adapter and frozen-fusion unit cases, seven worker
  unit cases, a real private-worker process exchange, all 15 lexical search
  contracts, CLI/MCP/process parity and typed model-state coverage, and the
  repository-owned `cargo xtask check` gate. The frozen 100-query held-out
  evaluation is complete and requires the lexical-first / hybrid-beta
  disposition recorded in `eval/results/heldout-v1-2026-08-02-macos-arm64.md`.
  Native B1-09/B1-10/B1-13 product evidence, cross-client dogfood, and stable
  release qualification are not yet claimed.
- The frozen local retrieval benchmark is 25 author-labeled tasks on this
  repository, not external or stable-release evidence. Across three fresh
  indexes, hSUM leads mean graded quality but is much slower than grep, misses
  every paraphrase and multi-evidence task, and changes ordering materially
  across identical-corpus rebuilds. Exact scores live outside the evaluated
  corpus in `benches/agent_ab/README.md` with the full protocol and limitations.

## Local Alpha.2 JSONL source-lifecycle evidence on 2026-08-01

- `cargo xtask check` passed on this checkout: formatting, Clippy with warnings
  denied, all unit and integration tests, and doctests.
- The JSONL integration suite passed all 11 cases covering strict whole-file
  failure, exact decoded offsets, stable identity, explicit and absence
  deletion guards, quota preflight, default carry-forward, strict batch abort,
  one-generation multi-source commits, immutable Get, and `snapshot_only`
  reporting.
- The real-process lifecycle suite covers add/list, exact idempotency, conflict
  refusal, mixed dry-run and ingest, default partial and strict abort behavior,
  one shared generation, all-failed preservation, confirmed removal, active
  retrieval exclusion, historical Get, and same-UUID reattachment.
- The named-project process suite qualifies create/list/use, project-local
  source attach/detach, root replacement across two repositories, active scope
  isolation, historical Get, persistent binding retargeting, and compatible
  init reruns. Trust tests additionally freeze collision refusal without
  registry mutation.
- The filesystem-source registration process suite proves canonical bounded-root
  registration without attachment or ingest, exact idempotency and collision
  refusal, unchanged scope/epoch/trust selection, registered-UUID reuse during
  confirmed root activation, and same-UUID reactivation after retirement.
- The confirmed-index-deletion process suite proves exact name targeting,
  reader-fence refusal without mutation, malformed-config and recovery-conflict
  refusal, configured-default and trust cleanup, other-index isolation,
  stale-pointer non-authorization, immediate name reuse, and fixed-quarantine
  resume after interrupted cleanup.

## Published alpha.2 evidence (historical)

- Annotated GPG-signed tag `v0.1.0-alpha.2` resolves to protected-branch merge
  `ef54758`; its public GitHub prerelease contains macOS arm64 and Linux x86_64
  archives, per-asset checksums, `SHA256SUMS`, and per-target SPDX SBOMs.
- Release workflow run `30187380449` passed the signed-tag guard, both target
  builds, isolated same-runner rebuilds, first-user and no-network smoke tests,
  released-alpha.1 recovery smoke, SBOM attestations, and publication.
- A post-publication audit downloaded both archives and verified their
  per-asset and manifest checksums. The downloaded macOS executable also passed
  the release, no-network, and alpha.1 recovery smoke suites.
- The macOS executable has a linker-generated ad-hoc signature, no Developer ID
  identity, and no notarization. Its Gatekeeper workaround remains documented.
- The production alpha.2 documentation tree and `llms.txt` resolve over HTTPS.
  The repository also records all 175 locked Cargo package license declarations
  in `outputs/hsum-v0.1.0-alpha.2-cargo-licenses.json`.
- No crates.io package, installer, Apple-signed/notarized binary, benchmark,
  retrieval evaluation, external dogfood, or cross-client-version result is
  claimed.

Run `cargo xtask check` for the checkout-wide contributor gate. Record the
actual result separately; the presence of tests does not substitute for the
protected clean-runner and signed-tag gates.

## Local evidence actually run on 2026-07-26

These are single-machine developer results on this checkout, not release,
platform, or clean-machine evidence:

- `cargo +1.91.0 xtask check` passed: formatting, Clippy with warnings denied,
  all targets/features tests, and doctests.
- `cargo +1.91.0 build --locked --release` produced the macOS arm64 alpha.2
  candidate from code commit `43a0d22`.
- An isolated second release build matched that candidate byte for byte:
  SHA-256 `ec3fe6ea20c1cdc566369145a13f95f6cf6cef5a0307b8506e0bdd35788d7c2c`.
- The ordinary first-user release smoke and the same smoke with network
  syscalls denied both passed.
- The checksum-pinned published macOS alpha.1 binary created a real index;
  alpha.2 rejected it with `PIPELINE_FINGERPRINT`, rebuilt it, restored search
  and doctor, and returned `NOT_FOUND` for the invalidated old citation.
- Real subprocess exits at four rebuild durability boundaries all remained
  recoverable through ordinary `hsum init`.

This local evidence was necessary but not sufficient for publication; the
clean-runner and post-publication evidence above completed that gate.

## Public release-gate evidence on 2026-07-26

- `main` requires pull requests, an up-to-date branch, `Linux x86_64` and
  `macOS arm64`, conversation resolution, and admin enforcement; force pushes
  and branch deletion are blocked.
- Code commit `43a0d22` on protected PR `burkan2/hSUM#1` passed both required
  jobs in CI run `30185717521`. Each clean runner passed the contributor gate,
  release build, isolated byte-for-byte rebuild, first-user smoke,
  network-denied smoke, and checksum-pinned alpha.1 recovery smoke.
- Private vulnerability reporting is enabled. The release GPG fingerprint and
  public-key repository variables match the local secret signing key.
- Documentation PR `burkan2/hsum-site2#1` was deployed to production and
  retains the frozen alpha.1 tree alongside the published alpha.2 tree.
