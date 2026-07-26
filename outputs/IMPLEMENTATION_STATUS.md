# hSUM `0.1.0-alpha.2` release implementation status

**Snapshot:** 2026-07-26
**Scope:** Published public prerelease from protected `main` at `ef54758`

This matrix maps the published alpha.2 surface to implementation, test, and
release evidence. “Implemented” means a code path and focused tests exist in
this checkout; the sections below separately record clean-runner, signed-tag,
artifact, and production-documentation evidence.

| Surface | Checkout status | Primary implementation | Focused evidence |
|---|---|---|---|
| CLI grammar and stable exits | Implemented | `src/cli.rs`, `src/runtime.rs` | `tests/cli_contract.rs`, `tests/runtime_process.rs` |
| Safe init and idempotent root identity | Implemented | `src/app/init.rs` | `tests/init_smoke.rs` |
| Pipeline-fingerprint recovery | Implemented | `src/app/init.rs`, `src/store/doctor.rs`, `src/store/open.rs` | `tests/init_smoke.rs`, `tests/runtime_process.rs`, subprocess crash tests, released-alpha.1 upgrade smoke |
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
| JSONL, vectors/models, watcher, HTTP, backup/prune/forget/restore | Not implemented | Intentionally absent from the alpha command surface | Rejected-surface assertions in `tests/cli_contract.rs` |
| GitHub release archives | Alpha.2 published | `.github/workflows/release.yml`, `scripts/release-smoke.sh` | Checksums, SPDX SBOM attestations, signed-tag guard, Linux/macOS release jobs |
| crates.io package or installer | Not available | `Cargo.toml` sets `publish = false`; archive-only alpha policy | Explicitly deferred; no public install claim |

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

## Published alpha.2 evidence

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
