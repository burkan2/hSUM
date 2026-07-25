# Pipeline Fingerprint Recovery Implementation Plan

> # ⛔ SUPERSEDED — DO NOT EXECUTE
>
> **Tasks 1 and 2 landed and are kept** (`2897794`, `9ca396d`): the
> `PIPELINE_FINGERPRINT` error subcode and the `FingerprintPolicy` plumbing.
>
> **Tasks 3, 3b, and 4 are wrong and must not be implemented.** Ingest
> traverses four fingerprint gates, not the three this plan names; the fourth
> (`src/store/generation.rs:331`) is shared with `ingest --dry-run`, making
> this plan's "dry-run stays strict" requirement inexpressible. Relaxing every
> gate was verified not to recover the index. And Task 3b's generation
> deletion breaks six `validate_generation_invariants` checks rather than the
> one it accounts for.
>
> Recovery moved to `hsum init --rebuild`. See
> `docs/superpowers/specs/2026-07-26-init-rebuild-design.md`.

**Goal:** Make `hsum ingest` able to rebuild an index whose stored pipeline fingerprint no longer matches the binary, so a pipeline change stops being an unrecoverable dead end.

**Architecture:** Four layers, each independently shippable. First, give `PipelineFingerprintMismatch` its own error subcode and honest remediation text — today it masquerades as `SchemaChecksum` and points users at a command that fails identically. Second, thread a `FingerprintPolicy` through the open and inspection paths, defaulting every existing caller to today's reject behavior. Third, carry that policy on `ContextRequest` so ingest can resolve a context for a stale index at all — every command resolves its context before running, so this gate is upstream of everything else. Fourth, have ingest's commit transaction drop pre-change document versions and rewrite the stored fingerprint atomically.

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

**There are THREE gates, and the most important one is easy to miss.** This was established by instrumenting the real binary, after a first implementation attempt relaxed two gates and observed no change in behavior at all:

| Order | Gate | Location | Who reaches it |
|---|---|---|---|
| 0 | `materialize_context` | `src/app/context.rs:210` | **every** CLI command, before any command-specific logic |
| 1 | `open_existing(ReadWrite)` | `src/runtime.rs:458` | ingest only |
| 2 | `Doctor::run` | `src/store/generation.rs:331`, `:496` | ingest only |

Gate 0 dominates: `run_ingest`'s first statement is `direct_context(...)`, so ingest dies there before reaching its own code. `Doctor::run` (`src/store/doctor.rs:63`) is genuinely independent of `open_existing` — it uses `IndexDb::open_read_only`, which does not inspect, then calls `inspect_connection(..., InspectionDepth::Full)` itself.

Two nearby sites must STAY strict: `src/store/generation.rs:284` (`configure_filesystem_scope`, reached only from `hsum init --no-ingest`) and `src/app/context.rs:181` (`resolve_trust_target`, serving `hsum trust`).

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

### Task 3: Let ingest reach its own code (gate 0)

> **Revised 2026-07-26 after a failed first attempt.** The original Task 3
> bundled gate plumbing with the heal and named only two gates. A real-binary
> run proved there are **three**, and that the missing one sits upstream of
> both: `run_ingest`'s first statement is `direct_context(...)`, which opens the
> index through `materialize_context` (`src/app/context.rs:210`) before ingest
> reaches any of its own code. Relaxing gates 1 and 2 alone changes nothing
> observable — that was measured, not assumed.

**The complete, verified gate map for `hsum ingest`:**

| Order | Gate | Location | Who reaches it |
|---|---|---|---|
| 0 | `materialize_context` | `src/app/context.rs:210` | **every** CLI command |
| 1 | `open_existing(ReadWrite)` | `src/runtime.rs:458` | ingest only |
| 2 | `Doctor::run` | `src/store/generation.rs:331`, `:496` | ingest only |

Two sites must STAY on `Reject` — do not touch them:

- `src/store/generation.rs:284` (`configure_filesystem_scope`) — reached only from `hsum init --no-ingest` (`src/app/init.rs:319`), which is not ingest.
- `src/app/context.rs:181` (`resolve_trust_target`) — serves `hsum trust`, not ingest.

**Files:**
- Modify: `src/app/context.rs` — `ContextRequest` struct (line 30), its `direct` constructor (line 40), `resolve_context` (line 72), `materialize_context` (line 200) and its open (line 210)
- Modify: `src/runtime.rs` — `direct_context` (line 1604) and ingest's open (line 458)
- Modify: `src/store/generation.rs` — the `Doctor::run` calls at lines 331 and 496 ONLY
- Test: `tests/fingerprint_recovery.rs` (extend)

