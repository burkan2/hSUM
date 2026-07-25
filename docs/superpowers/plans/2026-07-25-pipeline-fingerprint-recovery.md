# Pipeline Fingerprint Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `hsum ingest` able to rebuild an index whose stored pipeline fingerprint no longer matches the binary, so a pipeline change stops being an unrecoverable dead end.

**Architecture:** Three layers, each independently shippable. First, give `PipelineFingerprintMismatch` its own error subcode and honest remediation text — today it masquerades as `SchemaChecksum` and points users at a command that fails identically. Second, thread a `FingerprintPolicy` through the open and inspection paths, defaulting every existing caller to today's reject behavior. Third, let ingest alone open under `FingerprintPolicy::Tolerate`, and have its commit transaction delete pre-change document versions and rewrite the stored fingerprint atomically.

**Tech Stack:** Rust 1.91 (pinned), `rusqlite` 0.39 with bundled SQLite in WAL mode, `tempfile` for test fixtures. Integration tests in `tests/*.rs` use `hsum::` public paths; some drive the built CLI binary directly.

## Global Constraints

- Rust toolchain is pinned to `1.91.0` in `rust-toolchain.toml`. Do not change it.
- All dependencies are exact-pinned (`=x.y.z`) in `Cargo.toml`. Do not add, remove, or bump any dependency.
- `cargo xtask check` must pass before every commit. It runs `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-targets --all-features`, and `cargo test --doc --all-features`. Run `cargo fmt --all` before committing.
- Clippy warnings are errors. Never silence clippy with `#[allow(...)]`; if clippy objects, stop and report it.
- Do not change `pipeline_fingerprint()`, `PIPELINE_DESCRIPTOR`, `chunker_fingerprint()`, or any frozen digest value. This plan changes recovery behavior, not ingest semantics.
- The fingerprint policy relaxes **only** the pipeline-fingerprint comparison. Application ID, schema version, schema checksum, migration chain, WAL mode, integrity, foreign keys, and file-permission checks stay mandatory in every mode.
- Alpha.1 exposes no `migrate`, `repair`, `prune`, or `forget` command. Do not add one.
- Commit messages use conventional format: `<type>: <description>` (feat, fix, refactor, docs, test, chore, perf, ci). Do NOT add any `Co-Authored-By` line or AI attribution footer.

## Background the implementer needs

The failure this plan fixes was established by running the real binary against a real stale index, not by reading code. Every command fails:

| Command | Result on a stale index |
|---|---|
| `hsum search` | fails |
| `hsum status` | fails |
| `hsum context` | fails |
| `hsum doctor` | fails |
| `hsum ingest` | fails — so the documented remedy does not work |

**The mechanism.** `IndexDb::open_existing` calls `inspect_connection(connection, read_only, InspectionDepth::Open)` at `src/store/open.rs:190`. That runs `validate_metadata` (`src/store/doctor.rs:340`), which compares the stored fingerprint and returns `StoreError::PipelineFingerprintMismatch` (`src/store/doctor.rs:362`). Every command opens the index, so every command dies there.

**There are TWO gates, not one.** Besides `open_existing`, the ingest write path calls `Doctor::run` directly at `src/store/generation.rs:284`, `:331`, and `:496`. `Doctor::run` (`src/store/doctor.rs:63`) uses `IndexDb::open_read_only` — which does *not* inspect — and then calls `inspect_connection(..., InspectionDepth::Full)` itself. So a policy applied only to `open_existing` will not be enough; ingest would pass the first gate and fail the second. Task 2 must cover both.

## File structure

