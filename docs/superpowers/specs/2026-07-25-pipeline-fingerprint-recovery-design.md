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

### Deletion order matters

`document_versions`, `document_heads`, `chunks`, and `content_blobs` are wired
with `ON DELETE RESTRICT` (`migrations/0001_alpha1.sql:101-131`). Deletion is
deliberately hard in this schema. The heal must therefore remove rows in
dependency order inside one transaction — heads, then generation changes and
active passages, then versions, then orphaned chunks and blobs — or the
transaction will abort on a foreign-key violation. A partial delete must never
commit.

## Design

### Open path

`IndexDb::open_existing` gains a variant that carries a policy for the
pipeline-fingerprint check: reject (today's behavior, the default for every
caller) or tolerate (used only by ingest's writer open). The policy affects
**only** the pipeline-fingerprint comparison. Application ID, schema version,
schema checksum, migration chain, and WAL/permission checks stay mandatory in
all modes — a tolerated fingerprint must not become a tolerated corrupt index.

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
- Assert non-ingest commands never open with the tolerate policy.

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
   valuable even alone.
2. Open-path policy plumbing, defaulting every existing caller to reject.
3. Ingest heal: version deletion plus fingerprint rewrite in one transaction.
4. Docs.

Steps 1 and 2 are additive and low-risk. Step 3 carries the real weight: it is
the only step that deletes user data, and its transactional integrity is what
the tests above exist to prove.