**Interfaces:**
- Consumes: `FingerprintPolicy`, `IndexDb::open_existing_with_policy`, `Doctor::run_with_policy` from Task 2.
- Produces, used by Task 3b: `ContextRequest` gains a `pub fingerprint_policy: FingerprintPolicy` field. `ContextRequest::direct(...)` sets it to `Reject`, so all twelve non-ingest handlers are unchanged by construction. Only `run_ingest` sets `Tolerate`, and only when `!arguments.dry_run`.

**This task heals nothing.** Afterward, `hsum ingest` against a stale index gets past all three gates and proceeds into ingest logic; the stored fingerprint is still stale. Task 3b makes it repair. Gate plumbing and data deletion are separately reviewable, which is why they are separate tasks.

- [ ] **Step 1: Write the failing test**

Add to `tests/fingerprint_recovery.rs`:

```rust
/// Gate 0 lives in `materialize_context`, which every CLI command traverses
/// before its own logic. A tolerant request must resolve a stale index; the
/// default request must still refuse one.
#[test]
fn only_a_tolerant_request_resolves_a_context_for_a_stale_index() {
    let fixture = stale_fingerprint_index();

    let strict = ContextRequest::direct(
        fixture.repo_root.clone(),
        fixture.managed_paths.clone(),
    );
    assert!(
        resolve_context(&strict).is_err(),
        "the default context request must still refuse a stale index"
    );

    let mut tolerant = ContextRequest::direct(
        fixture.repo_root.clone(),
        fixture.managed_paths.clone(),
    );
    tolerant.fingerprint_policy = FingerprintPolicy::Tolerate;
    assert!(
        resolve_context(&tolerant).is_ok(),
        "an explicitly tolerant context request must resolve a stale index: {:?}",
        resolve_context(&tolerant).err()
    );
}
```

Read `src/app/context.rs` and `src/app/mod.rs` first and use the real public paths for `ContextRequest`, `resolve_context`, and `ContextError`. If `resolve_context` is not publicly reachable from an integration test, say so in your report and assert through the CLI instead rather than making private items public.

Your fixture needs `repo_root` and `managed_paths` alongside `database_path`. Extend the existing stale-index fixture from Task 2 rather than writing a second one. Note the fixture must create the index inside a real repo directory so `ContextRequest::direct` can resolve it by root.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test fingerprint_recovery only_a_tolerant_request -- --nocapture`

Expected: FAIL to compile — `ContextRequest` has no `fingerprint_policy` field.

- [ ] **Step 3: Add the field and thread it to the open**

In `src/app/context.rs`, add to `ContextRequest` (line 30):

```rust
    pub fingerprint_policy: FingerprintPolicy,
```

Set it to `FingerprintPolicy::Reject` in `ContextRequest::direct` (line 40). That single default is what keeps all twelve non-ingest handlers unchanged.

Thread `request.fingerprint_policy` into the open at `src/app/context.rs:210`, replacing `IndexDb::open_existing(&database_path, OpenMode::ReadOnly)` with `IndexDb::open_existing_with_policy(&database_path, OpenMode::ReadOnly, request.fingerprint_policy)`.

Leave the open at `src/app/context.rs:181` (`resolve_trust_target`) alone — it serves `hsum trust`.

If other constructors of `ContextRequest` exist besides `direct`, give each `Reject` unless it is on the ingest path. List every constructor you found in your report.

- [ ] **Step 4: Pass Tolerate from ingest only**

In `src/runtime.rs`, `run_ingest` (line 428) calls `direct_context(...)` (line 1604) on its first line. Give `run_ingest` a way to request tolerance — either a policy-taking variant of `direct_context`, or construct the `ContextRequest` inline in `run_ingest`. Prefer whichever reads more like the surrounding code.

Set `Tolerate` **only when `!arguments.dry_run`**. A dry run reports on an index it will not repair, so it has no reason to open one it cannot trust.

Then change ingest's own open at `src/runtime.rs:458` to `IndexDb::open_existing_with_policy(..., OpenMode::ReadWrite, FingerprintPolicy::Tolerate)`. Leave the dry-run open at line 442 on the default.

- [ ] **Step 5: Pass the policy at gate 2**

Change `Doctor::run(self.path())?` to `Doctor::run_with_policy(self.path(), FingerprintPolicy::Tolerate)?` at `src/store/generation.rs:331` and `:496` only.

Do NOT change line 284. Read its enclosing function (`configure_filesystem_scope`) and its only caller (`src/app/init.rs:319`) and confirm for yourself that it is init-only before leaving it alone. State that confirmation in your report.

- [ ] **Step 6: Run the tests**

Run: `cargo test --test fingerprint_recovery -- --nocapture`
Expected: PASS.

Run: `cargo test --test store_doctor && cargo test --test ingest_generations && cargo test --test context_resolution`
Expected: PASS. `context_resolution` matters most here — you changed a struct every command uses.

- [ ] **Step 7: Verify the gate order really moved**

Build the release binary and confirm ingest now gets *past* the gates. It will not repair the index yet — that is Task 3b — but its failure (or success) must no longer be `PIPELINE_FINGERPRINT` at context resolution.

```bash
cargo build --locked --release
cd "$(mktemp -d)" && git init -q . && printf 'fn hello() {}\n' > lib.rs
export HSUM_HOME="$PWD/home" && mkdir -p home
/absolute/path/to/checkout/target/release/hsum init
```

Patch the stored fingerprint to `bb`×32 with `python3` + `sqlite3` (get the database path from `hsum context`; open, UPDATE `index_meta` and `generations`, commit, close — do NOT issue `PRAGMA journal_mode`). Then:

- `hsum search hello` must still fail with `PIPELINE_FINGERPRINT`
- `hsum status` must still fail with `PIPELINE_FINGERPRINT`
- `hsum ingest --dry-run` must still fail with `PIPELINE_FINGERPRINT`
- `hsum ingest` must NOT fail with `PIPELINE_FINGERPRINT` at context resolution

Paste the real terminal output into your report. If `hsum ingest` still reports `PIPELINE_FINGERPRINT`, there is a fourth gate — find it, report it, and STOP rather than guessing.

- [ ] **Step 8: Full check and commit**

Run: `cargo fmt --all && cargo xtask check`
Expected: PASS.

```bash
git add src/app/context.rs src/runtime.rs src/store/generation.rs tests/fingerprint_recovery.rs
git commit -m "refactor: let ingest resolve a context for a stale index

