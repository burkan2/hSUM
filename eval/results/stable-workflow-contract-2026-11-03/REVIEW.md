# Stable workflow preparation — November 3–4, 2026

**Implementation reviewed; stable publication remains blocked.** Independent AI
review approved the final workflow, authority/control helpers, smoke routing
and all-asset verification receipts without remaining critical/high findings.
This is not an external security audit or a production signing trial.

All 33 release-contract tests pass: six authority-pin tests, five tag/environment
control tests, twelve real-Minisign asset tests, seven installer tests and three
smoke-routing tests. The routing regression exercises both archive platforms,
missing bundles, Gatekeeper rejection, binary-only selection and error
propagation using explicit binary/OS stubs and real disposable signatures.
Those stubs do not qualify native signing or OS isolation.

The actual `5ca552c` Mac binary passes alpha installer smoke, full network-denied
smoke, binary-only network-denied smoke and published-alpha.1 upgrade smoke.
Workflow actionlint 1.7.12, shell syntax, source-diff whitespace, README and completion
ledger checks pass. The two downloaded sanitizer logs retain their original
trailing blank lines; those are the only full-diff whitespace warnings.
Ledger totals remain 222/316 cells, 54 rows and zero
release-qualified rows. The pinned GitHub CLI 2.103.3 stages successfully from
its checked archive and exposes the required attestation source-binding flags.

The initial routing regression failed on its expected error wording; it was
corrected to the existing Developer ID/notarization message. The all-asset
receipt regression first failed because the receipt omitted individual file
hashes, then passed after adding hashes for all 13 assets. Both initial logs
are retained. The new tag/environment helper tests also failed on the missing
module before implementation; their final green log is retained.

The live read-only configuration audit found immutable releases enabled and
zero configured environments. Production Apple credentials, offline Minisign
authority, independent key distribution and protected environment setup remain
open. No production keys, stable tags, environments or releases were created.
The workflow deliberately refuses to proceed without those prerequisites and
exact-candidate approval. Linux ordinary lexical qualification also still fails.
