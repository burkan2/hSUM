# Changelog

All notable public changes are documented here. hSUM follows semantic versioning
for its release tags. During alpha, only the current tagged release is supported.

## Unreleased

### Added

- Added an opt-in FastEmbed CPU portability probe without enabling vectors or
  semantic search. The loader accepts only re-hashed bytes from the verified
  hSUM model cache, pins BGE-small CLS pooling, validates normalized
  384-dimensional output, and records staged latency, deterministic output
  digests, and sampled RSS. Clean GitHub runners pass on Linux x86_64 and macOS
  arm64, with checked-in JSON and native dependency evidence; outputs are
  deterministic within each platform but not bit-identical across them.
  Release license inventory now resolves all Cargo features so the opt-in
  runtime graph is included.

- Added the first Beta.1 model-artifact lifecycle without enabling vectors or
  semantic search. The binary embeds one exact FastEmbed-compatible
  `bge-small-en-v1-5-fp32` manifest with a full upstream revision, required
  file list, byte sizes, SHA-256 checksums, 384-vector dimension, MIT license,
  and HTTPS source. `model install embedding` is the only network path and
  honors offline mode, system proxies, platform roots, an optional PEM CA,
  bounded HTTPS redirects, transient retries, partial cleanup, and atomic
  publication. `model import`, `list`, `verify`, and pin-aware `remove` remain
  local and share the same content-addressed cache contract.

- Added a durable managed-backup inventory and explicit forget-time
  disposition. `backup list [--all] [--json]` reports every CLI-created index
  backup and whether its private file still matches the recorded receipt.
  `forget apply` now requires exactly one of `--keep-managed-backups` or
  `--purge-managed-backups`; purge preflights before evidence mutation, deletes
  only unchanged single-link sidecar-free files, resumes across missing files,
  and never discovers or removes user-created copies.

- Added canonical `hsum index delete NAME --confirm`. The command validates the
  named managed database and UUID, waits for writers and evidence readers,
  clears a matching configured default and every matching trust binding, then
  quarantines and removes only that managed index directory. Fixed quarantine
  naming makes interrupted cleanup resumable; unrelated indexes, external
  backups, filesystem snapshots, and unauthoritative repository pointers are
  retained.

- Added `hsum source add fs PATH --name NAME` for explicit filesystem-source
  catalog registration. It canonicalizes and bounds roots, persists the
  conservative discovery policy, refuses name/root/config conflicts, and
  reactivates exact retired sources with their original UUID. Registration
  does not attach, ingest, retarget trust, or advance project scope and index
  epochs; confirmed `project set-root` activation reuses the registered UUID.

- Added `hsum config migrate plan/apply` for explicit current-plus-N-1 user
  config and trust-registry upgrades. One immutable hash-bound plan covers both
  files; apply revalidates exact live hashes under shared writer locks, creates
  private byte-for-byte backups before replacement, refuses redirected or
  underestimated plans, and resumes safely when an interruption leaves one
  artifact already migrated. Ordinary context and retrieval diagnose old
  schemas without rewriting them.

- Added durable `forget plan/apply` and exact-state `restore apply`
  ceremonies. Forget requires a pruned generation baseline, creates an
  evidence-bearing recovery backup and hashed restore plan, publishes a
  compacted replacement database with body-free namespace tombstones, fences
  old readers, and blocks every future revision of the forgotten logical
  connector. The external hash-chained ledger and replacement epoch also stop
  a pre-forget backup replay from silently re-enabling evidence. Restore
  verifies both the finalized post-forget database hash and recovery backup,
  refuses later live mutation, creates a safety backup, and advances
  evidence/replacement epochs with an append-only restored record.
- Added physical-rewrite lifecycle, copied-database, raw-byte absence,
  old-reader, reingest refusal, exact restore, stale restore, real-process, and
  crash-boundary coverage for forget/restore.
- Added Alpha.2 Doctor surfaces. Bare Doctor and `--integrity` retain the full
  read-only validation pass; `--repair --confirm` removes only abandoned
  generation rows under the writer lock with full pre/post validation; and
  `doctor report --output` writes private, non-overwriting, body-free and
  query-free canonical JSON.

- Added guarded maintenance ceremonies: verified `backup create`,
  citation-aware `prune plan/apply`, and released N-1 `migrate plan/apply`.
  Canonical plan hashes bind index identity, schema and pipeline fingerprints,
  epoch, history, estimates, and complete affected citation namespaces; apply
  re-plans under the writer lock, creates a verified non-overwriting backup,
  and runs Doctor after the transaction.
- Added schema 3 maintenance audit records and a monotonic history floor.
  Pruning can collapse retained generation history to a coherent baseline
  without resetting the index epoch, while later ingests continue normally.

- Added selected-project JSONL snapshot management through
  `hsum source add jsonl`, `hsum source list`, and confirmed
  `hsum source remove`. Registration is idempotent for an exact name/path/config
  match, and removal tombstones active documents while retaining immutable
  historical citation reads.
