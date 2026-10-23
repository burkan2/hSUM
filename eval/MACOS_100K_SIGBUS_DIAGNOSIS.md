# Mac 100k preparation SIGBUS — 2026-10-24

**Disposition: preparation failed; no Mac performance measurement exists.**
The M2/16 GB run remains pinned to candidate
`1ce11d00bf8cad949c12a9c496ab7dba3675717f`. `prepare.exit` is 2 and the retained
harness error says its child `hsum ingest --reembed` returned **-10**.
`setup-macos-arm64.json`, `measurement.started`, `measurement.exit`, and
`macos-arm64-100k.json` are absent. The guarded authenticated launcher correctly
refused to measure without successful preparation. There is no 2,250-query
Mac report or legitimate cross-platform comparison to publish.

## Retained evidence and recovery state

[The evidence folder](results/macos-preparation-sigbus-1ce11d0/) contains the
unaltered prepare log/exit, launch receipt, raw-file hashes, a crash summary,
read-only recovery checks, and the native lock probe with its source and output.
The full Apple `.ips` crash report and exact pre-inspection index/WAL/SHM bytes
are retained locally at
`/Users/b.k./.cache/hsum-qualification/macos-100k-failure-1ce11d0/`.
APFS clones preserve the original failure without duplicating gigabytes of
physical storage. Full index and raw crash identifiers are not uploaded.

A disposable second clone passes SQLite `quick_check` and contains 100,000
active passages and **100,000 cached embeddings**. Its active generation/epoch
remain 1, vector slot 0, and generation 1 has `vector_state = absent`.
Thus inference cache population survived, but active vector membership was not
committed. A quick check is not full hSUM Doctor/vector-integrity validation.
Do not rewrite the failed preparation receipt or pretend cache-reuse timing
measures the original full embedding work.

## Crash location

Apple's report identifies PID 11603, captured at **2026-10-24 00:41:56 +03:00**,
with `EXC_BAD_ACCESS / SIGBUS`, subtype **FS pagein error: 22 Invalid argument**.
Kernel diagnostics explicitly report **cluster_pagein past EOF**. The fault is
at the beginning of a 32 KiB mapped-file region while `__bzero` runs inside
`walIndexAppend`, then `pagerWalFrames`, `pagerStress`, SQLite B-tree insertion,
`vec0_new_chunk`, and `commit_cached_reembedding_with_lock`.

This localizes the failure to a WAL shared-memory mapping during vector-slot
construction. It is not an observed model inference crash. The retained SHM
file is 32 KiB while the crash shows several adjacent 32 KiB mappings. Disk
was low (3.8 GiB available at inspection), but the report gives a mapped-file
past-EOF error; available disk alone does not establish the cause.

## Reproduced POSIX lock hazard

`ValidatedParent::validate_sidecars` opens each sidecar with
`open_validated_regular_at`, validates its file metadata, then drops the file
descriptor. `ValidatedIndexPath::verify_connection` performs this validation
around its live SQLite handle. Namespace revalidation similarly opens and
closes the main database file. On POSIX, closing another descriptor for an inode
releases that process's record locks for that inode, including locks acquired
by SQLite through a different descriptor.

The retained **SQLite 3.51.3 native probe** isolates the SHM case on a tiny,
separate WAL database. A separate process queries the DMS lock at byte 128:

1. With the native SQLite connection open, the SHM inode has a shared DMS lock.
2. A validation-style `open(O_RDONLY | O_NOFOLLOW)`, `fstat`, `close` on that
   same SHM inode makes the observer report **unlocked**, while the SQLite
   connection remains open.
3. Another SQLite reader can subsequently attach. The probe deliberately does
   not write through the now-unprotected mapping or force another SIGBUS.

The bundled SQLite `unixLockSharedMemory` implementation truncates SHM when it
finds no DMS owner. This establishes a concrete lock-loss mechanism in the
validation pattern that can permit mapped storage to be rebuilt or truncated.
It fits the crash's past-EOF failure. The crash report does **not** identify
which process truncated the live SHM file, so attribution of that particular
truncate is still an inference, not a traced event. Periodic external SQLite
readers were used during preparation; their presence is retained context,
not proof they issued the truncation.

## Required repair before another expensive attempt

Add a focused real-product cross-process regression that verifies live WAL
DMS/read/write locks survive namespace/sidecar validation and overlapping
connection lifetimes. Trace every caller of the shared file-validation helpers.
Preserve no-follow, private-owner/permission checks, held-inode and namespace
identity, replacement refusal, errors, and data-loss protections. Merely
skipping validation, changing timeouts, checkpointing around the race, or
rerunning the large workload would not repair the lock invariant.

Resolve the descriptor lifetime/metadata validation issue once in shared store
code, run the existing index-path, reader/writer, vector, and recovery checks,
and explicitly record the next candidate. Reuse retained inference cache only
through a correctly bound, explicitly labeled recovery/setup receipt; never
combine timings or binary provenance from different candidates. Full Mac cold
measurement will still require local authentication if the prior Terminal's
sudo ticket is no longer available. No credentials should be pasted into chat.
