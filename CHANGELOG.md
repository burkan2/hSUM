# Changelog

All notable public changes are documented here. hSUM follows semantic versioning
for its release tags. During alpha, only the current tagged release is supported.

## Unreleased

### Added

- Chunk kinds for eleven additional languages: Java, Kotlin, C, C++, Ruby,
  C#, Swift, PHP, Scala, shell, and SQL. Combined with the existing
  Markdown, text, Rust, Python, TypeScript, JavaScript, and Go support,
  alpha.1 now indexes twenty-eight lowercase extensions in total.
- Nine of the eleven languages (Java, Kotlin, C, C++, Ruby, C#, Swift, PHP,
  and Scala) chunk at declaration boundaries. Shell and SQL chunk at
  paragraph and line boundaries instead: shell functions are declared as
  `name()` with no leading keyword, and SQL has no declaration concept to
  anchor on.
- MCP server instructions and the four tool descriptions now state when to
  use each tool, not only what it touches.

### Changed

- **BREAKING:** the pipeline fingerprint changed from
  `bb24fc64a8602c9ec0479ae687f848b7d5b029294796701966b7bbafc8a23bab` to
  `ff465d499d3e917ab4f468dc6dec0cbf9b343cbbab661190f808c92d79bccf5b`.
  Indexes created by an earlier build must be re-ingested with
  `hsum ingest`. Until then, `hsum doctor` reports a generation
  pipeline-fingerprint failure; ordinary search still works against such an
  index, since the failure surfaces in `doctor`, not at open time.

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
