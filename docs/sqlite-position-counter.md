# Restricted FTS5 position counter

Broad lexical queries spend much of their time calling `xPhraseNext` once per
phrase occurrence. The ranker can count a narrowly defined remainder in one
bounded scan. This deliberately uses private SQLite iterator bytes; it is not
part of the stable public FTS5 extension contract.

The optimization is enabled only for the audited bundled SQLite 3.51.3 source:

- Amalgamation SHA-256: `9512509b1bccb7461f79bea8aad6280ae4699e925fa4804381b71f59e7efb0c5`.
- Runtime source ID: `2026-03-13 10:38:09 737ae4a34738ffa0c3ff7f9bb18df914dd1cad163f28fd6b6e114a344fe6d618`.

`build.rs` verifies bundled dependency metadata and the source hash. Linked
SQLite, pkg-config overrides and custom `LIBSQLITE3_FLAGS` use native iteration.
The exact runtime source ID must also match before reading iterator bounds.
Dependency updates therefore fall back automatically until reviewed.

In the audited implementation, `fts5ApiPhraseFirst` obtains a cursor-owned
position-list buffer and consumes the first occurrence. Its remaining `a..b`
range stays live while this callback runs. Each remaining byte in `2..=127`
makes `fts5ApiPhraseNext` emit exactly one occurrence. The counter first verifies
the whole tail has only those bytes, then returns one plus its length. Every
column marker, multibyte encoding, unsupported byte or questionable bound uses
the untouched native iterator. It does not decode general SQLite varints.

The range must be nonnull and ordered. Its length cannot exceed
`(i32::MAX - initial_offset) / 125`; this conservatively prevents offset
overflow and bounds the Rust slice. Allocation validity comes from the audited
SQLite implementation, not from integer pointer comparisons. The borrow never
escapes the callback, and no cursor-changing API runs while it exists.
Cancellation is checked before scanning and at least every 256 bytes. Full
phrase-statistics scans, corruption checks, BM25 arithmetic, scope, tie ordering
and guarded body materialization remain unchanged.

Tests compare every accepted fast-path frequency with a cloned public iterator,
compare score bits against native BM25 and forced-native execution, exercise
actual complex-position fallback, and cover invalid bounds, malformed bytes,
cancellation and source mismatch. `scripts/ranking-sanitizer-smoke.sh` builds
the actual ranker/counter modules in an isolated crate and tests both enabled
and deliberately disabled paths. Linux instruments Rust and C; macOS currently
instruments Rust because Apple Clang's sanitizer runtime is incompatible with
the pinned Rust nightly runtime. The focused harness excludes the application
and model runtime and disables leak checks.

When updating SQLite, inspect `fts5CsrPoslist`, `fts5ApiPhraseFirst` and
`fts5ApiPhraseNext`, including buffer allocation, invalidation and malformed
input behavior. Update the allowlist only after that review, differential tests
and sanitizer checks. Native performance qualification remains a separate gate.
