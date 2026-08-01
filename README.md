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

Run this from the Git repository you want hSUM to index:

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

### Install from terminal

> [!WARNING]
> The macOS alpha is not Apple-notarized. The one-command install below includes
> `--allow-unnotarized-alpha` as the explicit acknowledgement required to run
> that binary. The flag has no effect on Linux. Remove it if you want the
> installer to stop at the macOS warning instead.

```bash
curl --proto '=https' --tlsv1.2 -fsSL https://github.com/burkan2/hSUM/releases/download/v0.1.0-alpha.4/install-hsum.sh | bash -s -- --activate . --confirm --allow-unnotarized-alpha
```

This pinned command installs into `XDG_BIN_HOME` or `~/.local/bin`, verifies the
downloaded hSUM binary checksum, registers the Codex MCP server, activates the
current repository, and runs the integration smoke test. It trusts GitHub's
HTTPS delivery for the installer itself; use the verified path below if you
want to check the installer bytes before execution.

```bash
# Search, then resolve immutable evidence
hsum search 'your identifier or "exact phrase"'
hsum get '<citation printed by search>'

# Verify the effective Codex registration at any time
hsum integration status codex --json
```

### Verify the installer before running

This longer path downloads the version-pinned installer and release checksums
into a temporary directory, verifies the installer, then runs the same install:

