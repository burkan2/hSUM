# Bounded lexical ranking follow-through

The actual scorer / scoped SQL diagnostic returns identical top-500 IDs and
score bits. Median A/B/A: 64.50 / 45.20 / 63.10 ms. Cache and FTS compaction
probes showed only small gains and are not selected for implementation.

Production design to implement and review:

1. New search/frontier.rs returns Option<Vec<(id, score)>>. None means unchanged
   lexical_ranked_ctes SQL fallback. Preflight docsize count <=131072; cap actual
   score vector length AND capacity, including underestimated count. Request-local
   score buffer is at most 2 MiB (16 bytes per row); do not use a whole-query map.
2. Scan every matching row with current guarded hsum_bm25 and ORDER BY rowid.
   Keep a bounded 500-score max heap while scanning to determine global cutoff.
   This removes the prototype's uninterruptible select_nth and extra score map.
   Every score bit and all native phrase statistics/corruption checks remain.
3. Feed frontier IDs in batches <=1024 through json_each, using exactly existing
   project/head/version/chunk eligibility joins. Return json_each.key to associate
   each result with its score. Fixed [u8;16] source/document tie keys; malformed
   tie metadata falls back without suppressing it. SQL errors always propagate.
4. BTreeSet bounded at K=500 tracks eligible canonical keys (score/source/doc/start).
   If an ambiguous fully equal key can affect selected output, fall back to SQL;
   never add an unproven rowid tie-break. Scan remainder only if <K eligible frontier
   rows survive. Batch metadata and top-K storage are bounded independently.
5. Existing SearchEvidenceRequest already carries cancelled: Option<fn()->bool>.
   Add IndexDb::search_with_cancellation(crate-only); existing public search passes
   None. App forwards its existing persistent callback. Do not couple search to MCP
   globals or rely on transient sqlite3_is_interrupted between statements.
   Pass deadline + cancellation checkpoints through score/heap/metadata processing.
6. Keep all steps in existing execute_search transaction. Only main lexical query
   initially uses the new helper; exact phrase pager can retain old SQL.
   Emit bounded VALUES(id, score) with <=1001 parameters (project + 500 pairs)
   into existing body SQL. Bind f64 directly; do not roundtrip score JSON.
   Keep existing Rows cursor / fetch_lexical_page so body checks remain lazy and
   exactly500 still reports WorkBudgetExhausted under current short-fetch semantics.
7. Tests first: differential0/49/50/51/499/500/501, all cutoff ties, foreign/removed/
   inactive/replaced-head frontier + remainder, malformed tie metadata fallback,
   fully equal keys fallback, deferred body corruption after first50, capacity
   boundary / underestimated docsize, persistent cancellation in every phase and
   recovery. Existing native BM25, cancellation, scope, corruption tests retained.
8. Then full contributor + optimized tests, A/B/A all25 product result equivalence,
   independent review, Linux diagnostic. Freeze new candidate only on meaningful
   measured gain. Full same-candidate Mac/Linux preparation+qualification remains
   mandatory; no SLO/harness/input relaxation or resampling to replace a failure.

Completed November 4: independent review approved the production implementation,
integration and retained result-equivalence audit for commit and Linux diagnostic.
The prototype is diagnostic only and must not be
left in examples during full contributor checks. All retained crash/recovered/
41af/5ca qualification evidence stays untouched.