| File | Responsibility in this plan |
|---|---|
| `src/domain/error.rs` | New `PipelineFingerprint` subcode + its four-line spec and offline catalog entry |
| `src/runtime.rs` | Map `StoreError::PipelineFingerprintMismatch` to the new subcode; report healing in ingest output |
| `src/store/doctor.rs` | `FingerprintPolicy` enum; thread through `inspect_connection`/`inspect_snapshot`/`validate_metadata`/`validate_generation_invariants`; `Doctor::run_with_policy` |
| `src/store/open.rs` | `open_existing_with_policy`; keep `open_existing` delegating with `Reject` |
| `src/store/generation.rs` | Heal inside the commit transaction: delete pre-change versions, rewrite fingerprint |
| `tests/fingerprint_recovery.rs` | New — the whole recovery story, including a CLI-level regression test |
| `README.md`, `CHANGELOG.md`, `src/store/schema.rs` | Replace the "no recovery" text committed in 76cff4c |

---

### Task 1: Give the fingerprint mismatch its own subcode and honest text

**Files:**
- Modify: `src/domain/error.rs` (add enum variant near `ErrorSubcode::SchemaChecksum` at line 117; add its `ErrorSpec` arm near the `Self::ForgetTombstone` arm at line 438)
- Modify: `src/runtime.rs:2928-2935` (the `StoreError` → `ErrorSubcode` mapping)
- Test: `tests/cli_contract.rs` or the existing error-catalog tests in `src/domain/error.rs` — follow whichever pattern that file already uses for subcode coverage

**Interfaces:**
- Consumes: nothing from other tasks. This task is independently shippable.
- Produces: `ErrorSubcode::PipelineFingerprint`, used by Task 4's assertions. Its stable string is `"PIPELINE_FINGERPRINT"`.

Why first: it is additive, carries no behavior risk, and improves the product even if Tasks 2-3 never land. Today a user is told the *schema checksum* is invalid when the schema is intact, and told to run `doctor`, which fails the same way.

- [ ] **Step 1: Read the existing subcode contract**

Before writing anything, read `src/domain/error.rs`. Four facts you need, all verified against the current file:

- `ErrorSpec` (line 724) is a **private** struct with private fields (`code`, `retryable`, `message`, `cause`, `fix`) and no public accessors. `spec()` (line 303) is private too. A test therefore cannot call `.spec()`, `.problem()`, or `.fix()` — assert through the public `render_offline_help()` instead.
- `render_offline_help()` (line 287) renders `error:`, `category:`, `retryable:`, `problem:`, `cause:`, `fix:`, and `example:` lines. Note `problem:` is rendered from the field named `message`, and `example:` currently reuses `spec.fix`.
- `ErrorSubcode::ALL` (line 143) is declared `[Self; 64]`. Adding a variant means changing that to `[Self; 65]` and adding the variant to the array, or `ErrorSubcode::parse` will not find it and the frozen-subcode tests will fail.
- Two existing tests iterate `ALL`: `every_frozen_subcode_has_self_contained_offline_help` (line 856) and `every_frozen_subcode_has_cli_json_mcp_ready_metadata`. Your variant must satisfy both.

- [ ] **Step 2: Write the failing test**

Add to `src/domain/error.rs`'s `mod tests`:

```rust
    #[test]
    fn pipeline_fingerprint_subcode_names_the_pipeline_not_the_schema() {
        let subcode = ErrorSubcode::PipelineFingerprint;
        assert_eq!(subcode.as_str(), "PIPELINE_FINGERPRINT");
        assert_eq!(ErrorSubcode::parse("PIPELINE_FINGERPRINT"), Some(subcode));

        let rendered = subcode.render_offline_help();
        let problem = rendered
            .lines()
            .find(|line| line.starts_with("problem:"))
            .expect("offline help renders a problem line");
        let fix = rendered
            .lines()
            .find(|line| line.starts_with("fix:"))
            .expect("offline help renders a fix line");

        assert!(
            problem.contains("pipeline"),
            "problem line must name the pipeline: {problem:?}"
        );
        assert!(
            !problem.contains("schema checksum"),
            "problem line must not claim the schema checksum is invalid: {problem:?}"
        );
        assert!(
            fix.contains("hsum ingest"),
            "fix line must name the working remedy: {fix:?}"
        );
    }
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib pipeline_fingerprint_subcode_names -- --nocapture`