run_ingest resolves its context before reaching any ingest code, and that
resolution opened the index under the strict default policy, so ingest
failed on a fingerprint mismatch before it could repair one. Carry the
policy on ContextRequest, defaulting to reject, and pass tolerate only
from a non-dry-run ingest."
```

---

### Task 3b: Heal the index during ingest

**Files:**
- Modify: `src/store/generation.rs` (the commit transaction around lines 728-741)
- Test: `tests/fingerprint_recovery.rs` (extend)

**Interfaces:**
- Consumes: everything from Tasks 2 and 3 — ingest can now open and inspect a stale index.
- Produces: a healed index. `index_meta.pipeline_fingerprint` equals `pipeline_fingerprint()`, and no `generations` row disagrees with it.

**This is the only task in the plan that deletes user data.** Its transactional integrity is the entire point. A partial delete must never commit.

- [ ] **Step 1: Write the failing tests**

Add to `tests/fingerprint_recovery.rs`:

```rust
/// `hsum ingest` must rebuild a stale index, and the result must satisfy the
/// *full* doctor inspection — not merely open.
#[test]
fn ingest_heals_a_stale_index_and_full_doctor_passes_afterward() {
    let fixture = stale_fingerprint_index();

    assert!(
        hsum::store::Doctor::run(&fixture.database_path).is_err(),
        "fixture must start out failing doctor"
    );

    run_ingest(&fixture).expect("ingest must heal a stale index");

    let report = hsum::store::Doctor::run(&fixture.database_path)
        .expect("full doctor must pass after the heal");
    assert_eq!(
        report.pipeline_fingerprint,
        hsum::store::pipeline_fingerprint(),
        "healed index must carry the current fingerprint"
    );
}

/// A document unchanged across the fingerprint bump keeps a document_heads row
/// pointing at a pre-change generation. Deleting that generation outright
/// violates ON DELETE RESTRICT, so the heal must retain and relabel it. This
/// test is the one that catches a naive delete-everything implementation.
#[test]
fn an_unchanged_document_survives_the_heal_and_stays_searchable() {
    let fixture = stale_fingerprint_index();

    run_ingest(&fixture).expect("ingest must heal a stale index");

    let hits = search_after_heal(&fixture, "hello");
    assert!(
        !hits.is_empty(),
        "a document unchanged across the bump must still be searchable"
    );
}

/// A normal ingest on a healthy index must not delete anything or rewrite the
/// fingerprint. The heal is conditional, not unconditional work.
#[test]
fn a_healthy_index_is_not_healed() {
    let fixture = healthy_index();
    let before = fingerprint_row_bytes(&fixture.database_path);
    let documents_before = active_document_count(&fixture.database_path);

    run_ingest(&fixture).expect("ingest must succeed on a healthy index");

    assert_eq!(before, fingerprint_row_bytes(&fixture.database_path));
    assert_eq!(
        documents_before,
        active_document_count(&fixture.database_path),
        "a healthy ingest must not delete documents"
    );
}
```

Write `run_ingest`, `search_after_heal`, `fingerprint_row_bytes`, `active_document_count`, and `healthy_index`. Keep them as private helpers in `tests/fingerprint_recovery.rs` rather than in `tests/support/mod.rs` — helpers placed in the shared support module trigger dead-code errors under `-D warnings` in every other test binary that does not use them, and `#[allow(...)]` is banned.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test fingerprint_recovery -- --nocapture`