```bash
(
set -eu
set -o pipefail
version=0.1.0-alpha.4
release="https://github.com/burkan2/hSUM/releases/download/v$version"
install_tmp="$(mktemp -d)"
trap 'rm -rf "$install_tmp"' EXIT
curl --proto '=https' --tlsv1.2 -fsSL "$release/install-hsum.sh" \
  -o "$install_tmp/install-hsum.sh"
curl --proto '=https' --tlsv1.2 -fsSL "$release/SHA256SUMS" \
  -o "$install_tmp/SHA256SUMS"
(
  cd "$install_tmp"
  grep ' install-hsum.sh$' SHA256SUMS | shasum -a 256 -c -
)
bash "$install_tmp/install-hsum.sh" \
  --activate . --confirm --allow-unnotarized-alpha
)
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

Activate each additional repository explicitly:

```bash
"$HSUM" integration activate codex --path ~/Projects/my-repository --confirm
```

hSUM does not scan a projects parent, combine repositories into one corpus, or
initialize a repository from an MCP tool call. Alpha.4 workspace-authorization
records remain readable for compatibility, but the current read-only server
does not consume them for lazy enrollment.

MCP searches the last explicitly committed generation. Run `hsum ingest` from
the activated repository when you want to index filesystem changes. Search and
get still probe the live source and report `source_state`, so a changed or
missing file is visible without silently changing the cited stored bytes.

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
memory tool writes supported Markdown or text files into that repository, hSUM
includes them in the corpus. hSUM may return that content to the MCP client,
which may forward it to a model provider under the client's policy.

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
| Local filesystem ingest | Available | Register roots explicitly; one active filesystem authority per project; registration and root replacement do not ingest implicitly |
| Named projects | Available in the current checkout; unreleased | Create, list, persistently select, and replace the filesystem root |
| Markdown, text, and source code | Available | Lowercase `.md`, `.markdown`, `.txt`, `.rs`, `.py`, `.ts`, `.tsx`, `.js`, `.jsx`, `.go`, `.java`, `.kt`, `.kts`, `.c`, `.h`, `.cpp`, `.hpp`, `.cc`, `.hh`, `.cxx`, `.rb`, `.cs`, `.swift`, `.php`, `.scala`, `.sh`, `.bash`, and `.sql` |
| Exact and BM25 search | Available | `auto` and `lexical` are equivalent; there are no embeddings or vectors |
| Immutable citations and historical `get` | Available | Evidence remains resolvable while its immutable version remains in this alpha index |
| Atomic generations | Available | Explicit mixed-source ingest; no watcher or daemon |
| Status and Doctor | Full diagnosis available; bounded repair/report in the current checkout, unreleased | Repair removes abandoned generation rows only; support reports are body-free and query-free |
| Guarded maintenance | Available in the current checkout; unreleased | Verified backup, index/config migration plan/apply, prune, durable forget, and exact-state restore ceremonies |
| Managed backup disposition | Available in the current checkout; unreleased | Inventory every hSUM-created index backup; forget requires an explicit keep-or-purge choice |
| Confirmed index deletion | Available in the current checkout; unreleased | Exact logical name plus `--confirm`; clears configured authority, fences readers/writers, and removes only the named managed index directory |
| Pinned model artifacts | Available in the current checkout; unreleased | Explicit HTTPS install or air-gapped import; exact manifest, revision, file-list, byte-size, SHA-256, dimension, and license verification |
| MCP stdio | Available | One global Codex registration; every server process pins one exact trusted project scope |
| JSONL snapshot sources | Available in the current checkout; unreleased | Add, list, attach, detach, ingest, and globally remove snapshots |
| Live connectors | Unsupported | Planned after snapshot-source lifecycle work |
| Semantic search, vectors, and reranking | Unsupported | Artifact management does not activate inference or add semantic/hybrid search modes |
| HTTP server or web UI | Unsupported | MCP stdio is the only transport |
| Prebuilt installation | Available | Checksum-verifying no-`sudo` installer and archives for macOS arm64 and Linux x86_64; no crates.io package |

### Pinned model artifacts in the current checkout

Model management is global and explicit. `init`, `ingest`, `search`, MCP, and
every inspection command remain network-free. The only command allowed to
download is:

```bash
hsum model list
hsum model install embedding bge-small-en-v1-5-fp32
hsum model verify bge-small-en-v1-5-fp32
```

Set `HSUM_OFFLINE=1` to disable that network path too. Installation uses HTTPS,
the platform certificate store, standard proxy environment variables, and an
optional `--ca-bundle private-ca.pem`. Files are published only after every
size and SHA-256 check succeeds.

For an air-gapped machine, install on a connected machine, transfer the exact
content-addressed model directory (including `hsum-model.json`), then import
the directory or the receipt path:

```bash
hsum model import /media/transfer/bge-model-directory
hsum model list --json
```

This slice manages trusted local bytes only. It deliberately does not expose
`init --embedding-model`, `ingest --reembed`, `search --mode semantic`, or
`search --mode hybrid`; those remain gated on the vector and inference
portability proof.

The next gated step now has an opt-in, non-product
[FastEmbed CPU portability probe](benches/model_portability/README.md). It
loads only re-verified cache bytes and persists no vectors. The checked-in
Apple M2/macOS arm64 development run passes its latency, output, and RSS
ceilings; Linux x86_64 remains unqualified until the prepared native-runner
workflow publishes equivalent evidence.

### Filesystem sources in the current checkout

Filesystem roots can be registered in the selected managed index without
changing project scope or ingesting content:

```bash
hsum source add fs /absolute/path/to/repository --name docs-root
hsum source list --all --json
```

Registration canonicalizes the directory, refuses broad roots unless
`--allow-broad-root` is explicit, and is idempotent only for an exact
name/root/config match. An unattached source appears only with `source list
--all`. Activate it through the project root-replacement ceremony, then ingest
explicitly:

```bash
hsum project set-root /absolute/path/to/repository --source-name docs-root --confirm
cd /absolute/path/to/repository
hsum ingest --source docs-root --strict
```

Activation reuses the registered source UUID. If a later root replacement
retires the source, repeating the exact registration reactivates that same UUID
without changing the selected project's scope.

### JSONL snapshot sources in the current checkout

JSONL source management is explicit and currently applies to the selected
project. Registering a file does not ingest it:

```bash
hsum source add jsonl /absolute/path/records.jsonl --name records
hsum source list --json
hsum ingest
```

`hsum ingest` refreshes the filesystem authority and every active JSONL source
in one atomic generation. Repeat `--source ID_OR_NAME` to target a subset. In
the default mode, successful sources commit while failed sources retain their
last good heads; a partial batch exits `6`, and an all-failed batch exits `1`.
`--strict` makes any selected-source failure exit `1` before changing heads,
diagnostics, the generation, or the epoch.

Removal is an explicit destructive ceremony:

```bash
hsum source remove records --confirm
```

It tombstones the source's active documents and hides the source from every
project. `source detach` is the project-local alternative: it changes only the
selected project's membership and retains the source heads for other projects.
Immutable historical citations remain readable until a future prune operation
removes their stored versions. A prune plan enumerates the complete affected
document-revision namespaces and their canonical stored-chunk citations before
any bytes are deleted.

### Named projects in the current checkout

A new project initially shares the selected filesystem authority. Selection is
persistent and keeps the existing binding UUID, so client registrations do not
need to be regenerated:

```bash
hsum project create docs
hsum project list --json
hsum project use docs --confirm
hsum source list --all
hsum source attach records --confirm
```

Use `source detach ID_OR_NAME --confirm` to remove a JSONL source from only the
selected project. Attach/detach changes the project scope revision but does not
rewrite source heads or advance the index epoch.

Filesystem replacement is explicit and does not ingest as a side effect:

```bash
hsum project set-root /absolute/path/to/repository --confirm
cd /absolute/path/to/repository
hsum ingest --strict
```

The prior filesystem membership becomes historical. Other projects that still
use it are unaffected, and old citations remain readable until prune. Broad
roots require `--allow-broad-root`; active sources and projects remain bounded.

### Confirmed index deletion in the current checkout

Whole-index removal is name-targeted and never inferred from the current
directory:

```bash
# Create an external backup first if the evidence may be needed again.
hsum backup create /private/path/doomed-backup.sqlite --confirm
hsum index delete doomed --confirm
```

Deletion waits for index writers and in-flight evidence readers, verifies the
database with Doctor, clears a matching configured default, removes every trust
binding for the exact index UUID/name pair, and then renames the managed index
directory to a fixed quarantine before removing it. A cleanup interruption is
resumed by repeating the same command. Other managed indexes are untouched.

This command does not create or delete backups, or search repositories for
`.hsum.toml`. Managed-backup records survive index deletion and remain visible
with `hsum backup list --all`; user-created copies are never added to that
inventory. Repository pointers are retained as unauthoritative hints and cannot
reopen the deleted index; remove them manually if they are no longer useful.
Filesystem snapshots and user-created copies are outside the deletion boundary.

### Guarded maintenance in the current checkout

Create a self-contained backup without overwriting an existing path:

```bash
hsum backup create /private/path/index-backup.sqlite --confirm
hsum backup list
# Include backups retained for deleted or currently unselected indexes.
hsum backup list --all --json
```

The destination directory must satisfy hSUM's private local-storage checks.
The online SQLite copy is normalized to a sidecar-free WAL database, reopened
through full Doctor validation, and published only after its index identity is
verified. The receipt prints its byte length and file SHA-256.

Every index backup created by `backup`, `prune`, `migrate`, `forget`, or
`restore` is reserved and then recorded in a bounded, private registry under the
managed data directory. Inventory is conservative: `verified` entries still
match their recorded size and SHA-256; `changed`, `unsafe`, and
`pending-present` entries may contain evidence but cannot be purged
automatically. Missing files remain visible until the next purge clears their
records. Paths are stored losslessly on the current operating system. Copies
made outside hSUM are user-created and cannot be discovered or deleted by this
registry.

Pruning is always a plan/apply ceremony:

```bash
hsum prune plan /private/path/prune.json \
  --before 2026-01-01T00:00:00Z \
  --keep-latest 1
