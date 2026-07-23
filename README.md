<p align="center">
  <img
    src="assets/brand/hSUMfinal.png"
    alt="Three classical robed figures rendered as a blue two-tone engraving against an etched pastoral background"
    width="100%"
  >
</p>

<h1 align="center">hSUM: Stateful Understanding &amp; Memory</h1>

<p align="center">
  <strong>Local evidence for agents.</strong>
</p>

<p align="center">
  Keep the source. Return the passage.
</p>

<p align="center">
  <a href="#license"><img alt="License: MIT OR Apache-2.0" src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue"></a>
  <a href="https://www.rust-lang.org/tools/install"><img alt="Language: Rust 1.91" src="https://img.shields.io/badge/rust-1.91-orange?logo=rust"></a>
  <a href="#connect-your-agent"><img alt="Interface: CLI and MCP" src="https://img.shields.io/badge/interface-CLI%20%2B%20MCP-8A63D2"></a>
  <a href="outputs/IMPLEMENTATION_STATUS.md"><img alt="Status: alpha, builds from source" src="https://img.shields.io/badge/status-alpha%20·%20builds%20from%20source-9cf"></a>
  <a href="#build-from-source"><img alt="Targets: macOS arm64 and Linux x86_64" src="https://img.shields.io/badge/targets-macOS%20arm64%20%7C%20Linux%20x86__64-informational"></a>
  <a href="#storage-and-quota-behavior"><img alt="Network: none after build" src="https://img.shields.io/badge/network-none%20after%20build-2ea44f"></a>
</p>

