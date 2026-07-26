# Session record — 2026-07-25/26

Complete record of one working session: what was asked, what was built, what
was found, what was wrong, and what remains. Written at the end of the session
from verified state, not from memory — every commit SHA, line reference, and
test result below was re-checked against the repository before writing.
Branch-ahead counts in §5 and §8 describe the state immediately before this
record's own commit, which became the branch's fifth commit afterward.

---

## 1. What was asked

Five requests, in order:

1. What should a user expect when installing hSUM for their agents?
2. Plan fixes for the three gaps that answer surfaced.
3. Execute the plan.
4. Test, then merge.
5. Fix the breaking change the merge introduced.

---

## 2. Starting point

`main` at `dbe2ef6`. A public GitHub pre-release `v0.1.0-alpha.1` had been
live since 2026-07-25 with binaries for macOS arm64 and Linux x86_64.

---

## 3. The install-experience review

Reading the repository produced an assessment with one central finding:

**hSUM's agent adoption was structurally weak.** The MCP server advertised four
tools whose descriptions named what they touched but never when to use them,
and its `instructions` field was a single sentence about the untrusted-content
boundary. An agent holding Grep, Read, and Glob had no stated reason to reach
for a fourth search tool. Predicted usage: only when a human typed "search
hSUM," essentially never otherwise.

Two supporting findings:

- **Ten indexed extensions.** A Java, C, C++, Ruby, C#, Swift, or Kotlin
  repository produced a near-empty index on first `init`, which reads as a
  broken product rather than a documented scope boundary.
- **The one capability Grep cannot have** — resolving a citation against bytes
  as they were at ingest, and reporting whether the live file still matches —
  appeared nowhere in any tool description.

---

## 4. Work item 1: agent adoption (merged to `main`)

Design: `docs/superpowers/specs/2026-07-25-hsum-agent-adoption-design.md`
Plan: `docs/superpowers/plans/2026-07-25-hsum-agent-adoption.md`

### Decisions taken

| Decision | Choice | Why |
|---|---|---|
| Extension strategy | Widen now, configurable later | Pays index-invalidation cost once, not twice |
| `ChunkKind` granularity | One variant per language | Preserves per-kind layout isolation the fingerprint machinery exists to provide |
| Scope | Tier 1 source languages only | Config formats need a lockfile-exclusion list with no principled stopping point, and would encode an editorial judgment inside a hash meant to encode a rule |
| Shell/SQL chunking | Line-chunked, documented | No unambiguous declaration keyword; stated as a property rather than hidden |

### Commits (in `main` via merge `e4f2263`)

| SHA | Change |
|---|---|
| `0d25105` | MCP server instructions state when to use hSUM (108 → 1030 bytes) |
| `5650b80` | Four tool descriptions carry the drift story |
| `044a093` | Eleven `ChunkKind` variants, nine declaration keyword lists, three tests |
| `672953e` | rustfmt cleanup (see finding F1) |
| `f65c054` | Eighteen extensions admitted; pipeline fingerprint bumped — **BREAKING** |
| `2db8d68` | `extension_coverage` test exercises the real discovery gate |
| `8e9bb60` | README + CHANGELOG |
| `98d528e` | Corrected the stale-index doctor failure mode |
| `69b22e9`, `9edefc8`, `76cff4c` | Recovery design, plan, and doc correction |

Merged as `e4f2263`. `main`: 309 tests passing, 0 failing, `cargo xtask check`
green across fmt, clippy, tests, doctests.

### Languages added

Declaration-chunked (nine): Java, Kotlin, C, C++, Ruby, C#, Swift, PHP, Scala.
Line-chunked (two): shell, SQL.
Extensions: 10 → 28.
`ChunkKind` variants: 7 → 18.
Pipeline fingerprint: `bb24fc64…` → `ff465d49…`.

### Verified end-to-end against the release binary

A polyglot repository (Java, Kotlin, C++, Ruby, C#, Swift, SQL, shell,
Markdown) was indexed and exercised:

- 9/9 files indexed, 0 skipped — on `main` before this work, 1
- Every new language returns search hits
- Citation resolution returns exact bytes and correct line spans
- **Drift detection works**: after editing a file, `get` returned the
  *pre-edit* bytes and reported `changed`; search labelled the result
  `changed_since_ingest`
- Full 1030-byte instructions delivered over a real MCP stdio session
- Four tool descriptions delivered: 386 / 337 / 240 / 212 bytes
- `untrusted_content: true` preserved
- `.json`, `.yaml`, `.env`, `node_modules` still correctly excluded
- `doctor` and `client doctor codex` both pass

---

## 5. Work item 2: fingerprint recovery (branch, not merged)

Branch `fix/fingerprint-recovery`, 4 commits ahead of `main`.

| SHA | Change | Status |
|---|---|---|
| `2897794` | `ErrorSubcode::PipelineFingerprint` with honest text | Kept |
| `9ca396d` | `FingerprintPolicy` threaded through inspection paths | Kept |
| `3c35acf` | Gate map corrected to three gates | Superseded |
| `71d3085` | Replaced heal-inside-ingest with `hsum init --rebuild` | Current |