# Review the affected revision namespaces and every stored-chunk citation.
hsum prune apply /private/path/prune.json \
  --backup /private/path/before-prune.sqlite \
  --confirm <plan-hash>
```

Both selector conditions must match: a revision must be older than `--before`
and outside the newest `--keep-latest N` revisions of its logical document.
There is no implicit age cutoff. Apply recomputes the exact hashed manifest
under the writer lock, creates or verifies the bound backup, collapses the
pruned activation history to one auditable baseline, deletes only manifested
immutable revisions, records the manifest, and runs Doctor. A completed apply
is idempotent when retried with the same plan and backup.

Durable forgetting selects complete logical source documents—not only the
currently cited revision—and requires a one-generation baseline. Run prune
first when the index has retained history:

```bash
hsum forget plan /private/path/forget.json \
  --citation 'hsum://v1/...#bytes=...'
# Review the entire affected namespace, not only the selector span.
hsum forget apply /private/path/forget.json \
  --recovery-backup /private/path/pre-forget.sqlite \
  --restore-plan /private/path/restore.json \
  --keep-managed-backups \
  --confirm <forget-plan-hash>
```

Apply first fsyncs a body-free, hash-chained sidecar ledger keyed by source and
connector-key hash. That prepared record suppresses Get, Search, and ingest
even before replacement activation. hSUM then creates and verifies the recovery
backup, removes the selected document and derived indexes in a staged database,
compacts it, waits for shared reader-delivery leases to drain, and atomically
replaces the live SQLite file while advancing an external epoch. Old citations
return `FORGET_TOMBSTONE`, copied forgotten databases retain the database mirror,
all future revisions of the logical connector remain suppressed, and replaying
the pre-forget backup at the live path fails against the surviving ledger. The
recovery backup intentionally still contains the evidence. `forget apply`
refuses at the CLI boundary unless exactly one disposition is supplied:
`--keep-managed-backups` reports and retains every managed backup for the index,
while `--purge-managed-backups` deletes every unchanged, private, single-link,
sidecar-free managed backup and clears missing records. Purge preflight runs
before the forget mutation, so a changed, hard-linked, redirected, or incomplete
backup blocks the operation without changing evidence. After forget starts,
purge is resumable: deleted-but-still-recorded paths are treated as missing on
the retry. User-created copies, filesystem snapshots, SSD wear-leveling, and
other forensic remnants remain outside hSUM's application-level promise.

`forget apply` also writes the only restore plan accepted for that operation.
Restore is an immediate exact-state undo, not a general rollback:

```bash
hsum restore apply /private/path/restore.json \
  --recovery-backup /private/path/pre-forget.sqlite \
  --safety-backup /private/path/forgotten-state.sqlite \
  --confirm <restore-plan-hash>
