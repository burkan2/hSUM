# Linux ranking sanitizer — `5ca552c`

**PASS.** The combined Rust/C AddressSanitizer run exercised the actual copied
ranking, position-counter and C scoring code against the pinned bundled SQLite
source. It ran 12 tests with the audited fast path enabled and 11 with that
path deliberately disabled. Both configurations pass without sanitizer findings.
Independent AI review checked the source hashes and dependency lock subset.

This is focused memory-safety evidence, not a performance or release approval.
The corresponding workflow also passed native Linux and Mac contributor gates.
Raw receipts, lockfile, logs and workflow metadata are retained with file hashes.

Workflow: https://github.com/burkan2/hSUM/actions/runs/41321721032
