# Pipeline fingerprint recovery: making a fingerprint change survivable

Date: 2026-07-25
Status: approved design, not yet implemented
Scope: `src/store/open.rs`, `src/store/doctor.rs`, `src/store/generation.rs`,
`src/runtime.rs`, `src/domain/error.rs`, and their tests
Blocks: merge of `feat/agent-adoption`

## The problem, established empirically

A fingerprint change today leaves the user with a dead index and no way out.
This was verified by building an index, rewriting its stored
`index_meta.pipeline_fingerprint` to the previous value, and running the current
binary against it:

| Command | Documented behavior | Actual behavior |
|---|---|---|
| `hsum search` | works | **fails** |
| `hsum status` | — | **fails** |
| `hsum context` | — | **fails** |
| `hsum doctor` | fails | fails |
| `hsum ingest` | **the remedy** | **fails** |

All five fail with `SCHEMA_CHECKSUM: the hSUM index schema checksum is invalid`.

### Why every command fails

`IndexDb::open_existing` calls `inspect_connection(connection, read_only,
InspectionDepth::Open)` at `src/store/open.rs:190`. That runs
`validate_metadata` (`src/store/doctor.rs:340`), which compares the stored
`pipeline_fingerprint` against the binary's and returns
`StoreError::PipelineFingerprintMismatch` (`src/store/doctor.rs:362`).

Every command opens the index, so every command hits it. The check is not
doctor-scoped as its call site suggests; `open.rs` imports the function from
`doctor.rs` (`src/store/open.rs:17`).

### The gate map (corrected 2026-07-26)

An earlier revision of this spec described a single check, and the first
implementation plan described two. Both were wrong. A `Doctor::run`/`open`
audit plus an instrumented run of the real binary establish that `hsum ingest`
traverses **three** fingerprint gates in this order:

| Order | Gate | Location | Reached by |
|---|---|---|---|
| 0 | `materialize_context` | `src/app/context.rs:210` | every CLI command, via `resolve_context` → `direct_context`, before any command-specific logic |
| 1 | `open_existing(ReadWrite)` | `src/runtime.rs:458` | ingest only |
| 2 | `Doctor::run` | `src/store/generation.rs:331`, `:496` | ingest only |

Gate 0 is the one that matters most and was missed twice: `run_ingest`'s very
first statement is `direct_context(...)`, so ingest fails there before reaching
any of its own code. A fix that relaxes only gates 1 and 2 changes nothing
observable — verified by building the binary with those two gates relaxed and
watching `hsum ingest` still fail with `PIPELINE_FINGERPRINT`.

Two further sites must stay on `Reject` deliberately:

- `src/store/generation.rs:284` (`configure_filesystem_scope`) is reached only
  from `hsum init --no-ingest` (`src/app/init.rs:319`), not from ingest.
- `src/app/context.rs:181` (`resolve_trust_target`) serves `hsum trust`, not
  ingest.

Relaxing either would widen the hole past what recovery requires.

### Why the documented remedy cannot work

`hsum ingest` must open the index to rebuild it, so it fails at the same gate
before doing any work. Worse, the ingest commit path only writes `index_epoch`
and `active_generation` (`src/store/generation.rs:732-741`); it never writes
`index_meta.pipeline_fingerprint`. So even if ingest could open the index, it
would not clear the mismatch.

**This is a deadlock: the remedy requires the thing the mismatch forbids.**
The only current escape is deleting the index directory and re-running `init`.

### A second, smaller defect

The error text names the wrong thing. `PipelineFingerprintMismatch` maps to
`ErrorSubcode::SchemaChecksum` (`src/runtime.rs:2933-2935`), so the user is told
"the hSUM index schema checksum is invalid" when the schema is intact and the
pipeline fingerprint is what changed. The stated fix — "quarantine the index and
run hsum doctor" — sends them to a command that fails the same way.

## Goals

- `hsum ingest` recovers a fingerprint-mismatched index without the user
  deleting anything.
- Read commands keep failing closed until the index is rebuilt.
- The error a user sees names the real cause and a remedy that works.

## Non-goals

- A `migrate`, `repair`, or `prune` command. Alpha.1 deliberately has none, and
  this fix does not need one.
- Auto-healing at open time. Rejected: see Rejected alternatives.
- Preserving citations issued before the fingerprint change. See Decision 2.
- Any change to what the fingerprint covers, or to the extension set.

## Decision 1: heal in ingest, not at open