Expected: FAIL to compile — `no variant named PipelineFingerprint found for enum ErrorSubcode`.

- [ ] **Step 4: Add the variant and its spec**

Three edits, all required — omitting the third makes `parse` fail and breaks the frozen-subcode tests:

1. Add `PipelineFingerprint,` to the `ErrorSubcode` enum immediately after `SchemaChecksum,` (line 117).
2. Add its `as_str` arm returning `"PIPELINE_FINGERPRINT"` alongside the other arms (the match around line 210-276).
3. Change `pub const ALL: [Self; 64]` (line 143) to `[Self; 65]` and add `Self::PipelineFingerprint,` to the array, positioned to mirror the enum order.

Add the `ErrorSpec` arm:

```rust
            Self::PipelineFingerprint => ErrorSpec::new(
                ErrorCode::SchemaTooOld,
                false,
                "the index was built by a different hSUM indexing pipeline",
                "the indexing rules changed, so stored evidence no longer matches this binary",
                "run hsum ingest to rebuild this index under the current rules; \
                 evidence recorded before the change does not survive the rebuild",
            ),
```

On the `ErrorCode` choice: `SchemaChecksum` uses `ErrorCode::IndexCorrupt` (line 572), but a fingerprint mismatch is not corruption — the index is intact and internally consistent, it was simply built by different rules. `SchemaTooOld` says that honestly. Both map to process exit `5` (`process_exit_code`, line 58-61), so this changes the human-facing category without changing exit behavior. Do not add a new `ErrorCode` variant.

- [ ] **Step 5: Remap the StoreError**

In `src/runtime.rs:2928-2935`, remove `StoreError::PipelineFingerprintMismatch` from the `SchemaChecksum` arm and give it its own:

```rust
        StoreError::PipelineFingerprintMismatch => ErrorSubcode::PipelineFingerprint,
```

Check `src/mcp.rs:1631` — it also lists `SchemaChecksumMismatch` alongside other variants. Read that match and confirm whether `PipelineFingerprintMismatch` appears there too; if it does and the grouping is about error *category* rather than subcode, leave it. State in your report which you found and why you left it or changed it.

- [ ] **Step 6: Run the tests**

Run: `cargo test --lib pipeline_fingerprint_subcode_names -- --nocapture`
Expected: PASS.

Run: `cargo test --lib error`
Expected: PASS, including `every_frozen_subcode_has_self_contained_offline_help` and `every_frozen_subcode_has_cli_json_mcp_ready_metadata`. If either fails, your new variant is missing catalog data those tests require — add it rather than weakening the test.

- [ ] **Step 7: Full check and commit**

Run: `cargo fmt --all && cargo xtask check`
Expected: PASS.

```bash
git add src/domain/error.rs src/runtime.rs
git commit -m "fix: give the pipeline fingerprint mismatch its own error subcode"
```

---

### Task 2: Thread a fingerprint policy through the inspection paths

**Files:**
- Modify: `src/store/doctor.rs` (add `FingerprintPolicy`; thread through `inspect_connection` at line 162, `inspect_snapshot` at line 173, `validate_metadata` at line 340, `validate_generation_invariants` at line 437; add `Doctor::run_with_policy` beside `Doctor::run` at line 63)
- Modify: `src/store/open.rs:181-196` (add `open_existing_with_policy`; keep `open_existing` delegating)
- Test: `tests/fingerprint_recovery.rs` (new file)

**Interfaces:**
- Consumes: `ErrorSubcode::PipelineFingerprint` from Task 1 (for assertions only).
- Produces, used by Task 3:
  - `pub enum FingerprintPolicy { Reject, Tolerate }` (derive `Clone, Copy, Debug, Eq, PartialEq`), exported from `src/store/mod.rs`.
  - `IndexDb::open_existing_with_policy(path: &Path, mode: OpenMode, policy: FingerprintPolicy) -> Result<Self, StoreError>`.
  - `Doctor::run_with_policy(path: &Path, policy: FingerprintPolicy) -> Result<DoctorReport, StoreError>`.

