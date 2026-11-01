# Joined docsize ranking diagnostic — November 1

The guarded per-row docsize join replaces the lazy whole-index length cache.
Independent AI review approved the implementation after all 21 focused search
tests passed in debug and release, including native score bits, malformed and
missing sizes, sparse/negative/zero rowids, scope/ties/offsets and interruption.
The complete contributor gate and release smoke also pass. Red/green logs are
retained. No ranking formula, frozen query, threshold or body guard changed.

A reduced Mac product A/B/A on the existing disposable checkpointed recovered
100k clone ran two warmups and ten measured passes of all 25 frozen queries.
The broad ordinary-02 median was 85 / 84 / 85 ms; p95 was 89 / 87 / 88 ms.
The runner asserted identical complete result arrays and explanations for
every call. The earlier SQL-only prototype's ~8% gain did not carry through
to a comparable product gain. Do not infer a Linux qualification pass.

Raw timings, binary hashes and production source hashes are retained. This
initial local run predates the diagnostic harness's per-call result digests;
its exact harness file hash was not captured, so independent result-array
reconstruction is unavailable. The revised committed diagnostic harness will
retain baseline arrays, every call's result hash, and failed-phase evidence.
No cold-cache eviction, fresh inference or canonical three-run protocol was
performed. The frozen candidate41af5c7 reports remain unchanged.