`2897794` is verified working: all five commands now report
`PIPELINE_FINGERPRINT` — *"the index was built by a different hSUM indexing
pipeline"* — instead of the misleading `SCHEMA_CHECKSUM`.

The heal-inside-ingest approach was **abandoned** after three rounds. See
finding F5.

Current design: `docs/superpowers/specs/2026-07-26-init-rebuild-design.md`.

---

## 6. Findings

### F1 — `cargo xtask check` was broken by Tasks 1–2 and not caught

Both implementers ran clippy and the test suite but never `cargo fmt`, and
controller verification repeated the same gap. Task 3's implementer found two
rustfmt violations in `tests/mcp_contract.rs`, correctly diagnosed them as
pre-existing rather than blaming its own work, and both it and its reviewer
independently reproduced them at `5650b80`. Fixed in `672953e`.

**Lesson:** verifying "clippy + tests" is not verifying "the project's check
command."

### F2 — The plan's cross-site guard test could not reach what it guarded

`tests/extension_coverage.rs` claimed to keep three sites in sync. But
`is_supported_path` is a private fn inside a private `mod unix`
(`src/ingest/filesystem.rs:1262`), unreachable from an integration test. The
test compared a hardcoded array against two sites — and that array was a third
copy. The exact drift it advertised catching would have passed.

Fixed in `2db8d68` by exercising the real gate via the public `discover_files`.
Verified with a teeth-check: removing `swift` from the private gate makes the
test fail; restoring it makes it pass.

### F3 — Two frozen digests moved, not one

The plan asserted "no later task depends on the specific digest." False.
`retrieval_config_fingerprint()` (`src/mcp.rs:2034`) hashes
`pipeline_fingerprint()` into its own descriptor, and
`tests/mcp_contract.rs:211` freezes that derived value.

Found by the Task 4 implementer, which stopped rather than editing a file
outside its scope. Both digests were independently recomputed three times
(implementer, reviewer, controller) and confirmed authentic — not invented to
make a test pass.

A third frozen digest, `schema_checksum` (`tests/store_doctor.rs:873`), hashes
the migration SQL and was confirmed unaffected.

### F4 — The documented `doctor` failure mode was wrong

CHANGELOG and a source comment claimed a stale index makes `doctor` report
`GenerationInvariant("generation pipeline fingerprint")`. The real path is
`validate_metadata` (`src/store/doctor.rs:199`), which runs **before** the
`InspectionDepth::Full` gate at `:202` and returns
`PipelineFingerprintMismatch` — a different subcode (`SchemaChecksum` vs
`HeadIndexMismatch`). A user following the docs would search for the wrong
error. Corrected in `98d528e`.

### F5 — The breaking change had no recovery path, and finding out took four attempts

This is the session's most consequential finding, and the one that repeatedly
defeated code-reading.

**The bug:** an index built by an earlier binary cannot be opened by any
command. `search`, `status`, `context`, `doctor`, and `ingest` all fail. The
documented remedy — `hsum ingest` — fails at the same gate, because it must
open the index to rebuild it.

**The gate count went 1 → 2 → 3 → 4**, each correction found by *executing the
binary*, never by reading:

| Claim | Reality |
|---|---|
| "`open.rs` never re-checks the fingerprint" | It does, via `inspect_connection` imported from `doctor.rs` (`src/store/open.rs:190` on `main`; `:213` on the recovery branch after Task 2 shifted lines) |
| "Ordinary search still works" | It does not. Every command fails |
| "Two gates" | `run_ingest`'s first statement is `direct_context(...)`, opening the index at `src/app/context.rs:210` before any ingest code |
| "Three gates" | `src/store/generation.rs:331` is a fourth, **shared** between `ingest` and `ingest --dry-run` (`src/app/mod.rs:296` and `:393`), making the intended "dry-run stays strict" design inexpressible |

**Relaxing every gate still does not recover the index** — verified by a
reviewer running the binary. Ingest completes but never rewrites the stored
fingerprint.

**Deleting generation history breaks six invariants, not one.** The plan
accounted for the fingerprint count (`src/store/doctor.rs:468`). It also
enforces `index_epoch == committed_count` (`:506`, and `index_epoch` is
monotonic, never decremented — `src/store/generation.rs:729`), that every
committed generation has changes (`:631`), and full `prior_version_id` replay
continuity (`:640-731`).

**Root cause of the repeated failure:** `hsum ingest` means "add a generation
to an index I trust." Generations are an append-only chain with replay
invariants, and every layer re-validates that trust. A fingerprint mismatch is
a statement about the *whole index*, not an increment to it. Threading an
exception through a trust check to reach a commit path whose invariants assume
continuity that no longer holds is why the fix kept growing.

### F6 — The recovery documented on `main` does not work

`main` currently instructs a stuck user to delete the managed index directory.
Verified by running it: the trust registry still holds a binding for that root,
so the next `init` fails with `INDEX_NOT_FOUND`.