**This task changes no behavior.** Every existing caller keeps today's semantics by passing `Reject`. The tolerate path is added but nothing uses it until Task 3. That is deliberate: it makes this task's diff reviewable as pure plumbing.

- [ ] **Step 1: Write the failing test**

Create `tests/fingerprint_recovery.rs`:

```rust
use hsum::store::{FingerprintPolicy, IndexDb, OpenMode};

mod support;

/// A stale index — one whose stored pipeline fingerprint predates the running
/// binary — must stay unopenable by default, and must open only when a caller
/// explicitly tolerates the mismatch.
#[test]
fn a_stale_fingerprint_is_rejected_by_default_and_tolerated_only_on_request() {
    let fixture = support::stale_fingerprint_index();

    let rejected = IndexDb::open_existing(&fixture.database_path, OpenMode::ReadOnly);
    assert!(
        matches!(
            rejected,
            Err(hsum::store::StoreError::PipelineFingerprintMismatch)
        ),
        "default open must still refuse a stale index"
    );

    let tolerated = IndexDb::open_existing_with_policy(
        &fixture.database_path,
        OpenMode::ReadOnly,
        FingerprintPolicy::Tolerate,
    );
    assert!(
        tolerated.is_ok(),
        "an explicit tolerate policy must open the stale index: {:?}",
        tolerated.err()
    );
}

/// Tolerating a fingerprint mismatch must not tolerate anything else. A
/// corrupt schema checksum stays fatal in both policies.
#[test]
fn tolerating_the_fingerprint_does_not_relax_other_integrity_checks() {
    let fixture = support::stale_fingerprint_index_with_corrupt_schema_checksum();

    let tolerated = IndexDb::open_existing_with_policy(
        &fixture.database_path,
        OpenMode::ReadOnly,
        FingerprintPolicy::Tolerate,
    );
    assert!(
        matches!(
            tolerated,
            Err(hsum::store::StoreError::SchemaChecksumMismatch)
        ),
        "schema checksum must remain fatal under Tolerate, got {tolerated:?}"
    );
}
```

You must write the two `support` helpers in `tests/support/mod.rs`, which already exists. Read it first and follow its conventions — note it provides `private_tempdir() -> io::Result<TempDir>`, which creates a `0700` directory. Use that rather than bare `tempfile::tempdir()`, because the store refuses to open an index whose parent directory is group- or world-readable, and a test that skips it will fail with a confusing permissions error rather than the fingerprint error you are testing for. Each helper builds a real index (create it the way `tests/filesystem_ingest.rs` or `tests/store_doctor.rs` does), then rewrites `index_meta` rows via `rusqlite` to simulate the stale state:

- `stale_fingerprint_index()` — set `index_meta.pipeline_fingerprint` and every `generations.pipeline_fingerprint` to a different 32-byte value (use `[0xbb; 32]`).
- `stale_fingerprint_index_with_corrupt_schema_checksum()` — the same, plus set `index_meta.schema_checksum` to a different 32-byte value (use `[0xcc; 32]`).

Both return a struct exposing `database_path: PathBuf` and holding the `TempDir` alive so the directory is not dropped mid-test.

