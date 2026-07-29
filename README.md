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
  <a href="outputs/IMPLEMENTATION_STATUS.md"><img alt="Status: alpha prerelease" src="https://img.shields.io/badge/status-alpha%20prerelease-9cf"></a>
  <a href="#build-from-source"><img alt="Targets: macOS arm64 and Linux x86_64" src="https://img.shields.io/badge/targets-macOS%20arm64%20%7C%20Linux%20x86__64-informational"></a>
  <a href="#storage-and-quota-behavior"><img alt="Network: none after build" src="https://img.shields.io/badge/network-none%20after%20build-2ea44f"></a>
</p>

<!--
  Registry metrics are intentionally omitted while publish=false. Add them
  only if a future release is actually published to crates.io, e.g.:
  ![crates.io](https://img.shields.io/crates/v/hsum)
  ![downloads](https://img.shields.io/crates/d/hsum)
-->

<p align="center">
  <b>Install</b>
</p>

<p align="center">
  <a href="https://github.com/burkan2/hSUM/releases"><img alt="Verified user installer available" src="https://img.shields.io/badge/user%20installer-available-2ea44f?logo=github"></a>
  &nbsp;
  <a href="#quickstart"><img alt="Build from source" src="https://img.shields.io/badge/build%20from%20source-cargo%20build%20--release-2ea44f?logo=rust"></a>
  &nbsp;
  <img alt="cargo install: at v0.1.0" src="https://img.shields.io/badge/cargo%20install%20hsum-at%20v0.1.0-lightgrey">
  <a href="https://github.com/burkan2/hSUM/releases"><img alt="GitHub alpha archives available" src="https://img.shields.io/badge/GitHub%20alpha%20archives-available-2ea44f?logo=github"></a>
</p>

<p align="center">
  <sub>
    Colored badges are clickable and go to the relevant section or page.
    GitHub prereleases provide a checksum-verifying, no-<code>sudo</code>
    installer plus prebuilt macOS arm64 and Linux x86_64 archives.
    Published <code>cargo install</code> and <code>cargo binstall</code> remain
    deferred while the crate has <code>publish = false</code>.
    Alpha binaries are checksummed and carry GitHub build attestations, but
    the macOS archive is <b>not</b> Apple-signed or notarized — see
    <a href="#verifying-a-release">Verifying a release</a>.
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
# 1. Download and verify the version-pinned installer
version=0.1.0-alpha.4
base_url="https://github.com/burkan2/hSUM/releases/download/v$version"
curl -fsSLO "$base_url/install-hsum.sh"
curl -fsSLO "$base_url/SHA256SUMS"
grep ' install-hsum.sh$' SHA256SUMS | shasum -a 256 -c -

# 2. Install hSUM, register Codex, and activate this repository
cd /absolute/path/to/a/repository
bash /path/to/install-hsum.sh --activate . --confirm
# macOS alpha only: add --allow-unnotarized-alpha after reading the warning.

# 3. Search, then resolve immutable evidence
hsum search 'your identifier or "exact phrase"'
hsum get '<citation printed by search>'

# 4. Verify the effective Codex registration at any time
hsum integration status codex --json
```

### Ask your agent

Copy this prompt into your coding agent to run the quickstart from an existing
checkout:

```text
Install and smoke-test hSUM 0.1.0-alpha.4
(https://github.com/burkan2/hSUM) for the current Git repository. User-level
installation, user-level hSUM state, and the single Codex MCP registration are
in scope; do not modify repository files. Confirm this is macOS arm64 or Linux
x86_64. Download `install-hsum.sh` and `SHA256SUMS` from the exact
`v0.1.0-alpha.4` GitHub release, verify the installer's SHA-256 entry before
running it, and review the script. Run it with `--activate . --confirm`; on
macOS, explain that this alpha is not Apple-notarized and add
`--allow-unnotarized-alpha` only after I explicitly approve that required
acknowledgement. Verify that installation reports four read-only tools and a
successful citation round trip, and that `hsum integration status codex
--json` reports both the registration and agent policy as `current`. Do not
ask me to copy TOML, edit Codex configuration, or restart Codex. Report each
command and result, including any failure. Documentation is at
https://hsum.burkankale.com/docs/0.1.0-alpha.4/ and
https://hsum.burkankale.com/llms.txt.
```

`init` refuses dangerous roots, writes nothing into the repository, stores
the index in your user directory, and prints a search command it ran itself to
confirm the command returns evidence. Every result carries an immutable
`hsum://v1/...` citation that `get` resolves against stored bytes, not against
whatever the file looks like later.

> [!NOTE]
> This is an alpha. The
> [`v0.1.0-alpha.4` GitHub prerelease](https://github.com/burkan2/hSUM/releases/tag/v0.1.0-alpha.4)
> provides checksummed artifacts for macOS arm64 and Linux x86_64. The crate
> remains `publish = false`, so `cargo install` is unavailable.

## Connect your agent

hSUM speaks MCP over stdio only. Codex uses one user-wide registration whose
server derives an exact trusted repository from each task's working directory:

```bash
"$HSUM" integration install codex --activate . --confirm
"$HSUM" integration status codex
```

The install command uses Codex's supported `mcp add/get` interface, reads the
effective `hsum` entry back, launches that exact MCP command from the activated
repository, checks all four tools, runs `evidence_status`, and completes an
`evidence_search` → `evidence_get` citation round trip when the index is
nonempty. It also installs a bounded hSUM-managed block in Codex's user-global
`AGENTS.md` (or the effective `AGENTS.override.md`) so future agents know when
to reach for hSUM. Existing instructions are preserved, modified/duplicate
managed markers are refused, and `--agent-policy skip` is available as an
explicit escape hatch. The whole flow is idempotent.

Every later Codex task in the same repository gets hSUM automatically. In a
different repository the tools are still present; the first attempted use
returns `REPOSITORY_NOT_ACTIVATED` with the canonical root, privacy consequence,
and exact activation argv. After one repository-specific consent, the agent
runs that command and retries the same tool without restarting MCP. Repositories
retain separate indexes, trust bindings, generations, and citation namespaces.

For users who keep many repositories under one projects directory, authorize
that parent once:

```bash
"$HSUM" integration authorize-workspace codex --path ~/Projects --confirm
```

hSUM does not scan `~/Projects` recursively or combine it into one corpus.
When a Codex task first calls hSUM from a Git repository below the authorized
directory, only that exact repository is initialized as a separate index. Home,
filesystem-root, overly broad, and Git-repository workspace scopes are refused.
Use `integration revoke-workspace` to stop future lazy enrollments; existing
bindings and indexes remain intact.

Repositories activated through the integration attempt one refresh before the
first `evidence_search` or `evidence_get` in each MCP task. A successful refresh
searches the newest committed generation. Busy, unsafe, partial, or
mass-deletion-refused refreshes keep the last good generation and expose the
freshness state in tool output. Manual/low-level hSUM repositories retain
explicit ingest behavior.

`client config` remains the low-level/manual escape hatch. Its Codex form is
workspace-dynamic; other clients remain binding-pinned:

```bash
"$HSUM" client config codex --format toml
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

## Use hSUM with agent memory and company-brain tools

hSUM can run beside claude-mem and other agent-memory or company-brain
systems. Each MCP server keeps its own registration and storage. hSUM claims
the `hsum` server name and adds one checksummed block to Codex's user
instructions. The installer preserves other MCP entries and instruction text.
If another command owns the `hsum` name, hSUM stops installation and reports
the conflict.

Use each system for the material it stores:

| Need | Tool |
|---|---|
| Current repository structure, implementation, or cited evidence | hSUM `evidence_search` and `evidence_get` |
| Current contents of a file you are editing | An ordinary file read |
| Past sessions, decisions, and unfinished work | Your agent-memory system |
| Knowledge shared across repositories or teams | Your company-brain system |

hSUM and each memory tool keep separate indexes and databases. hSUM does not
read another tool's SQLite database, vector store, worker, hooks, or cloud API.
In alpha.4, hSUM indexes supported files inside an authorized repository. If a
memory tool writes Markdown or text files into that repository, hSUM handles
them as repository files.

A memory system may record hSUM tool calls and summarize returned passages.
Keep the `hsum://v1` citation when you need exact evidence. If a memory summary
conflicts with the current repository, read the file and call
`evidence_get` with source-hash verification.

Installing or uninstalling hSUM leaves other MCP registrations and unmanaged
agent instructions in place. hSUM sends no corpus data to a model or service;
check the memory system's policy for its storage, embeddings, and compression
calls.

## Available in 0.1.0-alpha.4

| Capability | Status | Current boundary |
|---|---|---|
| Local filesystem ingest | Available | One canonical root, one project, and one filesystem source per managed index |
| Markdown, text, and source code | Available | Lowercase `.md`, `.markdown`, `.txt`, `.rs`, `.py`, `.ts`, `.tsx`, `.js`, `.jsx`, `.go`, `.java`, `.kt`, `.kts`, `.c`, `.h`, `.cpp`, `.hpp`, `.cc`, `.hh`, `.cxx`, `.rb`, `.cs`, `.swift`, `.php`, `.scala`, `.sh`, `.bash`, and `.sql` |
| Exact and BM25 search | Available | `auto` and `lexical` are equivalent; there are no embeddings or vectors |
| Immutable citations and historical `get` | Available | Evidence remains resolvable while its immutable version remains in this alpha index |
| Atomic generations | Available | Explicit ingest; no watcher or daemon |
| Status and read-only doctor | Available | Diagnosis only; no repair, prune, backup, or migration command |
| MCP stdio | Available | One global Codex registration; every server process pins one exact trusted repository |
| JSONL and live connectors | Unsupported | Planned after the filesystem slice |
| Semantic search, models, and reranking | Unsupported | No model install or download path exists |
| HTTP server or web UI | Unsupported | MCP stdio is the only transport |
| Prebuilt installation | Available | Checksum-verifying no-`sudo` installer and archives for macOS arm64 and Linux x86_64; no crates.io package |

## Build from source

The repository pins Rust `1.91.0` with `rustfmt` and Clippy. Release CI runs on
clean GitHub-hosted macOS arm64 and Linux x86_64 machines. The implementation
uses Unix filesystem and locking primitives on local storage; Windows is
unsupported.

```bash
git clone <your-reviewed-copy-of-this-repository>
cd <repository>
cargo build --locked --release
./target/release/hsum --version
```

### Ask your agent

Copy this prompt into your coding agent to install hSUM from source:

```text
Install hSUM (https://github.com/burkan2/hSUM) from this reviewed source
checkout without changing application source or configuration files. First
confirm that the host is macOS arm64 or Linux x86_64 and check
`rustc --version` and `cargo --version`. If Rust/Cargo is missing, offer me two
choices and wait for my answer: I can install Rust from
https://www.rust-lang.org/tools/install, or you can install it with rustup
after I explicitly approve that system change. Once Cargo is available, run
`cargo build --locked --release` and `./target/release/hsum --version`. Then
give me the exact `export HSUM=...` command using this checkout's absolute path
and report the commands and results without making any other changes.
Documentation is at https://hsum.burkankale.com/docs/0.1.0-alpha.4/ and
https://hsum.burkankale.com/llms.txt.
```

The first build may contact Rust and crate registries to obtain the pinned
toolchain and dependencies. After a binary exists, hSUM has no network
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

The current candidate exposes only the following command graph:

| Command | Output and effect |
|---|---|
| `hsum init [PATH]` | Dry-run or create one managed filesystem index, project, source, and user trust binding; `--rebuild` safely replaces the binding and index |
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
`hsum <command> --help`. The current candidate has no `model`, `backup`,
`prune`, `forget`,
`restore`, `repair`, `watch`, or daemon command.

## Source discovery and ingest

The current candidate accepts valid UTF-8 text without NUL bytes. Lowercase
filename extensions are matched exactly. The per-file default is 2 MiB and
remains 2 MiB when `--allow-large-source` is used.

Source files chunk at declaration boundaries where the language has them —
`fn`, `class`, `def`, `func`, and their equivalents. Shell and SQL have no
unambiguous declaration keyword, so they chunk at paragraph and line
boundaries instead. Configuration formats such as `.yaml`, `.json`, and
`.toml`, markup such as `.html` and `.css`, and extensionless files such as
`Makefile` are not indexed.

Discovery stays conservative by default:

- nested `.gitignore` rules are honored;
- hidden names and `target`, `node_modules`, `build`, `dist`, `out`, `vendor`,
  `__pycache__`, and `venv` are excluded by default;
- `.git`, `.ssh`, `.aws`, `.gnupg`, `.config`, `.cache`, `.idea`, `.vscode`,
  environment files, common credential names, and private-key extensions are
  treated as sensitive and are not admitted by the CLI;
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

### When the indexing pipeline changes

The set of indexable extensions and the chunking rules are hashed into a
pipeline fingerprint that is stored in the index. When a new hSUM build changes
those rules, the stored fingerprint no longer matches and the index stops
opening: every command, including `search` and `ingest`, fails with
`pipeline fingerprint does not match this binary`.

`ingest` cannot recover such an index because it must open the index before it
can rebuild it. Inspect the destructive replacement first, then run it:

```bash
"$HSUM" init --rebuild --dry-run
"$HSUM" init --rebuild
```

Rebuild validates that the old index is structurally coherent apart from its
pipeline fingerprint, preserves other trusted roots, and creates a new index
and binding from the current source. It refuses corruption rather than
silently treating damage as compatibility. Evidence and citations from the
replaced index do not survive.

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

The current candidate combines three local lexical signals:

1. case-sensitive identifier-like literals;
2. conservative exact quoted spans;
3. SQLite FTS5 BM25.

The candidate lists are fused deterministically. `--explain` includes the
signal ranks and fixed-point fusion score. `--mode auto` and
`--mode lexical` produce the same behavior.

```bash
"$HSUM" search 'generation recovery'
"$HSUM" search '"EVIDENCE_FORGOTTEN"' --limit 20 --timeout-ms 3000 --explain
"$HSUM" search 'source_state' --json
```

Search limits are 1–50 results, the deadline is 100–10,000 ms, and the defaults
are 10 results and 3,000 ms. A query is at most 4,096 UTF-8 bytes. Double quotes
delimit exact spans and have no escape syntax. The CLI exposes `--cursor` in
the pre-stable grammar but rejects it; cursor pagination is MCP-only.

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
  "request_id": "<uuid>"
}
```

Errors go to stderr and leave stdout empty. Human errors render four labeled
lines: problem, cause, fix, and learn. The learn line names the offline command
`hsum help error <SUBCODE>` and the stable error code. That offline catalog
explains every public subcode with its category, retryability, cause, fix, and
an example, no browser or network needed. Preserve the request ID when you
report a failure.

Stable prerelease process exits:

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

These checks are contributor evidence, not a substitute for the clean-machine
release workflow, artifact verification, documentation, and signed-tag gates.
Apple notarization and benchmark claims remain explicitly deferred. See
[`outputs/IMPLEMENTATION_STATUS.md`](outputs/IMPLEMENTATION_STATUS.md) for a
source-to-test evidence map and [`TODOS.md`](TODOS.md) for the remaining work.

## Verifying a release

The `v0.1.0-alpha.4` archives come from a GPG-signed tag and carry a
`SHA256SUMS` manifest plus a GitHub SBOM attestation linking each archive to the
workflow run and commit that produced it.

```bash
# macOS example; use sha256sum -c on Linux.
asset=hsum-v0.1.0-alpha.4-aarch64-apple-darwin.zip
shasum -a 256 -c "$asset.sha256"
gh attestation verify "$asset" --repo burkan2/hSUM \
  --predicate-type https://spdx.dev/Document/v2.3
```

The attestation is an SPDX SBOM attestation, so `--predicate-type` is required;
without it `gh` looks for a provenance predicate and reports `no matching
predicate found`. A successful verification prints nothing and exits `0`. Add
`--format json` to inspect the signing workflow, tag, and commit.

The Syft SBOMs are component inventories, but their Rust package
license fields are `NOASSERTION`. The repository therefore records a separate,
deterministic
[`Cargo`-declared license inventory](outputs/hsum-v0.1.0-alpha.4-cargo-licenses.json);
the release workflow attaches that inventory and includes it in `SHA256SUMS`.
It is review evidence, not a legal conclusion.

**The macOS archive is not Apple-signed or notarized in alpha.** hSUM has no
Apple Developer account, so no Developer ID certificate exists to sign with.
Gatekeeper will refuse the binary on first launch. After verifying the checksum
and attestation above, clear the quarantine attribute yourself:

```bash
xattr -d com.apple.quarantine hsum
```

Only do that once the checksum and attestation both pass. Those two checks are
what establish the binary's provenance in alpha; the quarantine flag is macOS
asking for a trusted Developer ID signature and notarization hSUM cannot yet
provide, not an independent verdict on the file. Apple signing and notarization
are tracked for a later release.

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
