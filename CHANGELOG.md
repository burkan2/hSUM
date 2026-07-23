# Changelog

All notable public changes are documented here. hSUM follows semantic versioning
for its release tags. During alpha, only the current tagged release is supported.

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