**Important:** when you open the SQLite file directly to patch rows, do not change its journal mode. Open, `UPDATE`, commit, close. A stray `PRAGMA journal_mode` will change the live schema state and produce a confusing unrelated failure.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test fingerprint_recovery -- --nocapture`

Expected: FAIL to compile — `FingerprintPolicy` and `open_existing_with_policy` do not exist yet.

- [ ] **Step 3: Add the policy enum**

In `src/store/doctor.rs`, near `InspectionDepth` (line 69):

```rust
/// Whether a pipeline-fingerprint mismatch is fatal.
///
/// `Reject` is the default for every read path: an index built by a different
/// indexing pipeline must not be silently searched, because its stored chunks
/// were produced under different rules. `Tolerate` exists solely so `ingest`
/// can open an index in order to rebuild it — see the heal path in
/// `src/store/generation.rs`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FingerprintPolicy {
    Reject,
    Tolerate,
}
```

Export it from `src/store/mod.rs` in the existing `pub use doctor::{...}` list at line 13.

- [ ] **Step 4: Thread the policy through the doctor functions**

Add a `policy: FingerprintPolicy` parameter to `inspect_connection` (line 162) and `inspect_snapshot` (line 173), passing it down to `validate_metadata` and `validate_generation_invariants`.

In `validate_metadata` (line 340), guard only the pipeline-fingerprint comparison:

```rust
    if policy == FingerprintPolicy::Reject {
        expect_metadata(
            connection,
            "pipeline_fingerprint",
            pipeline_fingerprint().as_bytes(),
        )
        .map_err(|error| match error {
            StoreError::InvalidMetadata(_) => StoreError::PipelineFingerprintMismatch,
            other => other,
        })?;
    }
```

Leave the `schema_checksum` check above it and every other check untouched.

In `validate_generation_invariants` (line 437), the query counts generations whose fingerprint differs. Under `Tolerate`, skip that specific check the same way — a stale index by definition has such generations, and Task 3 is what fixes them.

Add beside `Doctor::run` (line 63):

```rust
    pub fn run_with_policy(
        path: &std::path::Path,
        policy: FingerprintPolicy,
    ) -> Result<DoctorReport, StoreError> {
        let database = IndexDb::open_read_only(path)?;
        inspect_connection(database.connection(), true, InspectionDepth::Full, policy)
    }
```

and make the existing `Doctor::run` delegate with `FingerprintPolicy::Reject` so its public behavior is unchanged.

- [ ] **Step 5: Add the open-path variant**

In `src/store/open.rs`, rename the body of `open_existing_with_observer` (line 181) into a policy-taking form and have the existing entry points delegate with `Reject`. The `inspect_connection` call at line 190 passes the policy through:

```rust
    pub fn open_existing_with_policy(
        path: &Path,
        mode: OpenMode,
        policy: FingerprintPolicy,
    ) -> Result<Self, StoreError> {
        Self::open_existing_with_policy_and_observer(path, mode, policy, |_| {})
    }
```

`open_existing(path, mode)` must keep its exact current signature and behavior — ten call sites depend on it (`src/runtime.rs:442,458,539,619`, `src/status.rs:463`, `src/mcp.rs:139,175`, `src/app/init.rs:539`, `src/app/context.rs:181,210`). Do not edit those call sites in this task.

- [ ] **Step 6: Run the tests**

Run: `cargo test --test fingerprint_recovery -- --nocapture`
Expected: PASS both tests.

Run: `cargo test --test store_doctor && cargo test --test mcp_contract`
Expected: PASS. These exercise the paths you just re-plumbed; any failure means a caller changed behavior when it should not have.

- [ ] **Step 7: Full check and commit**

Run: `cargo fmt --all && cargo xtask check`
Expected: PASS.

```bash
git add src/store/doctor.rs src/store/open.rs src/store/mod.rs tests/fingerprint_recovery.rs tests/support
git commit -m "refactor: thread a fingerprint policy through the inspection paths"
```

---

### Task 3: Heal the index during ingest

**Files:**
- Modify: `src/runtime.rs:458` (ingest's `open_existing` → policy-aware open)
- Modify: `src/store/generation.rs` (the three `Doctor::run` calls at lines 284, 331, 496; the commit transaction around lines 728-741)
- Test: `tests/fingerprint_recovery.rs` (extend)

**Interfaces:**
- Consumes: `FingerprintPolicy`, `IndexDb::open_existing_with_policy`, `Doctor::run_with_policy` from Task 2.
- Produces: a healed index — `index_meta.pipeline_fingerprint` and every `generations.pipeline_fingerprint` equal to `pipeline_fingerprint()`, with pre-change document versions removed.

**This is the only task that deletes user data.** Its transactional integrity is the point. A partial delete must never commit.

- [ ] **Step 1: Write the failing test**

Add to `tests/fingerprint_recovery.rs`:

```rust
/// `hsum ingest` must be able to rebuild a stale index, and the result must
/// satisfy the *full* doctor inspection — not merely open.
#[test]
fn ingest_heals_a_stale_index_and_full_doctor_passes_afterward() {
    let fixture = support::stale_fingerprint_index();

    // Precondition: doctor refuses before the heal.
    assert!(
        hsum::store::Doctor::run(&fixture.database_path).is_err(),
        "fixture must start out failing doctor"
    );

    support::run_ingest(&fixture).expect("ingest must heal a stale index");

    let report = hsum::store::Doctor::run(&fixture.database_path)
        .expect("full doctor must pass after the heal");
    assert_eq!(
        report.pipeline_fingerprint,
        hsum::store::pipeline_fingerprint(),
        "healed index must carry the current fingerprint"
    );
}

