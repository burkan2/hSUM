# Changelog

All notable public changes are documented here. hSUM follows semantic versioning
for its release tags. During alpha, only the current tagged release is supported.

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
  the ingest pipeline and schema are unchanged. In-flight MCP pagination
  cursors and retrieval fingerprints remain release-bound and must be
  restarted after upgrading.
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