- Added repeatable `hsum ingest --source ID_OR_NAME` selection and `--strict`
  batch policy. Filesystem and JSONL snapshots now commit successful changes in
  one generation and one epoch switch; default partial batches retain failed
  source heads and exit `6`, while strict or all-failed batches exit `1`
  without partial mutation.
- Added the Alpha.2 strict JSONL complete-snapshot connector core. It enforces
  bounded duplicate-key-aware parsing, stable `(source UUID, id)` identity,
  decoded-content citation offsets, explicit and guarded absence deletion,
  atomic multi-source behavior, and snapshot-only immutable evidence reads.
- Added named project management through `hsum project create`, `list`, `use`,
  and confirmed `set-root`. New projects share the selected filesystem source;
  persistent selection keeps the binding UUID stable; root replacement swaps
  project membership without implicit ingest or loss of historical citations.
- Added project-local `hsum source attach` and `detach`, plus `source list --all`
  for discovering active index sources. Membership changes advance only the
  project scope revision, while global `source remove` remains the separate
  tombstoning operation.

### Changed

- Restored the original read-only MCP contract. Tool calls no longer initialize
  approved workspaces or ingest activated repositories. Repository activation
  and generation changes now require the existing explicit local CLI commands;
  MCP continues to report live-source drift without changing stored evidence.
- Workspace authorization records created by alpha.4 remain readable for
  compatibility, but they no longer enable lazy MCP enrollment.
- CLI `get` and MCP `evidence_get` now execute through one read-only
  application handler. They return equivalent evidence, source-state, hash
  verification, and manual-freshness fields while preserving MCP resource caps
  and cancellation. Live hash verification is bound to the cited immutable
  revision rather than a newer indexed document head. CLI JSON and MCP now
  construct the complete Get packet through one shared `hsum.api.v1` protocol
  DTO.
- CLI `search` and MCP `evidence_search` now execute retrieval, bounded result
  selection, cursor-snapshot validation, and source-drift observation through
  one read-only application handler. The advertised CLI cursor contract is now
  implemented through the same opaque, query- and snapshot-bound codec as MCP;
  cursors can continue across CLI and MCP without changing the frozen alpha.4
  bytes. MCP retains its aggregate body cap, structured-response trimming, and
  cancellation contract. Both interfaces now construct Search response
  metadata, passage identity, scores, duplicate citations, and source state
  through one shared protocol mapping while preserving their existing outer
  envelopes, span layouts, and rank-key layouts.
- CLI `status` and MCP `evidence_status` now read index identity, project
  counts, source health, and actionable problems through one read-only
  application handler and one SQLite snapshot. Storage diagnostics still run
  only after SQLite closes, while each transport preserves its existing public
  packet shape and bounds. Health issues, repair objects, and both compatibility
  envelopes are now constructed through one shared protocol mapping.
- Retrieval development now uses a SHA-256-pinned 25-query gold manifest with
  five balanced query classes and graded document judgments. The first frozen
  three-build working-tree baseline is published without tuning retrieval
  weights. hSUM leads mean graded quality but remains much slower than grep,
  fails every paraphrase and multi-evidence task, and exposes material
  fresh-index ranking variance for later hardening.

### Fixed

- Restored the published alpha.1 recovery path across both versioned stores.
  The release smoke now migrates v1 config/trust through the hash-bound backup
  ceremony, distinguishes N-1 migration from older-schema upgrade guidance,
  validates the frozen alpha.1 schema/checksum without accepting its stale
  pipeline, and permits only explicit dry-run plus citation-invalidating
  rebuild replacement.

- `get` now accepts a citation spanning one or more contiguous whole chunks from
  the same immutable document revision. A `returned_citation_uri` produced by
  CLI or MCP can therefore be passed back to `get` and resolves to the same
  bytes and line span. Arbitrary interior byte ranges remain rejected.

## 0.1.0-alpha.4 — 2026-07-29

### Added

- `hsum integration install codex --activate . --confirm` now completes the
  user-level Codex setup in one idempotent flow: it registers one global hSUM
  MCP server through Codex's supported CLI, installs bounded agent guidance,
  activates the current repository, verifies all four read-only tools, and
  completes a search-to-citation round trip without asking the user to copy
  TOML or restart Codex.
- `integration status`, `repair`, and `uninstall` make the managed Codex
  registration and agent policy inspectable and reversible.
- Users with many repositories can authorize a projects directory once with
  `integration authorize-workspace codex`. The first hSUM tool call inside a
  Git repository below that directory initializes only that exact repository;
  `revoke-workspace` stops future lazy enrollments without deleting existing
  indexes or trust bindings.
- GitHub prereleases now include a version-pinned, checksum-verifying
  `install-hsum.sh`. It installs without `sudo`, registers Codex, activates the
  selected repository, and is exercised by isolated and network-denied smoke
  tests before publication.

### Changed

- Codex uses one workspace-dynamic MCP registration instead of one
  binding-pinned registration per repository. Existing low-level
  `client config` flows for other MCP clients remain binding-pinned.