/// The heal is all-or-nothing. If the rebuild fails, the index must be exactly
/// as it was — no half-deleted versions, no rewritten fingerprint.
#[test]
fn a_failed_heal_leaves_the_stale_index_untouched() {
    let fixture = support::stale_fingerprint_index();
    let before = support::fingerprint_row_bytes(&fixture.database_path);

    support::run_ingest_forcing_commit_failure(&fixture)
        .expect_err("this fixture must fail during ingest");

    let after = support::fingerprint_row_bytes(&fixture.database_path);
    assert_eq!(
        before, after,
        "a failed heal must not rewrite the stored fingerprint"
    );
    assert!(
        hsum::store::Doctor::run(&fixture.database_path).is_err(),
        "a failed heal must leave the index in its original stale state"
    );
}
```

Write `support::run_ingest`, `support::run_ingest_forcing_commit_failure`, and `support::fingerprint_row_bytes`. For the failure injection, prefer a mechanism the codebase already supports — read how `tests/multiprocess.rs` and `tests/ingest_generations.rs` induce failures (for example a writer-lock conflict, or a source that fails mid-read) rather than inventing a new fault-injection hook. If no existing mechanism fits, make the ingest fail by pointing the source root at a directory whose contents are removed between the estimate and commit passes, and say so in your report.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test fingerprint_recovery ingest_heals -- --nocapture`

Expected: FAIL — ingest currently returns `PipelineFingerprintMismatch` before doing any work.

- [ ] **Step 3: Open ingest under the tolerate policy**

In `src/runtime.rs` at line 458, change ingest's open:

```rust
    let mut database = IndexDb::open_existing_with_policy(
        &context.database_path,
        OpenMode::ReadWrite,
        FingerprintPolicy::Tolerate,
    )
    .map_err(map_store_error)?;
```

Change **only** this one call site. The other nine `open_existing` callers stay on the default reject path — that is what keeps read commands failing closed.

- [ ] **Step 4: Pass the policy at the second gate**

The ingest write path calls `Doctor::run` at `src/store/generation.rs:284`, `:331`, and `:496`. These are a second gate: `Doctor::run` performs its own `inspect_connection` at Full depth, so Step 3 alone leaves ingest still failing.

Change those three call sites to `Doctor::run_with_policy(self.path(), FingerprintPolicy::Tolerate)`.

Read each site before editing. If any of the three serves a path other than ingest, do NOT relax it — report which and why instead. Relaxing a gate that a non-ingest command reaches would defeat Task 2's whole design.

- [ ] **Step 5: Delete pre-change versions and rewrite the fingerprint in the commit transaction**