Expected: the two heal tests FAIL — ingest now runs but leaves the stored fingerprint stale, so `Doctor::run` still errors afterward. `a_healthy_index_is_not_healed` should already PASS, which is what makes it a useful guard.

- [ ] **Step 3: Derive the deletion order from the schema**

Read `migrations/0001_alpha1.sql` in full before writing any DELETE. The foreign-key graph dictates the order and getting it wrong aborts the transaction.

Three edges CASCADE and do work for you:
- `generation_changes.generation_id` → generations (line 144)
- `source_sync_errors.generation_id` → generations (line 189)
- `passage_literals.passage_id` → active_passages (line 178)

Everything else in the document/chunk/blob graph is RESTRICT: `document_heads` → documents/document_versions/generations (126-129), `active_passages` → documents/document_versions/chunks (160-163), `document_versions` → documents/content_blobs (113-114), `chunks` → chunk_layouts (98).

**The constraint that breaks a naive implementation:** a document unchanged across the fingerprint bump still has a `document_heads` row referencing the generation that last touched it. If that generation predates the change, deleting it violates RESTRICT. So pre-change generations split in two:

- **superseded** — nothing references them after the rebuild → delete
- **retained** — still referenced by a surviving `document_heads` row → update their `pipeline_fingerprint` in place

Record in your report the exact order you used and why.

- [ ] **Step 4: Implement the heal in the commit transaction**

In `src/store/generation.rs`, inside the same transaction as the existing `set_metadata` calls (lines 728-741), add the heal so it commits atomically with the generation.

Guard it so it runs ONLY when the stored fingerprint actually differs — read the stored value with the existing `metadata_value` helper and compare against `pipeline_fingerprint()` before doing anything. A healthy ingest must delete nothing and write no fingerprint.

Then, when healing: delete superseded pre-change generations and their dependent rows in schema order, update retained generations' `pipeline_fingerprint` in place, and finally:

```rust
        set_metadata(
            &transaction,
            "pipeline_fingerprint",
            pipeline_fingerprint().as_bytes(),
        )?;
```

`validate_generation_invariants` (`src/store/doctor.rs`) counts generations whose fingerprint differs from the binary's, so any row you neither delete nor relabel will fail Full doctor — which Step 1's first test asserts against.

- [ ] **Step 5: Run the tests**

Run: `cargo test --test fingerprint_recovery -- --nocapture`
Expected: PASS all tests.

Run: `cargo test --test ingest_generations && cargo test --test store_doctor && cargo test --test search_contract`
Expected: PASS. A normal ingest must be completely unaffected.

- [ ] **Step 6: Verify the whole story against the real binary**

This is mandatory. Store-layer tests are not sufficient evidence — every prior reviewer of this codebase concluded by code-reading that search survived a fingerprint mismatch, and one execution disproved it.

```bash
cargo build --locked --release
cd "$(mktemp -d)" && git init -q . && printf 'fn hello() {}\n' > lib.rs
export HSUM_HOME="$PWD/home" && mkdir -p home
/absolute/path/to/checkout/target/release/hsum init
/absolute/path/to/checkout/target/release/hsum search hello   # works
```

Patch the fingerprint to `bb`×32 (open, UPDATE `index_meta` and `generations`, commit, close — no `PRAGMA journal_mode`), then confirm the full sequence:

- `hsum search hello` → fails, `PIPELINE_FINGERPRINT`
- `hsum ingest` → **succeeds**
- `hsum doctor` → **passes**
- `hsum search hello` → **returns the passage again**

Paste the complete terminal transcript into your report. If any step differs, STOP and report rather than adjusting the test to match.

- [ ] **Step 7: Full check and commit**

Run: `cargo fmt --all && cargo xtask check`
Expected: PASS.

```bash
git add src/store/generation.rs tests/fingerprint_recovery.rs
git commit -m "feat: rebuild a stale index during ingest instead of failing closed

An index whose stored pipeline fingerprint predates the running binary
could not be repaired by any command. Ingest now drops document versions
recorded under the old pipeline and rewrites the stored fingerprint inside
its commit transaction, so the rebuild is atomic. Generations still
referenced by surviving document heads are relabelled rather than deleted,
because document_heads.generation_id is ON DELETE RESTRICT."
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