```

The restore plan binds the finalized post-forget database hash and the recovery
backup hash. Any later ingest or other database mutation makes it stale before
the safety backup is created. A successful restore advances the evidence and
replacement epochs, records an audit row, atomically publishes a compacted
replacement, clears external suppression with an append-only restored record,
and preserves the forgotten state in the required safety backup. Forget and
restore retries resume the same hash-bound operation after interruption.

Schema migration supports only the released N-1 schema and does not depend on
normal project context resolution:

```bash
hsum migrate plan --index INDEX /private/path/migration.json
hsum migrate apply --index INDEX /private/path/migration.json \
  --backup /private/path/before-migration.sqlite \
  --confirm <plan-hash>
```

Prune and migration apply print rollback guidance naming the verified pre-change
backup. Older-than-N-1 schemas, changed plans, incorrect confirmations,
existing outputs, and failed verification stop before mutation.

User configuration and the trust registry use their own schema and migrate
through a separate ceremony that does not require normal project selection:

```bash
hsum config migrate plan /private/path/config-migration.json \
  --backup-dir /private/path/config-before-v2
hsum config migrate apply /private/path/config-migration.json \
  --confirm <plan-hash>
```

Planning only diagnoses the current and N-1 files. Apply revalidates their
exact hashes under both configuration writer locks, creates byte-for-byte
private backups plus the canonical plan manifest before replacing either file,
and accepts an already-migrated target hash so an interrupted two-file apply is
resumable. Ordinary context selection, search, and MCP never migrate these
files implicitly. A relative global `--config` path is bound into the plan as
an absolute path.

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
toolchain and dependencies. After a binary exists, the current checkout's sole
network client is the explicit `hsum model install embedding` path described
above; ingest, retrieval, MCP, import, verification, and ordinary inspection
remain local. hSUM has no account, telemetry, HTTP server, or implicit network
runtime path. `HSUM_OFFLINE=1` disables model download, and an already
populated Cargo cache can build with Cargo's normal `--offline` option.

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
  index, project, active filesystem authority, and source root still match.

## Command inventory

The current candidate exposes only the following command graph:

| Command | Output and effect |
|---|---|
| `hsum init [PATH]` | Dry-run or create one managed filesystem index, project, source, and user trust binding; `--rebuild` safely replaces the binding and index |
| `hsum trust PATH --confirm` | Add or confirm a root-bound user trust binding for an existing matching index |
| `hsum ingest` | Dry-run or commit one atomic filesystem/JSONL generation; repeat `--source` or use `--strict` |
| `hsum source ...` | Add/list/attach/detach JSONL sources, or globally tombstone one with confirmed `remove` |
| `hsum project ...` | Create/list/use projects or replace the selected filesystem root with confirmed `set-root` |
| `hsum index delete NAME --confirm` | Clear matching config/trust authority, fence active readers and writers, and remove exactly one named managed index |
| `hsum search QUERY` | Human evidence or one JSON document; exact plus BM25 |
| `hsum get CITATION_URI` | Human evidence or one JSON document from an immutable citation |
| `hsum status` | Human status/problems or one JSON document |
| `hsum doctor [--integrity]` | Read-only full integrity diagnosis |
| `hsum doctor --repair --confirm` | Remove only abandoned generation rows under the writer lock with full pre/post validation |
| `hsum doctor report --output PATH` | Write a private, non-overwriting, body-free and query-free support report |
| `hsum backup create OUTPUT --confirm` | Create, normalize, hash, Doctor-verify, and inventory a non-overwriting SQLite backup |
| `hsum backup list [--all] [--json]` | Report managed backup paths, purposes, receipts, and current verification states |
| `hsum prune plan/apply ...` | Select with explicit `--before`/`--keep-latest`, review citation impact, create a rollback backup, and prune an exact immutable plan |
| `hsum forget plan/apply ... --keep-managed-backups\|--purge-managed-backups` | Review logical-document namespaces, choose explicit backup disposition, durably suppress connector keys, and atomically publish a compacted body-free index |
| `hsum restore apply ...` | Restore only the exact unchanged post-forget state after creating a safety backup |
| `hsum migrate plan/apply ...` | Diagnose or upgrade one named managed N-1 index with an exact plan and rollback backup |
| `hsum config migrate plan/apply ...` | Diagnose or upgrade N-1 user config/trust files with one hashed plan and exact private backups |
| `hsum context` | Human selection details or one JSON document |
| `hsum client config CLIENT` | Copyable JSON or TOML for `codex`, `claude-code`, `claude-desktop`, or `generic` |
| `hsum client doctor CLIENT` | Validate the generated configuration with a real empty-directory MCP probe |
| `hsum help error SUBCODE` | Offline explanation of one stable public error subcode |
| `hsum completions SHELL` | Completion text on stdout for `bash`, `zsh`, `fish`, `powershell`, or `elvish` |
| `hsum man` | A generated `hsum(1)` page on stdout |
| `hsum mcp` | Project-bound MCP over stdin/stdout |

The only `help` subcommand is the offline error catalog,
`hsum help error <SUBCODE>`; for command help use `hsum --help` or
`hsum <command> --help`. The current candidate has no `model`, top-level
`repair`, `watch`, or daemon command.

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
"$HSUM" search 'source_state' --limit 10 --cursor '<next_cursor>' --json
```

