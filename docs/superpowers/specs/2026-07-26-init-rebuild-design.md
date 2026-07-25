# `hsum init --rebuild`: recovering an index the current binary cannot open

Date: 2026-07-26
Status: approved design, not yet implemented
Supersedes: `2026-07-25-pipeline-fingerprint-recovery-design.md` (heal-inside-ingest — abandoned, see Why the previous approach was abandoned)
Scope: `src/app/init.rs`, `src/cli.rs`, `src/runtime.rs`, `src/config/trust.rs`, docs

## The problem

hSUM hashes its indexing rules — the indexable extension set, the chunker
configuration, the tokenizer settings — into a pipeline fingerprint stored in
each index. When a release changes those rules, every existing index carries a
fingerprint the new binary does not recognize, and **every command fails**:

| Command | Result on such an index |
|---|---|
| `hsum search` / `get` / `status` / `context` | fails |
| `hsum doctor` | fails |
| `hsum ingest` | fails — including `--dry-run` |
| `hsum init` | fails |

All report subcode `PIPELINE_FINGERPRINT`: *"the index was built by a different
hSUM indexing pipeline."*

There is no in-product recovery. Worse, the recovery documented on `main` is
**incomplete and does not work**: it says to delete the managed index
directory, but the trust registry still holds a binding for that root, so the
next `init` fails with `INDEX_NOT_FOUND`. Verified by running it. Actual
recovery today requires deleting *two* things — the index directory **and**
`<config>/trusted-projects.toml` — the second of which is undocumented and
which a user would have no reason to guess.

## Goal

One documented command rebuilds an unopenable index, with no manual file
surgery.

## Non-goals

- Preserving evidence or citations across a pipeline change. A rebuild is a
  rebuild; pre-change citations do not survive. This is stated, not hidden.
- Repairing corruption. `--rebuild` addresses a *recognized incompatibility*,
  not a damaged database. Corruption still fails closed.
- A `migrate`, `repair`, or `prune` command. Alpha.1 has none and gains none.
- Relaxing the fingerprint check on any read path. `search`, `get`, `status`,
  `context`, `doctor`, and `mcp` continue to fail closed.

## Why the previous approach was abandoned

The prior design healed the index inside `hsum ingest`. Three implementation
and review rounds established that it does not fit the architecture:

**It required threading an exception through four independent gates.** Ingest
traverses `materialize_context` (`src/app/context.rs:210`, reached by *every*
command), its own `open_existing` (`src/runtime.rs:458`), and two `Doctor::run`
calls (`src/store/generation.rs:331`, `:496`). Each was discovered only after
the previous one was fixed and the binary still failed. Gate `:331` is
additionally **shared** between `ingest` and `ingest --dry-run`
(`src/app/mod.rs:296` and `:393`), so the intended "dry-run stays strict"
behavior was not even expressible without further refactoring.

**Relaxing all four gates still does not recover the index.** Verified
empirically: ingest then runs to completion but never rewrites the stored
fingerprint, so the next command fails exactly as before.

**Rewriting generation history breaks six invariants, not one.** The plan
accounted for `validate_generation_invariants`' fingerprint count
(`src/store/doctor.rs:468`). It also enforces `index_epoch ==
committed_count` (`:506`) — and `index_epoch` is monotonic, never decremented
(`src/store/generation.rs:729`); that every committed generation has changes
(`:631`); and full `prior_version_id` replay continuity (`:640-731`). Deleting
generations requires reconstructing all three.

The root cause is conceptual. `hsum ingest` means *"add a generation to an
index I trust."* Generations are an append-only chain with replay invariants,
and every layer re-validates that trust. A fingerprint mismatch is a statement
about the **whole index**, not an increment to it. Threading an exception
through a trust check to reach a commit path whose invariants assume continuity
that no longer holds is why the fix kept growing.

`hsum init` already owns index lifecycle — creating the managed database, the
source, the project, and the trust binding. Recovery belongs there.

## Design

`hsum init --rebuild` replaces the index bound to the current root.

`initialize()` (`src/app/init.rs:137`) already branches at `if let Some(binding)
= existing_binding` (`:175`). Today that branch calls `validate_trusted_database`
— which is where a stale index dies. With `--rebuild`, the branch instead tears
down and falls through to the existing fresh-init path.