`hsum ingest` opens the index in a mode that tolerates a pipeline-fingerprint
mismatch, rebuilds, and rewrites the stored fingerprint as part of the commit
transaction. Every other command continues to fail closed on mismatch.

This is the smallest possible hole in the fail-closed posture, and it sits
exactly where a full re-chunk already happens. The stale generations are being
replaced wholesale at that moment, so the fingerprint they carried is genuinely
obsolete rather than being papered over.

## Decision 2: drop pre-change versions, keep one regime

When ingest heals a mismatched index, historical document versions produced
under the old pipeline are removed. The healed index contains only content
chunked under the current rules.

**Consequence, stated plainly: `hsum://` citations issued before the upgrade
stop resolving.** That is a real cost against hSUM's central promise that a
citation survives. It is accepted here because the alternative is worse in a
different way: an index whose stored chunks disagree with the fingerprint
stamped on it, where two chunking regimes coexist and neither the user nor
`doctor` can tell which rules produced a given passage. A single clean regime
means every chunk in the index is consistent with the fingerprint that describes
it, which is the invariant the fingerprint exists to express.

Dropped-version citations surface through the existing `EVIDENCE_FORGOTTEN`
error code (`src/domain/error.rs:34`), which already means "the cited evidence
is no longer resolvable." No new error taxonomy is required. A new subcode
distinguishes this cause from an explicit forget tombstone.

### Not every pre-change generation can be deleted

A first implementation attempt established a constraint this spec originally
missed: a document that is **unchanged** across the fingerprint bump keeps a
`document_heads` row pointing at the generation that last touched it. If that
generation predates the change, deleting it outright violates
`document_heads.generation_id → generations(id) ON DELETE RESTRICT`
(`migrations/0001_alpha1.sql:129`) and aborts the transaction.

So pre-change generations split in two:

- **Superseded** — nothing references them after the rebuild. Delete these.
- **Retained** — still referenced by a surviving `document_heads` row. Update
  their `pipeline_fingerprint` in place instead of deleting.

Both outcomes satisfy `validate_generation_invariants`
(`src/store/doctor.rs`), which counts generations whose fingerprint differs
from the binary's. Neither leaves a dangling reference.

### Deletion order matters

Deletion is deliberately hard in this schema. Three edges cascade and do the
work for you:

- `generation_changes.generation_id` → generations, CASCADE (line 144)
- `source_sync_errors.generation_id` → generations, CASCADE (line 189)
- `passage_literals.passage_id` → active_passages, CASCADE (line 178)

Every other edge in the document/chunk/blob graph is RESTRICT and dictates the
order: `document_heads` → documents/document_versions/generations (126-129),
`active_passages` → documents/document_versions/chunks (160-163),
`document_versions` → documents/content_blobs (113-114), `chunks` →
chunk_layouts (98).

The implementer derives the exact order from the schema rather than following a
list in this document, and records the order used and why. A partial delete
must never commit — the whole heal lives in the generation's commit
transaction.

## Design

### Open path