Search limits are 1–50 results, the deadline is 100–10,000 ms, and the defaults
are 10 results and 3,000 ms. A query is at most 4,096 UTF-8 bytes. Double quotes
delimit exact spans and have no escape syntax. When more results remain,
human output and JSON expose an opaque `next_cursor`; pass it back through
`--cursor` with the same query, mode, and `--explain` setting. Cursors bind the
project, retrieval configuration, index identity, generation, epoch, and scope
revision, and become stale rather than silently paging changed evidence.

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
Both citations are independently resolvable. The returned citation names the
exact contiguous whole-chunk window in `content` and can be passed back to
`get` without substituting the current source bytes.
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
"$HSUM" doctor --integrity
"$HSUM" doctor --repair --confirm
"$HSUM" doctor report --output /private/path/doctor-report.json
```

`status` reports generation/epoch, active document and passage counts,
per-source success or failure state, persistent quota, managed SQLite/WAL
bytes, available capacity, recovery reserve, filesystem locality, and
actionable problem codes. `context` shows the effective index, project, source
root, trust binding, selection origin, managed paths, and persisted quota.

Bare `doctor` and `doctor --integrity` are read-only full scans. They validate
the SQLite application ID, exact schema manifest and checksum, migration chain,
pipeline fingerprint, WAL mode, integrity and foreign keys, committed
generation history, immutable content digests and chunk layouts, active
FTS/literal index parity, and body-free forget/restore ledger consistency.

The confirmed repair is deliberately narrow: under the writer lock it runs the
same full diagnosis before and after deleting only rows already marked as
abandoned generations. It never repairs active heads, committed history,
stored evidence, derived indexes, schema objects, or forget state. The report
command writes private canonical JSON with schema identities, fingerprints,
aggregate scan counts, and abandoned-row counts. Its output names every
included field and explicitly excludes bodies, passages, queries, source URIs,
connector keys, and filesystem paths; an existing output is never overwritten.

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

Before initialization, ingest, backup, prune, migration, forget, and restore, hSUM estimates
the relevant writes and
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

CLI `get --json` and MCP `evidence_get` expose the same immutable evidence,
`source_state`, `source_hash_verification`, and manual `freshness` fields.
Verification is relative to the cited revision, so an older citation remains
verifiable even after a newer document head has been indexed. Both interfaces
serialize that complete response through the same versioned protocol type.

CLI `search` and MCP `evidence_search` share the same ranking, snapshot,
live-source observation, and opaque cursor semantics. A cursor emitted by
either adapter can continue through the other when its bound query shape and
snapshot still match. MCP additionally applies aggregate-body and
structured-response limits at its transport boundary. Both interfaces derive
response metadata, each passage, score explanation, duplicate citation, and
source state through the same protocol mapping; their documented outer
envelopes, span layouts, and rank-key layouts remain compatible with existing
consumers.

CLI `status --json` and MCP `evidence_status` derive index identity, project
counts, source health, and actionable problem codes from the same read-only
SQLite snapshot. Storage capacity and locality checks run after that snapshot
closes. One shared protocol mapping constructs health issues, repair objects,
and the two documented compatibility envelopes.

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
Apple notarization and launch-wide benchmark claims remain explicitly deferred.
The frozen 25-query repository-local development baseline is documented in
[`benches/agent_ab/README.md`](benches/agent_ab/README.md). hSUM has the best
mean quality across three isolated builds but remains much slower than grep and
shows unresolved cross-build ranking variance. See
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
