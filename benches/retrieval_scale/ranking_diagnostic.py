#!/usr/bin/env python3
"""Reduced A/B/A ranking diagnostic. Never a replacement for qualification."""

import argparse
from contextlib import closing
from pathlib import Path
import sqlite3
import sys
import time
import traceback

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from benches.retrieval_scale import harness as h


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--home", type=Path, required=True)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--prepare", action="store_true", help="Create an absent, disposable lexical-only home and corpus")
    parser.add_argument("--include-hybrid", action="store_true", help="Use an existing embedded diagnostic fixture")
    args = parser.parse_args()
    if args.output.exists():
        parser.error("output already exists; retain previous measurements")
    if args.prepare and args.include_hybrid:
        parser.error("fresh diagnostic preparation is lexical only")
    home, corpus = args.home.resolve(), args.corpus.resolve()
    baseline, candidate = args.baseline.resolve(), args.candidate.resolve()
    if h.sha256_file(baseline) == h.sha256_file(candidate):
        raise h.HarnessError("baseline and candidate binaries are identical; refusing invalid comparison")
    queries = h.load_queries()
    preparation = None
    if args.prepare:
        if home.exists() or corpus.exists():
            parser.error("fresh diagnostic home and corpus must both be absent")
        home.mkdir(parents=True, mode=0o700)
        corpus.parent.mkdir(parents=True, exist_ok=True)
        manifest = h.create_corpus(corpus, queries)
        started = time.perf_counter()
        h.run_command([str(candidate), "init", str(corpus), "--index", h.INDEX_NAME,
                       "--project", h.PROJECT_NAME, "--allow-large-source"], cwd=corpus, home=home)
        preparation = {"corpus": manifest, "lexical_init_seconds": time.perf_counter() - started,
                       "embedding": "not performed"}
        # All product processes are closed; this is a disposable offline fixture.
        # Explicit checkpointing is diagnostic preparation, not a product claim.
        database = home / "data" / "indexes" / h.INDEX_NAME / "index.sqlite"
        with closing(sqlite3.connect(database)) as connection:
            result = connection.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()
            if result != (0, 0, 0):
                raise h.HarnessError(f"diagnostic checkpoint incomplete: {result}")
        preparation["offline_checkpoint"] = list(result)
    corpus_manifest = h.read_json(corpus / ".hsum-retrieval-scale-corpus.json")
    if corpus_manifest.get("fingerprint_sha256") != h.corpus_fingerprint(queries):
        raise h.HarnessError("corpus fingerprint does not match the frozen contract")
    status = h.run_json_command([str(candidate), "status", "--json"], cwd=corpus, home=home)
    if status.get("active_passages") != h.EXPECTED_PASSAGES:
        raise h.HarnessError("diagnostic requires exactly 100,000 active passages")
    if not args.include_hybrid:
        queries = [query for query in queries if query["mode"] == "lexical"]
    report = {"qualification": False, "scope": "Reduced warm A/B/A diagnostic; no cold eviction or fresh inference; two warmups and ten measured passes per phase.",
              "source": h.git_revision(), "machine": h.machine_report(),
              "query_manifest_sha256": h.sha256_file(h.QUERY_PATH),
              "corpus": corpus_manifest,
              "harness_sha256": h.sha256_file(Path(h.__file__)),
              "diagnostic_sha256": h.sha256_file(Path(__file__)),
              "status": status, "preparation": preparation, "phases": [], "completed": False}
    expected = {}
    report["baseline_results"] = expected
    for name, binary in [("baseline-before", baseline), ("candidate", candidate), ("baseline-after", baseline)]:
        phase = {"name": name, "binary_sha256": h.sha256_file(binary),
                 "observations": [], "calls": [], "completed": False}
        report["phases"].append(phase)
        h.write_json_atomic(args.output, report)
        session = None
        try:
            session = h.McpSession(binary, corpus, home)
            for pass_index in range(-2, 10):
                for query in queries:
                    packet, milliseconds = session.search(query)
                    call = {"query_id": query["id"], "pass": pass_index + 1,
                            "result_sha256": h.sha256_bytes(h.canonical_json_bytes(packet.get("results")))}
                    phase["calls"].append(call)
                    try:
                        h.validate_search_packet(query, packet)
                        results = packet["results"]
                        if query["id"] not in expected:
                            expected[query["id"]] = results
                        if results != expected[query["id"]]:
                            raise h.HarnessError(f"result or explanation changed: {name} {query['id']}")
                    except Exception:
                        call["failed_packet"] = packet
                        raise
                    if pass_index >= 0:
                        phase["observations"].append(h.compact_observation(query, packet, milliseconds, pass_index + 1))
            phase["summary"] = {query["id"]: h.distribution(observation["timing_ms"]["total"]
                for observation in phase["observations"] if observation["query_id"] == query["id"]) for query in queries}
            phase["completed"] = True
        except BaseException:
            phase["error"] = traceback.format_exc()
            raise
        finally:
            try:
                if session is not None:
                    peak_kib, sampler_error = session.close()
                    phase["peak_process_tree_rss_bytes"] = peak_kib * 1024
                    if sampler_error:
                        raise h.HarnessError(f"RSS sampler failed: {sampler_error}")
            except BaseException:
                phase["completed"] = False
                phase["shutdown_error"] = traceback.format_exc()
                raise
            finally:
                h.write_json_atomic(args.output, report)
        print(f"{name}: ordinary-02 {phase['summary']['ordinary-02']}", flush=True)
    report["completed"] = True
    report["result_equivalence"] = "all returned fields and explanations identical across all calls"
    h.write_json_atomic(args.output, report)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