`IndexDb::open_existing` gains a variant that carries a policy for the
pipeline-fingerprint check: reject (today's behavior, the default for every
caller) or tolerate (used only by ingest's writer open). The policy affects
**only** the pipeline-fingerprint comparison. Application ID, schema version,
schema checksum, migration chain, and WAL/permission checks stay mandatory in
all modes — a tolerated fingerprint must not become a tolerated corrupt index.

### Context path (gate 0)

Because `materialize_context` opens the index for every command, the policy
must also thread through `ContextRequest` → `resolve_context` →
`materialize_context`. This is the load-bearing part of the change and the part
that touches shared code.

The policy is carried as a field on `ContextRequest`, defaulting to `Reject`.
`ContextRequest::direct(...)` keeps producing `Reject` requests, so all twelve
non-ingest command handlers are unchanged by construction. Only `run_ingest`
constructs its request with `Tolerate`, and only for a non-dry-run ingest — a
`--dry-run` ingest reports on an index it will not repair, so it has no reason
to open one it cannot trust.

A test must assert that a command other than ingest still fails closed on a
stale index. Without it, a later refactor could flip the default and silently
open every read path to mismatched indexes.

### Ingest path

On a tolerated-mismatch open, ingest:

1. Records that it is healing, so the outcome can be reported.
2. Performs its normal discovery and generation build.
3. Inside the existing commit transaction, additionally: deletes pre-change
   versions in dependency order, then writes the current
   `pipeline_fingerprint` into `index_meta`.
4. Commits. Either the whole heal lands or none of it does.

Because generations carry their own `pipeline_fingerprint`
(`src/store/generation.rs:1295`), post-heal `doctor` at Full depth must also
pass — the generation-level invariant at `src/store/doctor.rs:439` is what
would otherwise catch a half-healed index.

### Error path

`PipelineFingerprintMismatch` gets its own subcode rather than reusing
`SchemaChecksum`. Its four-line human error becomes:

- problem: the index was built by a different hSUM pipeline
- cause: the indexing rules changed, so stored evidence no longer matches this
  binary
- fix: run `hsum ingest` to rebuild this index under the current rules
- learn: `hsum help error <SUBCODE>`

The offline error catalog entry must state that rebuilding drops evidence
predating the change, so a user learns the cost before paying it.

### Reporting

Ingest's human output states when it healed and what it cost — that the index
was rebuilt under new pipeline rules and that citations issued before the
rebuild no longer resolve. Silence here would make a destructive step invisible.

## Rejected alternatives

**Auto-heal at open time.** Rewriting the fingerprint on any open would let a
`search` silently "heal" an index whose every stored chunk still reflects the
old rules. It converts a loud failure into quiet wrong answers, which is the
opposite of what this codebase is built to do.

**A dedicated `migrate`/`repair` command.** Cleaner conceptually, but alpha.1
states it has no repair or migration command, and adding one is a larger
promise than this fix requires. Revisit if a future change needs healing that
ingest cannot express.

**Carry old versions forward.** Keeps citations alive, but produces an index
mixing two chunking regimes. Rejected per Decision 2.

## Testing

- A stale-index fixture: build an index, rewrite `index_meta.pipeline_fingerprint`
  and the `generations` rows to a prior value, then assert `search`, `status`,
  and `context` all fail with the pipeline-fingerprint subcode — not
  `SchemaChecksum`.
- Assert `hsum ingest` succeeds against that fixture, and that afterward
  `doctor` passes at **Full** depth, not just shallow.
- Assert the healed index's `index_meta.pipeline_fingerprint` and every
  `generations.pipeline_fingerprint` equal the current value.
- Assert a citation to a pre-change revision returns `EVIDENCE_FORGOTTEN` with
  the new subcode after healing.
- Assert a heal that fails mid-transaction leaves the index exactly as it was —
  no partial deletion, no rewritten fingerprint.
- Assert the tolerate policy does **not** relax application ID, schema
  checksum, or migration-chain validation: corrupt each in turn and confirm
  ingest still refuses.
- Assert non-ingest commands never open with the tolerate policy. Concretely:
  after a stale index exists, `status`, `context`, and `search` must each still
  fail with the pipeline-fingerprint subcode, and `hsum ingest --dry-run` must
  fail too — a dry run reports on an index it will not repair.
- Assert a document unchanged across the fingerprint bump survives the heal and
  is still searchable afterward. This is the case that makes naive
  delete-all-pre-change-generations abort on a foreign-key violation.

**This work is not done until a test drives the real binary against a real
stale index.** Every prior reviewer and the author concluded by code-reading
that search survived a mismatch; one five-minute execution disproved it. The
regression test must exercise the CLI, not just the store layer.

## Documentation

- `CHANGELOG.md`: correct the Unreleased entry. All commands fail on a stale
  index; `hsum ingest` is the remedy; rebuilding drops pre-change evidence.
- `README.md`: document the recovery path in the ingest section.
- `src/store/schema.rs`: correct the comment above
  `alpha_1_pipeline_fingerprint_is_frozen` for the same reason.

## Sequencing

1. Error taxonomy: new subcode and accurate text. Independently shippable and
   valuable even alone. **Landed** (`2897794`) — verified end-to-end: all five
   commands now report `PIPELINE_FINGERPRINT` instead of `SCHEMA_CHECKSUM`.
2. Open-path policy plumbing (gates 1 and 2), defaulting every existing caller
   to reject. **Landed** (`9ca396d`).
3. Context-path policy (gate 0), so ingest can reach its own code at all.
   Touches shared code; only `run_ingest` passes `Tolerate`.
4. Ingest heal: split superseded from retained generations, delete the former,
   rewrite the fingerprint, all in one transaction.
5. Docs.

Steps 1-3 are additive and behavior-preserving for every non-ingest caller.
Step 4 carries the real weight: it is the only step that deletes user data, and
its transactional integrity is what the tests above exist to prove.

Splitting the old step 3 into steps 3 and 4 is deliberate. The first attempt
bundled them, and the gate-0 discovery arrived only after the heal was already
written — a reviewer could not have approved or rejected either half on its
own.