In `src/store/generation.rs`, inside the commit transaction alongside the existing `set_metadata` calls (lines 728-741), add the heal. It must run in the same transaction so it commits atomically with the generation.

Delete rows in dependency order. `document_versions`, `document_heads`, `chunks`, and `content_blobs` carry `ON DELETE RESTRICT` (`migrations/0001_alpha1.sql:101-131`), so a wrong order aborts the transaction on a foreign-key violation:

1. `document_heads` rows pointing at pre-change versions
2. `generation_changes` and `active_passages` rows referencing them
3. `passage_literals` for those passages
4. `document_versions` rows
5. orphaned `chunks`, then orphaned `content_blobs`, then orphaned `chunk_layouts`

Read `migrations/0001_alpha1.sql` in full before writing the deletes — it is the authority on the foreign-key graph, and the list above is a guide, not a substitute for reading it.

Then rewrite the fingerprint:

```rust
        set_metadata(
            &transaction,
            "pipeline_fingerprint",
            pipeline_fingerprint().as_bytes(),
        )?;
```

Guard the whole heal so it runs only when the stored fingerprint actually differs — a normal ingest on a healthy index must do no deletion and write no fingerprint. Read the stored value with the existing `metadata_value` helper and compare before acting.

Also update `generations.pipeline_fingerprint` for any retained rows, or delete pre-change generations outright. `validate_generation_invariants` (`src/store/doctor.rs:437`) counts generations whose fingerprint differs, so leaving stale rows means the healed index fails Full doctor — which is exactly what Step 1's first test asserts against.

- [ ] **Step 6: Run the tests**

Run: `cargo test --test fingerprint_recovery -- --nocapture`
Expected: PASS all four tests.

Run: `cargo test --test ingest_generations && cargo test --test store_doctor`
Expected: PASS. A normal ingest must be completely unaffected — if any of these fail, the heal is running when it should not.

- [ ] **Step 7: Verify against the real binary**

The store-layer tests are not sufficient. Every prior reviewer of this codebase concluded by code-reading that search survived a fingerprint mismatch; one execution disproved it. Prove the fix end-to-end:

```bash
cargo build --locked --release
cd "$(mktemp -d)" && git init -q . && printf 'fn hello() {}\n' > lib.rs
export HSUM_HOME="$PWD/home" && mkdir -p home
/absolute/path/to/checkout/target/release/hsum init
```

Then patch the stored fingerprint to `bb`×32 with `python3` + `sqlite3` (the database path is printed by `hsum context`), and confirm:

- `hsum search 'hello'` fails with the `PIPELINE_FINGERPRINT` subcode — **not** `SCHEMA_CHECKSUM`
- `hsum ingest` succeeds
- `hsum doctor` passes afterward
- `hsum search 'hello'` returns the passage

Paste the real terminal output into your report. If any step behaves differently, stop and report rather than adjusting the test to match.

- [ ] **Step 8: Full check and commit**

Run: `cargo fmt --all && cargo xtask check`
Expected: PASS.

```bash
git add src/runtime.rs src/store/generation.rs tests/fingerprint_recovery.rs tests/support
git commit -m "feat: rebuild a stale index during ingest instead of failing closed

An index whose stored pipeline fingerprint predates the running binary
previously could not be opened by any command, including the ingest that
was supposed to repair it. Ingest now opens under a tolerate policy and,
inside its commit transaction, drops document versions recorded under the
old pipeline and rewrites the stored fingerprint. Read commands still fail
closed until the rebuild happens."
```

---

### Task 4: Report the heal and correct the documentation

**Files:**
- Modify: `src/runtime.rs` (ingest human output)
- Modify: `CHANGELOG.md`, `README.md`, `src/store/schema.rs` — all three currently say recovery is impossible (committed in `76cff4c`)
- Test: `tests/fingerprint_recovery.rs` (extend) or `tests/cli_contract.rs`

**Interfaces:**
- Consumes: the heal behavior from Task 3.
- Produces: nothing.