- Managed Codex repositories attempt one bounded refresh before the first
  search or citation read in each MCP task. If refresh is busy, unsafe, or
  refused by deletion guards, hSUM keeps serving the last good generation and
  reports the freshness state.
- An unactivated repository now returns a structured,
  privacy-explicit activation error with the exact command an agent can run
  after repository-specific consent.

### Compatibility and boundaries

- Alpha.2 and alpha.3 indexes and citations remain compatible with alpha.4;
  the ingest pipeline and schema are unchanged. In-flight search cursors and
  retrieval fingerprints remain release-bound and must be restarted after
  upgrading.
- Retrieval remains local exact/quoted/BM25 lexical search. Alpha.4 adds no
  embedding model, vector search, corpus upload, account, telemetry, daemon,
  or HTTP transport.
- The macOS archive and installer remain neither Developer ID-signed nor
  notarized. The installer requires an explicit
  `--allow-unnotarized-alpha` acknowledgement on macOS after checksum
  verification.

## 0.1.0-alpha.3 — 2026-07-26

### Added

- Every GitHub prerelease now includes a deterministic Cargo-declared license
  inventory for the complete locked dependency graph. The inventory, both
  per-target SPDX SBOMs, and both binary archives are covered by
  `SHA256SUMS`.

### Changed

- GitHub prereleases are assembled as complete drafts before publication, so
  every asset is present before release immutability makes the published
  release permanent.
- The macOS verification guidance now states the exact trust boundary: the
  linker-generated ad-hoc signature passes `codesign --verify`, but the binary
  has no Developer ID identity and is not notarized.

### Compatibility

- The CLI, MCP API, schema, and indexing pipeline are unchanged from alpha.2.
  Existing alpha.2 indexes and citations remain compatible with alpha.3.
  In-flight MCP pagination cursors remain release-bound and must be restarted
  after upgrading.

## 0.1.0-alpha.2 — 2026-07-26

### Added

- Chunk kinds for eleven additional languages: Java, Kotlin, C, C++, Ruby,
  C#, Swift, PHP, Scala, shell, and SQL. Combined with the existing
  Markdown, text, Rust, Python, TypeScript, JavaScript, and Go support,
  alpha.2 indexes twenty-eight lowercase extensions in total.
- Nine of the eleven languages (Java, Kotlin, C, C++, Ruby, C#, Swift, PHP,
  and Scala) chunk at declaration boundaries. Shell and SQL chunk at
  paragraph and line boundaries instead: shell functions are declared as
  `name()` with no leading keyword, and SQL has no declaration concept to
  anchor on.
- MCP server instructions and the four tool descriptions now state when to
  use each tool, not only what it touches.
- `hsum init --rebuild`, with a write-free `--dry-run`, replaces a coherent
  index whose pipeline fingerprint no longer matches the current binary.
  Rebuild preserves unrelated trust bindings and makes the loss of prior
  evidence and citations explicit.
- Release gates now exercise an index created by the checksum-pinned published
  alpha.1 binary, repeat the first-user smoke path with networking denied, and
  require an isolated release build to match the packaged binary byte for byte.

### Changed

- **BREAKING:** the pipeline fingerprint changed from
  `bb24fc64a8602c9ec0479ae687f848b7d5b029294796701966b7bbafc8a23bab` to
  `ff465d499d3e917ab4f468dc6dec0cbf9b343cbbab661190f808c92d79bccf5b`.
  An index created by an earlier build stops opening entirely: every
  command, including `search`, `status`, `context`, `ingest`, and `doctor`,
  fails metadata validation with "pipeline fingerprint does not match this
  binary" (error subcode `PIPELINE_FINGERPRINT`). The check runs on every open,
  not only under `doctor`.

  `hsum ingest` remains strict and cannot repair such an index. Run
  `hsum init --rebuild --dry-run`, then `hsum init --rebuild`. The command
  validates the old index except for the recognized fingerprint mismatch,
  replaces only its trust binding and managed database, and re-ingests from
  source. Evidence and citations from the replaced index do not survive.

  Subprocess fault tests terminate rebuild after trust removal, database
  removal, replacement creation, and replacement registration. Every
  interrupted state remains recoverable through ordinary `hsum init`.

## 0.1.0-alpha.1

### Added

- Local filesystem ingest into private SQLite generations with immutable
  citations.
- Deterministic exact and lexical BM25 evidence retrieval.
- A project-bound, read-only MCP stdio server exposing evidence search, get,
  project, and status tools.
- Read-only integrity diagnosis, client-configuration generation, shell
  completions, a manual page, and version-pinned offline error help.

### Boundaries

- Supported alpha targets are macOS arm64 and Linux x86_64 local filesystems.
- No account, model download, telemetry, HTTP server, remote connector, vector
  search, watcher, backup, restore, prune, or migration command is included.
- The crate is not published to crates.io; the first public alpha is distributed
  through verified GitHub Release assets only.
