import importlib.util
import json
import sqlite3
import tempfile
import unittest
from contextlib import closing
from pathlib import Path
from unittest import mock


HARNESS_PATH = Path(__file__).with_name("harness.py")
SPEC = importlib.util.spec_from_file_location("retrieval_stress_harness", HARNESS_PATH)
assert SPEC is not None and SPEC.loader is not None
HARNESS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HARNESS)


def search_packet(mode: str, retrievers: list[str]) -> dict:
    return {
        "requested_mode": mode,
        "effective_mode": mode,
        "degraded_mode": [],
        "results": [{}],
        "examined": {
            "exact": 1,
            "exact_fallback": 2,
            "lexical": 3,
            "vector": 4,
        },
        "retrievers": retrievers,
        "stop_reason": "limit_reached",
        "timing_ms": {stage: 1 for stage in HARNESS.TIMING_STAGES},
    }


class RetrievalStressHarnessTests(unittest.TestCase):
    def test_manifest_freezes_the_one_million_passage_contract(self):
        manifest = HARNESS.load_manifest()
        body = HARNESS.shared_body(manifest["body_bytes"])

        self.assertEqual(manifest["source_count"], 64)
        self.assertEqual(manifest["document_count"], 100_000)
        self.assertEqual(manifest["expected_passages"], 1_000_000)
        self.assertEqual(manifest["concurrent_old_readers"], 4)
        self.assertEqual(len(body), 15_000)
        self.assertEqual(HARNESS.scale.unbroken_chunk_count(len(body)), 10)
        self.assertEqual(body.count(b"StressPostingLiteral"), 155)
        self.assertFalse(any(character in body for character in b"\n\r.!?"))

    def test_document_distribution_is_balanced_and_complete(self):
        distribution = HARNESS.documents_per_source(HARNESS.load_manifest())

        self.assertEqual(len(distribution), 63)
        self.assertEqual(sum(distribution), 100_000)
        self.assertEqual(max(distribution) - min(distribution), 1)

    def test_snapshot_generation_is_streamed_and_reproducible(self):
        manifest = dict(HARNESS.load_manifest())
        manifest["document_count"] = 3
        manifest["jsonl_source_count"] = 2
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "snapshots"
            paths, report = HARNESS.create_snapshots(root, manifest)
            encoded = b"".join(path.read_bytes() for path in paths)

        self.assertEqual([path.name for path in paths], ["stress-00.jsonl", "stress-01.jsonl"])
        self.assertEqual(report["documents_per_source"], [2, 1])
        self.assertEqual(report["encoded_snapshot_bytes"], len(encoded))
        self.assertEqual(HARNESS.hashlib.sha256(encoded).hexdigest(), report["stream_sha256"])
        self.assertEqual(len(encoded.splitlines()), 3)

    def test_metadata_record_update_preserves_fixed_line_length(self):
        body_json = json.dumps(HARNESS.shared_body().decode("ascii"))
        initial = HARNESS.record_line(0, 0, body_json, 0)
        updated = HARNESS.record_line(0, 0, body_json, 100)

        self.assertEqual(len(initial), len(updated))
        self.assertNotEqual(initial, updated)

    def test_search_packet_requires_the_frozen_retrieval_paths(self):
        manifest = HARNESS.load_manifest()
        punctuation = manifest["queries"][1]
        hybrid = manifest["queries"][-1]

        HARNESS.validate_search_packet(
            punctuation,
            search_packet("lexical", ["exact", "exact_fallback", "lexical"]),
        )
        HARNESS.validate_search_packet(
            hybrid,
            search_packet("hybrid", ["exact", "lexical", "vector"]),
        )
        with self.assertRaises(HARNESS.StressError):
            HARNESS.validate_search_packet(
                punctuation,
                search_packet("lexical", ["exact", "lexical"]),
            )
        with self.assertRaises(HARNESS.StressError):
            HARNESS.validate_search_packet(
                hybrid,
                search_packet("hybrid", ["exact", "lexical"]),
            )
        over_budget = search_packet("lexical", ["exact", "lexical"])
        over_budget["examined"]["lexical"] = 501
        with self.assertRaises(HARNESS.StressError):
            HARNESS.validate_search_packet(manifest["queries"][0], over_budget)

    def test_probe_query_uses_the_cli_maximum_timeout(self):
        manifest = HARNESS.load_manifest()
        packet = search_packet("lexical", ["exact", "lexical"])

        with mock.patch.object(HARNESS, "run_json", return_value=packet) as run_json:
            HARNESS.probe_query(
                Path("hsum"), Path("workspace"), Path("home"), manifest["queries"][0]
            )

        arguments = run_json.call_args.args[0]
        timeout_index = arguments.index("--timeout-ms")
        self.assertEqual(HARNESS.QUERY_TIMEOUT_MS, 10_000)
        self.assertEqual(arguments[timeout_index + 1], "10000")

    def test_first_knee_is_strictly_more_than_double_the_preceding_median(self):
        def sample(source_count: int, p50: float) -> dict:
            return {
                "source_count": source_count,
                "passages": source_count * 10,
                "queries": {
                    "literal": {"client_round_trip_ms": {"p50": p50}},
                },
            }

        samples = [sample(2, 10), sample(8, 20), sample(16, 41)]
        knee = HARNESS.first_observed_knee(samples, 2.0)

        self.assertIsNotNone(knee)
        assert knee is not None
        self.assertEqual(knee["source_count"], 16)
        self.assertEqual(knee["ratio"], 2.05)

    def test_four_old_readers_retain_their_snapshot_during_final_generation(self):
        manifest = dict(HARNESS.load_manifest())
        manifest["metadata_only_generations"] = 2
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            database = root / "index.sqlite"
            snapshot = root / "source.jsonl"
            body_json = json.dumps(HARNESS.shared_body().decode("ascii"))
            snapshot.write_bytes(HARNESS.record_line(0, 0, body_json))
            with closing(sqlite3.connect(database)) as connection:
                with connection:
                    connection.execute("PRAGMA journal_mode=WAL")
                    connection.execute(
                        "CREATE TABLE index_meta(key TEXT PRIMARY KEY, value BLOB)"
                    )
                    connection.execute(
                        "INSERT INTO index_meta(key, value) VALUES ('active_generation', ?)",
                        (b"7",),
                    )

            def fake_ingest(*_args, **_kwargs):
                with closing(sqlite3.connect(database)) as writer:
                    with writer:
                        generation = HARNESS.active_generation(writer) + 1
                        writer.execute(
                            "UPDATE index_meta SET value = ? WHERE key = 'active_generation'",
                            (str(generation).encode("ascii"),),
                        )
                return {
                    "seconds": 1.0,
                    "peak_process_tree_rss_bytes": 1024,
                    "stdout_tail": "",
                }

            with mock.patch.object(HARNESS, "run_monitored", side_effect=fake_ingest):
                report = HARNESS.run_metadata_generations(
                    Path("hsum"), root, root, database, snapshot, manifest
                )

        readers = report["old_readers"]
        self.assertEqual(report["operations"]["count"], 2)
        self.assertEqual(readers["count"], 4)
        self.assertEqual(readers["prior_generations"], [8, 8, 8, 8])
        self.assertEqual(readers["retained_generations"], [8, 8, 8, 8])
        self.assertEqual(readers["fresh_reader_generation"], 9)

    def test_database_counts_cover_the_history_and_vector_tables(self):
        with tempfile.TemporaryDirectory() as directory:
            database = Path(directory) / "index.sqlite"
            with closing(sqlite3.connect(database)) as connection:
                with connection:
                    for table in (
                        "sources",
                        "project_sources",
                        "documents",
                        "document_versions",
                        "content_blobs",
                        "chunks",
                        "generations",
                        "chunk_embeddings",
                    ):
                        connection.execute(f"CREATE TABLE {table}(id INTEGER)")
                        connection.execute(f"INSERT INTO {table}(id) VALUES (1)")

            counts = HARNESS.final_database_counts(database)

        self.assertTrue(all(count == 1 for count in counts.values()))

    def test_cancel_storm_suppresses_late_responses_and_recovers(self):
        fake_server = r'''#!/usr/bin/env python3
import json
import sys

stages = (
    "query_embedding", "exact", "exact_fallback", "lexical",
    "vector", "fusion", "body_materialization", "total",
)
for line in sys.stdin:
    frame = json.loads(line)
    if frame.get("method") == "initialize":
        response = {"jsonrpc": "2.0", "id": frame["id"], "result": {}}
    elif frame.get("method") == "tools/call" and frame["id"] >= 100:
        arguments = frame["params"]["arguments"]
        mode = arguments["mode"]
        retrievers = ["exact", "lexical", "vector"] if mode == "hybrid" else ["exact", "lexical"]
        packet = {
            "requested_mode": mode,
            "effective_mode": mode,
            "degraded_mode": [],
            "results": [{}],
            "examined": {"exact": 0, "exact_fallback": 0, "lexical": 1, "vector": 1},
            "retrievers": retrievers,
            "stop_reason": "limit_reached",
            "timing_ms": {stage: 1 for stage in stages},
        }
        response = {
            "jsonrpc": "2.0",
            "id": frame["id"],
            "result": {"structuredContent": packet},
        }
    else:
        continue
    print(json.dumps(response), flush=True)
'''

        class FakeSampler:
            def __init__(self, _pid):
                self.peak_kib = 123
                self.error = None

            def start(self):
                return None

            def stop(self):
                return None

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "fake-hsum"
            binary.write_text(fake_server, encoding="utf-8")
            binary.chmod(0o755)
            with (
                mock.patch.object(HARNESS, "model_worker_pids", return_value=[111, 222]),
                mock.patch.object(HARNESS.os, "kill"),
                mock.patch.object(HARNESS.scale, "RssSampler", FakeSampler),
            ):
                report = HARNESS.run_cancel_storm(
                    binary, root, root, HARNESS.load_manifest()
                )

        self.assertEqual(report["cancelled_requests"], 8)
        self.assertEqual(report["real_worker_processes_paused"], 2)
        self.assertTrue(report["late_responses_suppressed"])
        self.assertTrue(report["lexical_recovery"])
        self.assertTrue(report["hybrid_recovery"])
        self.assertEqual(report["peak_process_tree_rss_bytes"], 123 * 1024)


if __name__ == "__main__":
    unittest.main()