Deleting user evidence silently would be the worst outcome of this whole change. The user must be told.

- [ ] **Step 1: Write the failing test**

Add to `tests/fingerprint_recovery.rs`:

```rust
/// A heal deletes evidence. Ingest must say so rather than doing it quietly.
#[test]
fn ingest_output_reports_the_rebuild_and_its_cost() {
    let fixture = support::stale_fingerprint_index();
    let output = support::run_ingest_capturing_output(&fixture)
        .expect("ingest must heal a stale index");

    assert!(
        output.contains("pipeline"),
        "ingest must say the pipeline changed: {output:?}"
    );
    assert!(
        output.contains("rebuilt") || output.contains("rebuild"),
        "ingest must say the index was rebuilt: {output:?}"
    );
    assert!(
        output.contains("citation"),
        "ingest must warn that prior citations no longer resolve: {output:?}"
    );
}
```

Write `support::run_ingest_capturing_output`. Follow how `tests/runtime_process.rs` captures CLI output — do not invent a new capture mechanism.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test fingerprint_recovery ingest_output_reports -- --nocapture`
Expected: FAIL — ingest reports nothing about the heal.

- [ ] **Step 3: Report the heal**

In the ingest rendering path in `src/runtime.rs`, when a heal occurred, emit a line before the normal outcome. Match the file's existing plain declarative voice (see the init rendering around `src/runtime.rs:225-330` for the house style):

```text
The indexing pipeline changed, so this index was rebuilt under the current
rules. Evidence recorded before the rebuild is gone, and citations issued
against it no longer resolve.
```

You will need to plumb a "healed" flag out of the ingest outcome to know when to print it. Follow how existing outcome counts (`carried_forward_documents`, `tombstoned_documents`) are threaded from `src/store/generation.rs` to the renderer, and add the flag the same way.

- [ ] **Step 4: Run the test**

Run: `cargo test --test fingerprint_recovery -- --nocapture`
Expected: PASS all five tests.

- [ ] **Step 5: Correct CHANGELOG.md**

The `## Unreleased` → `### Changed` entry currently says there is no in-product recovery and the user must delete the index. Replace that with the truth after this plan: every command still fails closed on a stale index, but `hsum ingest` now rebuilds it, and the rebuild drops evidence recorded before the change so prior citations stop resolving. Keep the two fingerprint values and the existing prose voice, wrapped at roughly 80 columns.

- [ ] **Step 6: Correct README.md**

The "When the indexing pipeline changes" section added in `76cff4c` (just above "A refresh runs as two bounded passes") says ingest cannot recover and the user must delete the index directory. Rewrite it: `hsum ingest` is the recovery path; the rebuild drops pre-change evidence; citations issued before it no longer resolve. Match the surrounding voice and line wrapping.

- [ ] **Step 7: Correct the schema.rs comment**

The comment above `alpha_1_pipeline_fingerprint_is_frozen` in `src/store/schema.rs` says ingest cannot repair a stale index. Correct it. Do not touch the assertion or the frozen digest value.

- [ ] **Step 8: Full check and commit**

Run: `cargo fmt --all && cargo xtask check`
Expected: PASS.

```bash
git add src/runtime.rs src/store/generation.rs src/store/schema.rs README.md CHANGELOG.md tests/fingerprint_recovery.rs tests/support
git commit -m "docs: record that ingest now rebuilds a stale index"
```

---

## Out of scope

Do not implement these here:

1. A `migrate`, `repair`, or `prune` command. Alpha.1 states it has none.
2. Auto-healing at open time. Explicitly rejected in the spec: it would let a `search` silently "heal" an index whose stored chunks still reflect the old rules, turning a loud failure into quiet wrong answers.
3. Preserving pre-change citations across a heal. The approved decision is a single clean chunking regime.
4. Any change to `PIPELINE_DESCRIPTOR`, the extension set, or the frozen digests.