**Teardown, in order:**

1. Remove the trust binding for this canonical root and save the registry
   atomically. `TrustRegistry` already exposes `bindings()`,
   `from_bindings(...)`, and `save_atomic(...)` (`src/config/trust.rs:180`,
   `:174`, `:327`) — filtering and re-saving needs no new API.
2. Remove the managed database and its WAL/SHM sidecars.
3. Fall through to the ordinary creation path, which builds a new index,
   project, source, and binding, and ingests.

Registry first is deliberate: a crash between steps leaves a registry with no
binding and an orphaned database file, which the ordinary `init` path already
handles. The reverse order would leave a binding pointing at a deleted
database — the `INDEX_NOT_FOUND` dead end users hit today.

### What `--rebuild` must refuse

- **Without an existing binding for this root.** There is nothing to rebuild;
  plain `init` is the right command. Refuse with a message saying so.
- **Combined with `--no-ingest`.** A rebuild that indexes nothing leaves the
  user worse off than before, with an empty index and no evidence.
- **Outside a trusted root**, or where plain `init` would refuse (broad root,
  oversized source). `--rebuild` does not bypass any existing safety gate.

### `--dry-run`

`init --rebuild --dry-run` reports what would be destroyed — the index path,
its document and passage counts if readable, and the binding — and writes
nothing. Counts come from a best-effort read; if the database cannot be opened
far enough to count, say so rather than failing, since being unopenable is the
whole reason the user is here.

### What the user is told

Because this destroys evidence, `--rebuild` states the cost before doing it and
confirms after. Output names the index being replaced, that prior citations
stop resolving, and the new document and passage counts.

`--rebuild` requires no interactive confirmation: it is an explicit flag on an
explicit command, consistent with how `--allow-mass-delete` and
`--allow-empty-snapshot` already work in this codebase.

### Error remediation

`ErrorSubcode::PipelineFingerprint`'s `fix` line currently says *"run hsum
ingest to rebuild this index under the current rules"* — which does not work.
It becomes `hsum init --rebuild`, so the error a user hits names the command
that actually recovers them.

## What this does not change

The four gates stay strict. `FingerprintPolicy` — added in `9ca396d` and
already reviewed — keeps its `Tolerate` variant, now used only by tests and by
the `--rebuild` teardown's best-effort dry-run read. Nothing in `src/runtime.rs`
or `src/store/generation.rs` needs the policy threaded through it, and the
`ingest` path is untouched.

## Testing

- A stale-index fixture: `init`, then rewrite `index_meta.pipeline_fingerprint`
  and the `generations` rows to a prior value.
- `hsum init --rebuild` against it succeeds; afterward `doctor` passes at
  **Full** depth and `search` returns a passage.
- `--rebuild` on a **healthy** index also works — it is not conditional on
  detecting a mismatch, and must not corrupt a working index.
- `--rebuild --dry-run` writes nothing: the database file's mtime and the
  registry are unchanged, and the stale index still fails afterward.
- `--rebuild` without an existing binding is refused with a clear error.
- `--rebuild --no-ingest` is refused.
- After a rebuild, a citation captured before it returns `EVIDENCE_FORGOTTEN`
  or `NOT_FOUND` — never a wrong passage, and never a stale hit.
- The trust registry has exactly one binding for the root afterward, with a
  **new** binding ID — proving teardown and recreation, not mutation in place.
- Read commands still fail closed on a stale index: `search`, `status`,
  `context`, and `ingest` (both modes) must each still report
  `PIPELINE_FINGERPRINT`.

**A test must drive the real binary end-to-end.** Four separate code-reading
conclusions about this subsystem were wrong and were caught only by execution.
Store-layer tests are not sufficient evidence for this change.

## Documentation

`CHANGELOG.md`, `README.md`, and the comment above
`alpha_1_pipeline_fingerprint_is_frozen` (`src/store/schema.rs`) all currently
describe recovery incorrectly — they omit the trust registry, so the procedure
they document fails. All three must state `hsum init --rebuild`.

## Sequencing

1. Refuse-and-report: the flag, its validation rules, and `--dry-run`. Ships
   without destroying anything.
2. Teardown and rebuild.
3. Error remediation text and docs.

Step 1 first means the destructive path lands only after its guards exist and
are tested.
