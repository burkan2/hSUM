# Restricted position batching — November 1

Status: local correctness and safety checks pass; Linux diagnostic and combined
Rust/C sanitizer coverage remain pending. This is not native qualification.

Only tails consisting entirely of one-byte position deltas2..127 use the fast
counter, after exact bundled-amalgamation hash/runtime-source checks. Null,
reversed, oversized or offset-overflow concerns and every other byte use the
untouched public iterator. Full statistics, scoring, corruption checks, scope,
ties and body guards remain. See docs/sqlite-position-counter.md for the private
SQLite dependency and audit requirements.

All 28 release search tests and the complete contributor gate pass. Tests compare
accepted frequency counts against a cloned public iterator and native score
bits, force native fallback, and cover source mismatch, bounds and cancellation.
Rust AddressSanitizer passes 12 focused actual-source tests with the fast path
and11 with its build gate disabled. The initial Rust+C Mac ASan attempt fails
at linking because Apple Clang and Rust sanitizer runtime ABIs differ; this
is retained as a toolchain failure. C instrumentation is pending Linux CI.

The isolated SQL prototype improves broad ranking from ~74–75ms to ~58–60ms with
identical 500 result IDs and score bits. Pointer-hoisting and a single C math
loop prototypes did not improve it and were discarded; their logs are retained.

The all 25-query product A/B/A uses the frozen41af5c7 binary as both controls,
a disposable previously embedded fixture, two warmups and ten measured passes.
All 900 result digests match retained complete baseline arrays (750 measured
observations). Broad ordinary-02 median is88 /73 /96ms; p95 is245 /208 /235ms.
macOS storage-management services were active during the trial; a process/time
snapshot is retained. Control drift and outliers prevent a clean latency claim.
No samples were discarded and no full qualification SLO was evaluated here.
