# hSUM `0.1.0-alpha.1` implementation status

**Snapshot:** 2026-07-20
**Scope:** Local source checkout; no public release or distribution claim

This matrix maps the alpha.1 product surface to implementation and test
evidence in the repository. “Implemented” means a code path and focused tests
exist in this checkout. It does not mean release CI, all-target portability,
signing, packaging, performance, or clean-machine gates have run.

| Surface | Checkout status | Primary implementation | Focused evidence |
|---|---|---|---|
| CLI grammar and stable exits | Implemented | `src/cli.rs`, `src/runtime.rs` | `tests/cli_contract.rs`, `tests/runtime_process.rs` |
| Safe init and idempotent root identity | Implemented | `src/app/init.rs` | `tests/init_smoke.rs` |
| Trust registry, pointer hint, selection precedence | Implemented | `src/config/`, `src/app/context.rs` | `tests/trust_registry.rs`, `tests/context_resolution.rs`, `tests/config_paths.rs` |
| Bounded filesystem discovery | Implemented for Unix alpha target | `src/ingest/filesystem.rs` | `tests/filesystem_ingest.rs` |
| Deterministic chunking and literal extraction | Implemented | `src/ingest/chunk.rs`, `src/ingest/literals.rs`, `src/ingest/quote_bloom.rs` | `tests/chunking.rs`, `tests/literals.rs`, `tests/quote_bloom.rs` |
| Atomic generations and deletion guards | Implemented | `src/store/generation.rs`, `src/store/lock.rs` | `tests/ingest_generations.rs`, `src/app/tests.rs`, `tests/multiprocess.rs` |
| Immutable SQLite evidence store | Implemented | `src/store/open.rs`, `src/store/schema.rs`, `migrations/0001_alpha1.sql` | `tests/store_foundation.rs` |
| Exact/quoted/BM25 retrieval and deterministic fusion | Implemented | `src/search/query.rs`, `src/search/retrieval.rs` | `tests/query_contract.rs`, `tests/search_contract.rs` |
| Canonical citation and historical `get` | Implemented | `src/domain/citation.rs`, `src/search/get.rs` | `tests/get_contract.rs` |
| Bounded source-drift observation | Implemented for Unix alpha target | `src/status.rs` | `tests/source_drift.rs`, `tests/runtime_process.rs` |
| Status and actionable storage/source problems | Implemented | `src/status.rs`, `src/store/capacity.rs` | `tests/store_foundation.rs`, `tests/runtime_process.rs` |
| Read-only full doctor | Implemented | `src/store/doctor.rs` | `tests/store_doctor.rs` |
| Capacity, quota, locality, and durability preflight | Implemented | `src/store/capacity.rs`, `src/store/open.rs` | module tests plus init/ingest process tests |
| Four-tool, project-bound MCP stdio | Implemented | `src/mcp.rs` | `tests/mcp_contract.rs`, `tests/runtime_process.rs` |
| Generated client snippets and local client probe | Implemented | `src/runtime.rs` | `tests/runtime_process.rs` |
| Shared public error taxonomy | Implemented | `src/domain/error.rs`, transport mappings in `src/runtime.rs` and `src/mcp.rs` | domain, CLI, MCP, and process contract tests |
| Completion and man generation | Implemented | `src/cli.rs` | `tests/cli_contract.rs` |
| JSONL, vectors/models, watcher, HTTP, backup/prune/forget/restore | Not implemented | Intentionally absent from alpha.1 command surface | Rejected-surface assertions in `tests/cli_contract.rs` |
| Published crate/binaries/installer | Not available | `Cargo.toml` sets `publish = false`; no release workflow/artifacts | Must be established and independently verified before publication |

## Current invariants

- Runtime ingest and retrieval are local; the binary has no network transport.
  MCP uses the process's stdin and stdout.
- One alpha index is bound to exactly one project and one filesystem source.
- MCP opens the selected database read-only and query-only and exposes no
  mutation tool.
- Indexed content is untrusted and is marked as such in CLI JSON and MCP
  evidence.
- Generations activate atomically; an all-failed scan does not advance the
  active generation or epoch.
- Canonical citations bind index, source, document, revision hash, and byte
  span.
- Storage preflight reserves the greater of 64 MiB or 10% of managed index
  bytes in addition to the estimated write.

## Evidence not yet claimed

- No signed tag, crate publication, prebuilt binary, installer, SBOM, or
  notarized artifact exists.
- No public remote, release custody, security-reporting address, or published
  versioned documentation site is established.
- No clean-machine macOS arm64/Linux x86_64 release matrix is recorded here.
- No benchmark, retrieval-evaluation, time-to-first-evidence, external
  dogfood, or cross-client-version result is claimed.
- The brand asset provenance and software license remain release blockers.

Run `cargo xtask check` for the checkout-wide contributor gate. Record the
actual result separately; this status file intentionally does not turn the
presence of tests into a claim that a final release gate passed.

## Local evidence actually run on 2026-07-20

These are single-machine developer results on this checkout, not release,
platform, or clean-machine evidence:

- `cargo xtask check` passed: formatting, Clippy with warnings denied, all
  targets/features tests, and doctests.
- `cargo build --release --locked --offline` produced a working binary with no
  network access from an already populated Cargo cache.
- The MCP contract suite passed 21/21, including the reworked wire-cap
  pagination and escape-heavy fixtures that respect the canonical 1,800-byte
  chunk maximum; the retired oversized-chunk fixtures are intentionally
  rejected by the store as corrupt.
- One shared-fixture CLI↔MCP parity process test compares citation order,
  score and rank values, spans, requested/effective modes, source state, API
  version, and the public error taxonomy across a real CLI process and a real
  `hsum mcp` stdio subprocess (`tests/runtime_process.rs`).
- Offline error help (`hsum help error <SUBCODE>`) is process-tested: it works
  with no index or network, and real failures' `learn:` lines name both the
  offline command and the version-pinned docs URL.
- A fresh-temporary-repository smoke run covered dry-run init, init with a
  verified suggested query, JSON search, immutable `get` with
  `--verify-source-hash` returning `unchanged`, context, status, problems,
  read-only doctor, client config (privacy warning on stderr, copyable config
  on stdout), the real empty-directory client doctor probe, completion and man
  generation, and an MCP stdio session exposing exactly the four read-only
  tools with clean stdout framing.

On 2026-07-21, `hsum --version --verbose` gained its contracted detail report
(API version, readable/writable schema range, build-target triple, and the
tag-gated capability list) via a build-script target export; `cargo xtask
check` and the locked offline release build were re-run green with that
change, and both version forms were verified against the release binary.

None of this constitutes a public release; the P0 items in `TODOS.md` remain
open.
