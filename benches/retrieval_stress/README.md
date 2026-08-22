# One-million-passage stress qualification

This is the stable-v0.1 report-only knee finder from
`work/local-rust-evidence-bus-design.md`. It has no release latency SLO and
must not be used to relax the separate 100k qualification thresholds.

## Frozen workload

- one empty filesystem authority plus 63 attached JSONL snapshot sources;
- 100,000 document identities and exactly 1,000,000 active passages;
- one shared 15,000-byte body containing a maximum-frequency identifier and a
  tokenizer-incompatible quoted literal;
- scale observations at 2, 8, 16, 32, and 64 total project sources;
- 100 fixed-width metadata-only generations, with the final generation
  committed while four old SQLite readers retain their prior snapshots;
- final offline re-embedding and a 64-source semantic/hybrid probe; and
- an eight-request cancellation storm that must suppress late responses and
  leave a later lexical request usable in the same MCP process.

The report records corpus generation, ingest/re-embed throughput, process-tree
RSS, source/passage counts, candidate growth, database/vector amplification,
query distributions, the first observed latency knee, old-reader visibility,
and cancellation recovery. Every retriever remains within the frozen 500-item
candidate budget and emits an explicit stop reason. A knee is the first scale
point where a query's median client round trip exceeds twice its immediately
preceding value. This is a descriptive preregistration, not a support boundary.

## Execution

The workload is intentionally manual and Linux-only for the stable evidence
train. The **Retrieval stress** workflow installs the pinned model outside all
timed stages, executes the complete harness offline, and uploads the raw report
even when a fail-closed invariant aborts the run.

Fast contract checks belong to the contributor gate:

```bash
python3 -m unittest -v benches/retrieval_stress/test_harness.py
python3 benches/retrieval_stress/harness.py validate
```
