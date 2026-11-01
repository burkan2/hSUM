# Stable v0.1 release execution plan — 2026-10-31

Status: Linux latency optimization in progress; stable publication blocked. Starting commit
`83b4d4c`. Preserve the existing `benches/agent_ab/benchmark.py` isolation fix,
original Mac crash snapshot, recovered index, and all previous raw evidence.

## Sequence and acceptance criteria

1. **Connection lifetime repair.** Add regressions before changing orchestration.
   Reuse one validated connection through preflight and retrieval within a
   request, without holding a read transaction during inference. Preserve fresh
   snapshots on retries, namespace/replacement refusal, cancellation, bounded
   deadlines, and WAL reader/writer locking. Measure before deciding whether
   cross-request pooling is necessary; do not add a benchmark-only keeper.
2. **Lexical ranking.** Profile broad FTS ranking on the retained fixture in a
   disposable diagnostic copy. Preserve the exact BM25 order, project scope,
   deterministic ties, 500-candidate bound, guarded materialization, and frozen
   query manifest. Verify equivalent results and bounded work with focused
   regressions. Reduced diagnostics are not qualification passes.
3. **Candidate review and local gates.** Review the cursor/descriptor repairs and
   new changes independently; resolve findings. Run `cargo xtask check`, release
   smoke, deterministic retrieval, cancellation/recovery, and appropriate
   packaging checks. Record exact candidate and binary/source hashes.
4. **Same-candidate native qualification.** Fresh preparation and inference on
   Mac M2/16 GB and Linux x86_64, then the unchanged 3 × 750-observation protocol.
   Every run: identifier and ordinary lexical p95 <100 ms, fallback <750 ms,
   hybrid <500 ms; three-run total-p95 population CV <=10%. Review all raw
   records, cold measurements, RSS, storage, and ingest throughput. Retain
   failures and do not resample to select a preferred result. Recheck native
   stress, portability, and contributor gates when affected by the candidate.
5. **Stable distribution.** Prepare a stable-capable draft-first release
   workflow, versioned documentation, changelog, reproducible archives, checksums,
   SBOMs/attestations and required signatures. Run candidate clean-machine and
   supported-client checks; reconcile the complete stable security/recovery
   checklist. Existing alpha publication is not a stable release path.
6. **Publish and verify.** Only after all technical and authority gates pass,
   create the matching signed stable tag, verify draft assets before publication,
   and verify downloaded native assets plus stable-tag clean-machine trials.
   Update release evidence and claims to match measured behavior. Lexical stays
   the default; hybrid remains beta under the existing evaluation disposition.

## Current constraints

- User confirmed on October 31 that Apple Developer ID/notarization and
  protected artifact-signing authority are **still outstanding**. GitHub lists
  no Actions secrets. This blocks stable publication, not implementation.
- `.github/workflows/release.yml` explicitly accepts alpha tags only.
- November 1: the user freed disk space; approximately 12 GiB is now available.
  Original crash evidence and recovered index remain intact. Recheck capacity
  before fresh native qualification; compact development builds remain enabled.
- Mac cold-cache eviction requires local operator authentication. Prepare the
  exact launcher and evidence paths before requesting it; never collect a
  password in chat or waive the cold-cache gate.
- No current candidate passes cross-platform performance qualification. Prior
  checkpoint details remain in `RETRIEVAL_QUALIFICATION_2026-10-23.md`.

## Execution log

- October 31: reconciled checkout, open PR #9, original evidence and release
  runbook; hSUM index inspected (generation 3, indexed October 21). Current
  source reads are authoritative for edits. GitHub authentication is available.
- November 1: implemented request-local connection reuse with retry, concurrent
  scope-change, namespace, and cancellation regressions. Added explicit zero
  busy-timeout WAL checkpoint after successful CLI re-embedding; live readers
  defer cleanup, and cleanup failure cannot disguise the committed generation.
