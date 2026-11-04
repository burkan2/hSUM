# Bounded lexical ranking

The lexical retriever scores every matching FTS row using the existing
unit-weight BM25 implementation. A request-local fast path avoids materializing
all scores into a SQLite temporary table when the FTS docsize table contains
at most 131,072 rows. Larger indexes use the existing SQL ranking query.

The score buffer holds at most 131,072 `(i64, f64)` pairs, or 2 MiB. Both the
estimated count and actual collection are checked; an underestimated count
cannot grow the allocation. A heap retains at most 500 scores to determine
the global cutoff. Scope eligibility is checked in batches of at most 1,024
IDs, and a sorted set retains at most 500 eligible fixed-width keys (501
transiently before eviction). No whole-query score map or metadata list is
allocated. JSON carries only signed integer IDs; `json_each.key` associates
each result with the original score without a floating-point conversion.

All ties at the cutoff participate. Project membership, removed sources,
document head/version agreement, active state and chunk identity use the
existing SQL joins. If fewer than 500 eligible frontier rows survive, the
remaining already-scored matches are considered. The result uses the same
score/source/document/start-byte order. A completely equal ordering key that
could affect selected output requests the original SQL path, preserving its
established tie behavior. Malformed tie metadata also requests that fallback;
SQL errors and interruption always propagate instead of becoming a success.

The selected IDs and original `f64` scores bind directly into the existing
guarded body query. Five hundred pairs plus the project use 1,001 parameters,
within the connection's 1,024 limit. Both paths retain the same lazy `Rows`
cursor: fusion reveals 50 candidates at a time, and body validation happens
on consumption. Exactly 500 candidates still has the existing work-budget
semantics; knowing the score list's length does not shortcut exhaustion.

Scoring, eligibility and body reads remain inside one existing search snapshot
transaction. Rust processing polls the search deadline and the application's
persistent cancellation callback, including while skipping scores between
batches. The callback is supplied by `SearchEvidenceRequest`; CLI callers
retain the public `IndexDb::search` entry point without a cancellation callback.
Search errors check application cancellation before probing snapshot state so
cancelled requests retain their public cancellation identity.

Phrase fallback retrieval retains its previous SQL path. BM25 statistics,
position counting, score arithmetic and corruption checks are unchanged; the
private SQLite position-counter dependency remains documented separately in
[sqlite-position-counter.md](sqlite-position-counter.md).

The diagnostic motivating this change reduced one broad query's median from
64.50/63.10 ms in its surrounding controls to 45.20 ms with identical top-500
IDs and score bits. That prototype is not production qualification. Full
same-candidate Linux and Mac protocols remain required before release.
