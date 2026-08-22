# Retrieval determinism qualification

This harness proves the stable-v0.1 same-index ordering contract. It does not
claim that two independently built indexes assign the same identities or rank
equal-score candidates identically. That separate behavior is diagnosed in
[`eval/LEXICAL_VARIANCE_DIAGNOSIS.md`](../../eval/LEXICAL_VARIANCE_DIAGNOSIS.md):
fresh source and document UUIDs are the final tie-break boundary, so
independent-build studies must continue to publish ranges.

## Evidence boundary

The native workflow creates one lexical index on Linux, closes it without WAL
sidecars, and transports those exact SQLite bytes to both supported targets.
The corpus is materialized at the same canonical `/opt/...` path because the
workspace MCP server binds its index through the trusted repository root.

On each target the harness runs:

- every frozen query through five fresh CLI processes;
- all frozen queries through each of five fresh MCP server processes;
- CLI/MCP normalization and exact same-target comparison; and
- an offline-only assertion that lexical search never invokes query embedding.

The fan-in comparison requires the same index SHA-256, manifest, commit,
query order, citation order, duplicate-citation sets, degradation flags, and
explanation lists. Backend numeric scores may differ by at most `1e-6`.
Request IDs and timing fields are the only runtime-varying fields excluded from
the normalized evidence contract.

This is lexical stable evidence. It does not qualify hybrid for promotion;
hybrid remains beta under the held-out evaluation disposition.

## Fast local gate

The contributor gate runs the standard-library unit tests and validates every
corpus hash, query, bound, and process count:

```bash
python3 -m unittest -v benches/retrieval_determinism/test_harness.py
python3 benches/retrieval_determinism/harness.py validate
```

The complete `/opt` journey is intentionally a hosted-runner qualification,
not a developer-machine prerequisite. Run the **Retrieval determinism**
workflow to produce the portable fixture, two native reports, and the
cross-target comparison artifact.