- November 1: implemented an equivalent unit-weight BM25 auxiliary using
  phrase-local iteration and a lazy, bounded, snapshot-local docsize cache.
  Scope-aware frontier ranking avoids metadata joins for worse global scores
  when enough eligible candidates survive; otherwise it includes the remainder
  before applying the stable limit. Original body guards and 500-candidate
  cursor remain. The score spool still scales with all matches.
- November 1: independent review found and resolved global-cutoff, malformed
  size, eager-cache, cancellation, and repeated-registration concerns. The
  production implementation passes separate native/custom score-bit tests,
  scoped-order tests, corruption tests, and real interruption/recovery tests.
  This is independent AI review, not an external security audit.
- November 1: full `cargo xtask check` passed with the final registration fix
  (formatting, clippy, Rust integration/unit/doc tests, frozen input checks and
  Python harness tests). Mac release search tests (21) and CLI/MCP smoke also
  pass. Reduced all-25-query product A/B/A preserves results/explanations and
  reduces the broad lexical query median from 182 to 88 ms on the checkpointed
  disposable fixture. Cross-platform qualification is still outstanding.

- November 1: frozen `41af5c7` completes fresh setup and all 2,250 queries on
  both platforms. Mac passes every gate (ordinary p95 84/85/84 ms; total-p95
  CV 0.269%); Linux ordinary p95 104/104/104 ms fails <100 ms, with all other
  gates passing. Raw results and provenance independently audited. Eight
  workflows pass in total, including Linux million-passage stress. See the
  qualification report for retained raw evidence and fresh throughput.
- November 1: joined per-row docsize replaces the whole-index token-length
  cache, preserving native score bits, corruption checks and cancellation.
  Focused debug/release tests and independent review pass. Reduced Mac
  all-25-query A/B/A preserves every result/explanation; broad-query median
  is 85 -> 84 -> 85 ms. This small product gain is not a qualification pass.
  A fresh lexical-only Linux diagnostic is required before a new full run.
- November 1: stable signature verifier and installer authentication are
  implemented locally with real disposable Minisign tests and independent
  review. Production key authority, Apple signing and workflow integration
  remain open; no stable tag or release has been published.

- November 1: initial Linux diagnostic 41304545751 is invalid: both builds
  shared a target directory and all three phase hashes are identical. Independent
  review caught the reuse; retain the raw run, isolate target directories and
  reject equal binary hashes before measurement. The new refusal regression
  fails before the repair and passes after it. No Linux optimization conclusion
  can be drawn from this run; corrected diagnostic remains required.

- November 1: corrected diagnostic 41319532732 uses distinct isolated binaries
  and retains all 684 result digests. Broad-query median 81 ->79 ->80ms shows
  only a small within-run benefit; cross-run runner-speed differences are not
  an optimization claim. Stable signature/installer CI passes on Ubuntu's
  Minisign 0.11 in run 41319536850 (local tests use 0.12).
- November 1: a restricted one-byte phrase-position counter is under final
  review. It uses private SQLite iterator bytes only after exact bundled-source
  hash/runtime-source checks, with bounded offset arithmetic, cancellation and
  native fallback for every unsupported case. A SQL-only prototype improves
  ~74ms to~59ms with identical result IDs and score bits.28 release search tests
  pass. Rust AddressSanitizer passes all12 focused counter/ranker tests; the
  earlier combined Rust/Apple-Clang ASan attempt failed to link due to runtime
  incompatibility and is retained as a toolchain failure, not a memory result.
  Full product timing, disabled-build validation and native qualification remain.

- November 1: full contributor gate, release smoke and reviewed sanitizer
  script pass. The script checks both the enabled 12-test path and disabled
  11-test path on macOS. Product A/B/A preserves all 900 complete result digests;
  broad-query medians 88 ->73 ->96ms improve, but high tail outliers and active
  macOS storage-management processes make this diagnostic only. Freeze for
  Linux diagnostic/combined Rust+C sanitizer CI next; full qualification waits.
