# Changelog

All notable public changes are documented here. hSUM follows semantic versioning
for its release tags. During alpha, only the current tagged release is supported.

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