<!--
  Metrics badges (stars, downloads, crates.io version, CI) are intentionally
  omitted until they point at something real. Add them at public launch, e.g.:
  ![crates.io](https://img.shields.io/crates/v/hsum)
  ![CI](https://img.shields.io/github/actions/workflow/status/<org>/hsum/ci.yml)
  ![downloads](https://img.shields.io/crates/d/hsum)
  ![stars](https://img.shields.io/github/stars/<org>/hsum)
-->

<p align="center">
  <b>Install</b>
</p>

<p align="center">
  <a href="#quickstart"><img alt="Build from source" src="https://img.shields.io/badge/build%20from%20source-cargo%20build%20--release-2ea44f?logo=rust"></a>
  &nbsp;
  <img alt="cargo install: at v0.1.0" src="https://img.shields.io/badge/cargo%20install%20hsum-at%20v0.1.0-lightgrey">
  <img alt="prebuilt binaries: at v0.1.0" src="https://img.shields.io/badge/prebuilt%20binaries-at%20v0.1.0-lightgrey">
</p>

<p align="center">
  <sub>
    Colored badges are clickable and go to the relevant section or page.
    Source build works today (see <a href="#quickstart">Quickstart</a>).
    Published <code>cargo install</code>, <code>cargo binstall</code>, and
    signed macOS/Linux binaries land at the first tagged release; the two
    greyed badges mark those and stay unlinked until they exist.
  </sub>
</p>

<p align="center">
  <a href="#connect-your-agent">Connect an agent</a> &nbsp;·&nbsp;
  <a href="CONTRIBUTING.md">Contributing</a> &nbsp;·&nbsp;
  <a href="TODOS.md">Roadmap</a> &nbsp;·&nbsp;
  <a href="outputs/IMPLEMENTATION_STATUS.md">Status</a>
</p>

`hsum` is a local-first evidence engine for coding agents. It indexes one
local source tree into immutable SQLite generations, then returns exact and
BM25-ranked passages with canonical citations through a CLI or a read-only MCP
stdio server. No account, model, daemon, telemetry, or network access is
involved after the build.

When something leaves your agent's context window, you lose it. `hsum` keeps
the source below the session, maps deterministic routes back to it, and returns
the passage with its citation intact.

## Quickstart

```bash
# 1. Build (pinned Rust 1.91; macOS arm64 or Linux x86_64)
cargo build --locked --release
export HSUM=/absolute/path/to/checkout/target/release/hsum

# 2. Index a repository
cd /absolute/path/to/a/repository
"$HSUM" init            # prints the exact data path and a verified first query

# 3. Search, then resolve immutable evidence
"$HSUM" search 'your identifier or "exact phrase"'
"$HSUM" get '<citation printed by search>'

# 4. Connect an agent (privacy note prints first)
"$HSUM" client config codex      # or claude-code | claude-desktop | generic
"$HSUM" client doctor codex      # real end-to-end probe of that config
```

`init` refuses dangerous roots, writes nothing into the repository, stores
the index in your user directory, and prints a search command it ran itself to
confirm the command returns evidence. Every result carries an immutable
`hsum://v1/...` citation that `get` resolves against stored bytes, not against
whatever the file looks like later.

> [!NOTE]
> This is an alpha. Build from source today with the steps above; the checkout
> carries no machine-specific state, so it builds the same on any macOS arm64
> or Linux x86_64 box. Packaged releases (`cargo install`, signed binaries) and
> a first tag land next. Until then the crate keeps `publish = false`, so read
> the code and dependencies before you build.

## Connect your agent

hSUM speaks MCP over stdio only. `client config` prints a copyable snippet
that pins the binary's absolute path and one project-scoped trust binding. It
guesses no PATH, edits no config files for you, and cannot be steered into
another project:

```bash
"$HSUM" client config codex --format toml     # Codex
"$HSUM" client config claude-code             # Claude Code
"$HSUM" client config claude-desktop          # Claude Desktop
"$HSUM" client config generic                 # anything MCP-capable
```

The server exposes exactly four read-only tools: `evidence_search`,
`evidence_get`, `evidence_project`, and `evidence_status`. It marks every
returned passage `untrusted_content: true` so agents treat it as evidence,
never as instructions. Privacy boundary: hSUM uploads nothing anywhere; a
cloud-backed client may forward returned passages to its own model provider
under that client's policy, and the generated config prints that warning
before the snippet.

## Available in alpha.1

| Capability | Status | Current boundary |
|---|---|---|
| Local filesystem ingest | Available | One canonical root, one project, and one filesystem source per managed index |
| Markdown, text, and source code | Available | Lowercase `.md`, `.markdown`, `.txt`, `.rs`, `.py`, `.ts`, `.tsx`, `.js`, `.jsx`, and `.go` |
| Exact and BM25 search | Available | `auto` and `lexical` are equivalent; there are no embeddings or vectors |
| Immutable citations and historical `get` | Available | Evidence remains resolvable while its immutable version remains in this alpha index |
| Atomic generations | Available | Explicit ingest; no watcher or daemon |
| Status and read-only doctor | Available | Diagnosis only; no repair, prune, backup, or migration command |
| MCP stdio | Available | Four read-only tools bound to one trusted project |
| JSONL and live connectors | Unsupported | Planned after the filesystem slice |
| Semantic search, models, and reranking | Unsupported | No model install or download path exists |
| HTTP server or web UI | Unsupported | MCP stdio is the only transport |
| Prebuilt installation | Unavailable | Source build only; no release artifacts have been produced |

## Build from source

The repository pins Rust `1.91.0` with `rustfmt` and Clippy. The implementation
uses Unix filesystem and locking primitives and is intended for macOS and
Linux on local storage. No clean-machine release matrix has run, so this is not
yet a support claim for either platform. Windows is unsupported in alpha.1.

```bash
git clone <your-reviewed-copy-of-this-repository>
cd <repository>
cargo build --locked --release
./target/release/hsum --version
```

The first build may contact Rust and crate registries to obtain the pinned
toolchain and dependencies. After a binary exists, alpha.1 has no network
client, account, telemetry, model download, HTTP server, or other network
runtime path. An already populated Cargo cache can build with Cargo's normal
`--offline` option.

For the examples below, set `HSUM` to the absolute path of the binary before
changing directories:

```bash
export HSUM=/absolute/path/to/checkout/target/release/hsum
```

## First local evidence

Run the dry run first. It scans and estimates the source but writes no index,
trust binding, or repository pointer.

```bash
cd /absolute/path/to/a/repository
"$HSUM" init --dry-run
"$HSUM" init
"$HSUM" search '"HTTPServer::handle_request"' --explain
```

`init` chooses the enclosing Git root, or the current directory when there is
no enclosing Git worktree. It creates a private managed SQLite index and a
user-side trust binding. It does **not** modify the repository unless
`--write-pointer` is supplied. The optional `.hsum.toml` pointer is only a
logical hint; it never grants authority without a matching user trust binding.

Useful initialization controls:

```bash
"$HSUM" init --no-ingest
"$HSUM" init --index my-index --project default
"$HSUM" init --write-pointer
"$HSUM" init --index-quota-bytes 1073741824
```

- `--no-ingest` creates and trusts the index while leaving the source in
  `never_succeeded` state.
- `--allow-broad-root` is required for `/`, the account home, or a directory
  above the enclosing Git worktree.
- `--allow-large-source` raises the persisted source ceiling from 10,000 files
  or 2 GiB to the hard ceiling of 50,000 files or 8 GiB.
- `--force-pointer` is valid only with `--write-pointer` and replaces
  conflicting pointer bytes after preflight.
- Re-running `init` for the same trusted root is idempotent only when the stored
  index, project, source identity, and source root still match.

## Command inventory

Alpha.1 exposes only the following command graph:

| Command | Output and effect |
|---|---|
| `hsum init [PATH]` | Dry-run or create one managed filesystem index, project, source, and user trust binding |
| `hsum trust PATH --confirm` | Add or confirm a root-bound user trust binding for an existing matching index |
| `hsum ingest` | Dry-run or commit one authoritative filesystem generation |
| `hsum search QUERY` | Human evidence or one JSON document; exact plus BM25 |
| `hsum get CITATION_URI` | Human evidence or one JSON document from an immutable citation |
| `hsum status` | Human status/problems or one JSON document |
| `hsum doctor` | Read-only human integrity diagnosis |
| `hsum context` | Human selection details or one JSON document |
| `hsum client config CLIENT` | Copyable JSON or TOML for `codex`, `claude-code`, `claude-desktop`, or `generic` |
| `hsum client doctor CLIENT` | Validate the generated configuration with a real empty-directory MCP probe |
| `hsum help error SUBCODE` | Offline explanation of one stable public error subcode |
| `hsum completions SHELL` | Completion text on stdout for `bash`, `zsh`, `fish`, `powershell`, or `elvish` |
| `hsum man` | A generated `hsum(1)` page on stdout |
| `hsum mcp` | Project-bound MCP over stdin/stdout |

The only `help` subcommand is the offline error catalog,
`hsum help error <SUBCODE>`; for command help use `hsum --help` or
`hsum <command> --help`. Alpha.1 has no `model`, `backup`, `prune`, `forget`,
`restore`, `repair`, `watch`, or daemon command.

## Source discovery and ingest

Alpha.1 accepts valid UTF-8 text without NUL bytes. Lowercase filename
extensions are matched exactly. The per-file default is 2 MiB and remains
2 MiB when `--allow-large-source` is used.

Discovery stays conservative by default:

- nested `.gitignore` rules are honored;
- hidden names and `target`, `node_modules`, `build`, `dist`, `out`, `vendor`,
  `__pycache__`, and `venv` are excluded by default;
- `.git`, `.ssh`, `.aws`, `.gnupg`, `.config`, `.cache`, `.idea`, `.vscode`,
  environment files, common credential names, and private-key extensions are
  treated as sensitive and are not admitted by the alpha.1 CLI;
- source roots, directories, and files are opened without following symlinks;
- hard links are ordinary regular files and cannot be distinguished from their
  original paths, so secret-path rules are defense in depth rather than a
  content sandbox;
- file identity and metadata are checked around reads, and directory changes
  during enumeration fail closed;
- traversal is capped at 100,000 visited entries, 10,000 directories, depth
  128, and 50,000 entries in one directory.

Files that are unreadable, invalid UTF-8, contain NUL, exceed the file limit,
or change during the read become source failures rather than partially indexed
bodies. Inspect a refresh before committing it:

```bash
"$HSUM" ingest --dry-run
"$HSUM" ingest
"$HSUM" status --problems
```

A refresh runs as two bounded passes. The first pass only enumerates and
estimates the eligible source without writing anything; a storage preflight
then checks that estimate against free capacity. The second pass re-verifies
each file's identity while spooling bodies into a bounded staging area before
the commit transaction. Because the prior generation stays fully readable until
the new one activates, managed storage transiently peaks above its steady
state during a refresh; the preflight reserve accounts for that peak.

Each successful authoritative refresh activates one atomic generation.
Unchanged bodies and immutable historical versions are reused. If every
targeted file fails, no generation or index epoch advances and the command
exits `1`. A committed generation that also retains file failures exits `6`
and reports a partial source. Prior evidence is carried forward where the
failure contract permits it.

Deletion has two independent confirmations:

- replacing a previously nonempty source with an empty snapshot requires
  `--allow-empty-snapshot`;
- deleting more than 25% of prior active documents, or more than 1,000
  documents, requires `--allow-mass-delete`.

Exactly 25% is allowed without the mass-delete confirmation. Use
`--lock-timeout-ms 0..60000` to bound the wait for the single writer; the
default is 5,000 ms.

## Search and citations

Alpha.1 combines three local lexical signals:

1. case-sensitive identifier-like literals;
2. conservative exact quoted spans;
3. SQLite FTS5 BM25.

The candidate lists are fused deterministically. `--explain` includes the
signal ranks and fixed-point fusion score. `--mode auto` and
`--mode lexical` produce the same alpha.1 behavior.

```bash
"$HSUM" search 'generation recovery'
"$HSUM" search '"EVIDENCE_FORGOTTEN"' --limit 20 --timeout-ms 3000 --explain
"$HSUM" search 'source_state' --json
```

Search limits are 1–50 results, the deadline is 100–10,000 ms, and the defaults
are 10 results and 3,000 ms. A query is at most 4,096 UTF-8 bytes. Double quotes
delimit exact spans and have no escape syntax. The CLI exposes `--cursor` in
the pre-stable grammar but rejects it in alpha.1; cursor pagination is MCP-only.

Every result includes a canonical citation:

```text
hsum://v1/<index-id>/<source-id>/<document-id>?rev=<sha256>#bytes=<start>-<end>
```

The URI fixes the index, source, document, immutable revision, and exact byte
span. `get` resolves stored evidence, not whatever happens to be in the source
tree now:

```bash
"$HSUM" get '<citation>'
"$HSUM" get '<citation>' --max-bytes 65536 --verify-source-hash --json
```

`get` defaults to 16 KiB and allows 1–64 KiB. It may return adjacent chunks
within that bound and reports both the requested and returned citation/span.
`--verify-source-hash` performs a bounded live-source content check and returns
`unchanged`, `changed`, `missing`, `blocked`, or `unverifiable`. Normal search
uses a bounded metadata probe and labels each result
`metadata_unchanged`, `changed_since_ingest`, `missing_since_ingest`, or
`unverifiable`.

All indexed passages are emitted with `untrusted_content: true`. A caller must
treat a passage as evidence, never as executable instruction.

## Status, context, and doctor

```bash
"$HSUM" status
"$HSUM" status --problems
"$HSUM" status --json
"$HSUM" context --json
"$HSUM" doctor
```

`status` reports generation/epoch, active document and passage counts,
per-source success or failure state, persistent quota, managed SQLite/WAL
bytes, available capacity, recovery reserve, filesystem locality, and
actionable problem codes. `context` shows the effective index, project, source
root, trust binding, selection origin, managed paths, and persisted quota.

`doctor` is read-only. It validates the SQLite application ID, exact schema
manifest and checksum, migration chain, pipeline fingerprint, WAL mode,
integrity and foreign keys, committed generation history, immutable content
digests and chunk layouts, and active FTS/literal index parity. It does not
repair data.

## Storage and quota behavior

The managed index uses bundled SQLite in WAL mode with full synchronous
commits. Initialization syncs the created database and its parent directory
where the platform supports that durability operation. Generations switch
active heads inside one SQLite transaction.

The managed index directory is private by construction. Its parent is created
and required to be user-only (`0700`), and the database plus its WAL/SHM
sidecars must be user-only (`0600`), single-link regular files owned by the
current user. Every open walks the path component by component without
following symlinks (only the fixed macOS `/var` and `/tmp` system aliases are
accepted), revalidates the directory namespace against the held descriptors,
and confirms through SQLite itself that the open handle still names the
managed database file before any statement runs. A tampered, swapped, or
relocated database therefore fails closed instead of being read or repaired.

Before initialization and each ingest, hSUM estimates new managed writes and
requires free capacity for that estimate plus a recovery reserve of the larger
of 64 MiB or 10% of current managed SQLite/WAL bytes. An optional
`--index-quota-bytes` is stored with the source and enforced on later ingests.
Unknown estimates, arithmetic overflow, insufficient capacity, and exhausted
quota fail before generation mutation.

Known network filesystems such as NFS and SMB/CIFS are refused for managed
index storage. If locality cannot be proven, hSUM proceeds with a warning and a
status problem. Recognized iCloud Drive, Dropbox, OneDrive, Google Drive, and
Box Drive roots are reported as unsupported sync storage; the current
implementation warns rather than silently treating them as local. Put managed
data on a local filesystem with an absolute `--data-dir`.

Use `HSUM_HOME=/absolute/path` to place config, data, and cache below one root.
`--data-dir` and `--cache-dir` are per-invocation absolute overrides. Run
`hsum context` to see the exact resolved paths instead of assuming
platform-specific defaults.

## Trust and selection

The user-side trust registry binds all of these values together:

- canonical repository root;
- random UUIDv4 binding ID;
- index name and immutable index ID;
- project name and immutable project ID.

The registry is strict, bounded, private user configuration. A copied
`.hsum.toml` pointer cannot authorize another root, and a copied or tampered
database must still pass identity, schema, source-cardinality, source-kind, and
source-root checks.

Direct CLI selection order is:

1. complete `HSUM_INDEX` and `HSUM_PROJECT` environment values;
2. the trust binding for the current canonical root;
3. the strict user configuration default;
4. a repository pointer, which remains only a hint and fails without trust.

Use `hsum context` before mutation when selection is uncertain. To bind an
existing matching managed index to a root:

```bash
"$HSUM" trust /absolute/path/to/repository --index my-index --project default --confirm
```

MCP ignores environment and configured defaults. It starts only from an
explicit trusted binding or a trusted current root. `--binding` and `--project`
are mutually exclusive; a project name is resolved only inside an already
trusted root context.

## MCP reference

The onboarding path is in [Connect your agent](#connect-your-agent) above;
this section is the transport reference. Generated configuration goes to
stdout only; the privacy warning prints to stderr so the snippet stays
copyable.

Generated configuration pins the binary's absolute path and one trust binding.
It does not edit client configuration files. `client doctor` re-parses the
exact configuration it would print, then launches that absolute executable
under the same conditions a GUI client provides: an empty working directory
with the inherited `HSUM_INDEX`/`HSUM_PROJECT` selection stripped. It
negotiates a real stdio MCP session, calls `tools/list` and `evidence_status`
against the bound project, bounds the whole probe with a deadline, and fails if
the probe process writes anything into that working directory.

The server can also be started directly:

```bash
"$HSUM" mcp --binding <uuid-v4>
```

It advertises exactly four `hsum.api.v1` tools:

| Tool | Purpose |
|---|---|
| `evidence_search` | Search the bound project; supports opaque, query- and snapshot-bound cursors |
| `evidence_get` | Resolve one canonical citation and optionally verify the live source hash |
| `evidence_project` | Describe the one project and its filesystem source |
| `evidence_status` | Return read-only project counts, source health, and shared index problems |

Requests reject unknown fields. The transport caps each input frame at 1 MiB,
nesting at 32 levels, each structured response at 512 KiB, search passage
bodies at 256 KiB per page, and `get` bodies at 64 KiB. At most four blocking
tool requests run concurrently. Cancellation interrupts admitted SQLite work.
MCP stdout contains protocol frames only.

## Output and errors

`search`, `get`, `status`, and `context` support `--json`. A successful JSON
command writes exactly one `hsum.api.v1` document to stdout. Human output
escapes terminal controls, bidirectional overrides, invalid path bytes, and
other nonprinting source text.

Public failures share one catalog across human CLI, CLI JSON, and MCP. The
structured form contains:

```json
{
  "code": "INVALID_ARGUMENT",
  "subcode": "CITATION_MALFORMED",
  "message": "the evidence citation is invalid",
  "retryable": false,
  "details": {},
  "next_action": "copy the complete citation from hsum search or evidence_search",
  "docs_url": "https://hsum.burkankale.com/docs/0.1.0-alpha.1/errors/CITATION_MALFORMED",
  "request_id": "<uuid>"
}
```

Errors go to stderr and leave stdout empty. Human errors render four labeled
lines: problem, cause, fix, and learn. The learn line always names both the
offline command `hsum help error <SUBCODE>` and the version-pinned
documentation URL. That offline catalog explains every public subcode with its
category, retryability, cause, fix, and an example, no browser or network
needed. The public documentation site is not yet published, so preserve the
request ID when you report a failure.

Stable alpha.1 process exits:

| Exit | Meaning |
|---:|---|
| `0` | Success |
| `1` | Failure, not found, stale cursor, source-invalid, or internal error |
| `2` | Invalid arguments or confirmation required |
| `3` | Dependency/model unavailable; reserved by the shared catalog |
| `4` | Busy writer or timeout |
| `5` | Corrupt, incompatible, or integrity failure |
| `6` | A generation committed with a partial ingest |
| `7` | Permission or resource failure, including disk/quota exhaustion |
| `130` | Interrupted or cancelled |

## Contributor checks

The repository's local check command runs formatting, Clippy with warnings
denied, all targets/features tests, and doctests:

```bash
cargo xtask check
```

The equivalent individual commands are:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo test --doc --all-features
```

These checks are contributor evidence, not a substitute for the still-missing
clean-machine platform, signing, installer, benchmark, and release gates. See
[`outputs/IMPLEMENTATION_STATUS.md`](outputs/IMPLEMENTATION_STATUS.md) for a
source-to-test evidence map and [`TODOS.md`](TODOS.md) for the remaining work.

## License

Made by the creator of ArgonGate, Burkan Alp Kale.

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you state otherwise, any contribution you submit is
dual licensed as above. Brand image provenance is recorded in
[`assets/brand/PROVENANCE.md`](assets/brand/PROVENANCE.md).

---

<p align="center">
  <em>The source is not the answer. It is what an answer must survive.</em>
</p>