Worse, `hsum context` fails at the same fingerprint gate, so it cannot report
the managed path when the user needs it. Manual recovery requires removing the
database and the exact matching binding from
`<config>/trusted-projects.toml`; deleting that whole file is safe only when it
contains no unrelated bindings.

**This is wrong on `main` right now.**

### F7 — A public pre-release exists and is affected

Corrected late in the session. Twice during this work the claim was made that
"the crate is `publish = false` with no tagged release, so nobody has an index
to break." That was wrong. `publish = false` only blocks crates.io.

Verified: GitHub pre-release `v0.1.0-alpha.1`, published 2026-07-25T03:20:52Z,
not a draft, 7 assets (macOS arm64 zip, Linux x86_64 tar.gz, SHA256SUMS, two
SPDX SBOMs, two checksums), 1 download each.

The tagged commit carries fingerprint `bb24fc64…`; `main` now carries
`ff465d49…`. **Any index built with the published binary is unreadable by
current `main`, and the recovery instructions are wrong (F6).**

Download counts of 1 are consistent with CI or self-verification rather than
real users, but that cannot be proven from the repository.

---

## 7. Process observations

**Execution beat code-reading, repeatedly.** Four separate conclusions about
the fingerprint subsystem — reached by the author and confirmed by up to three
independent reviewers each — were wrong. Every one was caught by running the
binary. The pattern: reviewers traced *where a value is compared* rather than
*what a user's command actually does*.

**Subagent scope boundaries worked.** Three times an implementer hit something
outside its declared scope and stopped with a question instead of guessing:
the second frozen digest (F3), the third gate (F5), and a non-ingest
`Doctor::run` site it was told not to relax. Each stop surfaced a real defect.

**Reviewers caught what implementers could not.** F2 (the vacuous guard test)
and the six-invariant problem were both found by review, not implementation —
in each case because the reviewer checked a claim the plan asserted rather than
accepting it.

**Plan defects outnumbered implementation defects.** Every significant problem
in this session originated in a specification written without verification, not
in code written from a specification.

---

## 8. Current state

### `main` — `e4f2263`

- 309 tests passing, 0 failing; `cargo xtask check` green
- 28 indexed extensions, 18 `ChunkKind` variants
- MCP instructions and tool descriptions state when to use each tool
- **14 commits unpushed.** `origin/main` is still at `dbe2ef6`
- **Carries a breaking change with no recovery and wrong recovery docs (F6)**

### `fix/fingerprint-recovery` — `71d3085`

- 4 commits ahead of `main`, not merged
- `2897794` and `9ca396d` are complete, reviewed, and worth keeping
- `hsum init --rebuild` is designed but not implemented
- A WIP stash holds the abandoned two-gate heal:
  `stash@{0}` "R3 WIP: two-gate heal, correct but incomplete"

### Superseded documents

Both carry explicit ⛔ headers so no agent executes them:

- `docs/superpowers/specs/2026-07-25-pipeline-fingerprint-recovery-design.md`
- `docs/superpowers/plans/2026-07-25-pipeline-fingerprint-recovery.md`
  (Tasks 1–2 landed and are kept; Tasks 3, 3b, 4 must not be implemented)

---

## 9. Outstanding work

**Immediate — before anything else**

1. Correct the recovery documentation on `main` (F6). It is wrong now, and the
   published alpha's users would follow it into a dead end.

**Next**

2. Implement `hsum init --rebuild` per
   `docs/superpowers/specs/2026-07-26-init-rebuild-design.md`. Needed
   regardless of launch timing — every future indexing-rules change requires it.
3. Decide whether to push `main` (14 commits) and whether the published alpha
   needs a note or withdrawal.

**Then — `TODOS.md` P0, largely unmet**

4. Clean-machine build matrix for macOS arm64 and Linux x86_64
5. Run the exact README CLI and MCP paths against the candidate artifact
6. Reproducible, signed artifacts; macOS notarization; verified no-`sudo`
   installer
7. Establish public repository owner, release operator, security-reporting
   channel, rollback procedure

**Deferred by explicit decision**

8. Per-source configurable extensions, with config formats as the headline case
9. Extensionless file support (`Makefile`, `Dockerfile`) — needs filename
   matching in both discovery gates
10. `chunk_kind_for_fingerprint` is a linear scan over `ChunkKind::ALL` per
    layout row during `doctor`. Fine at 18 variants; revisit if the list grows
11. `AGENTS.md` is untracked with no git history, currently holding a
    claude-mem context block. It is the natural home for tool-selection
    guidance

---

## 10. Was it ready to launch?

No. Three things were true before this session and remain true:

1. The pipeline fingerprint had no recovery path. Not introduced by this work —
   the code shipped that way. Any change to indexing rules would strand every
   user, and the product had no answer.
2. The documented recovery did not work (F6).
3. `TODOS.md` P0 is largely unmet, by the project's own account.

This session did not create the latent recovery flaw, but the intentional
fingerprint change activated it and therefore broke upgrade compatibility with
the published alpha. It also shipped real improvements alongside: 28
extensions instead of 10, an agent-facing reason to call hSUM at all, and an
error message that names the real problem instead of a false one.
