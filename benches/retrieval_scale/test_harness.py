import importlib.util
import sqlite3
import tempfile
import time
import unittest
from pathlib import Path


HARNESS_PATH = Path(__file__).with_name("harness.py")
SPEC = importlib.util.spec_from_file_location("retrieval_scale_harness", HARNESS_PATH)
assert SPEC is not None and SPEC.loader is not None
HARNESS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HARNESS)


class RetrievalScaleHarnessTests(unittest.TestCase):
    def test_frozen_query_manifest_has_750_observations_per_run(self):
        queries = HARNESS.load_queries()

        self.assertEqual(len(queries), 25)
        self.assertEqual(len(queries) * HARNESS.CANONICAL_PASSES, 750)

    def test_corpus_body_has_exact_frozen_size_and_chunk_count(self):
        queries = HARNESS.load_queries()
        body = HARNESS.corpus_body(19, queries)

        self.assertEqual(len(body), HARNESS.DOCUMENT_BYTES)
        self.assertFalse(any(character in body for character in b"\n\r.!?"))
        self.assertEqual(
            HARNESS.unbroken_chunk_count(len(body)),
            HARNESS.EXPECTED_CHUNKS_PER_DOCUMENT,
        )
        self.assertEqual(
            HARNESS.DOCUMENT_COUNT * HARNESS.EXPECTED_CHUNKS_PER_DOCUMENT,
            HARNESS.EXPECTED_PASSAGES,
        )

    def test_nearest_rank_uses_one_based_ceiling(self):
        values = list(range(1, 31))

        self.assertEqual(HARNESS.nearest_rank(values, 0.50), 15)
        self.assertEqual(HARNESS.nearest_rank(values, 0.95), 29)
        self.assertEqual(HARNESS.nearest_rank(values, 1.00), 30)

    def test_class_summary_keeps_stage_and_client_latency_separate(self):
        observations = []
        for query_class in HARNESS.CLASS_SLOS_MS:
            for sample in range(1, 4):
                observations.append(
                    {
                        "class": query_class,
                        "client_round_trip_ms": sample + 0.5,
                        "timing_ms": {stage: sample for stage in HARNESS.STAGES},
                        "examined": {
                            "exact": sample,
                            "exact_fallback": 0,
                            "lexical": sample + 1,
                            "vector": sample + 2,
                        },
                        "stop_reason": "limit_reached",
                    }
                )

        summary = HARNESS.summarize_observations(observations)

        identifier = summary["classes"]["identifier"]
        self.assertEqual(identifier["latency_ms"]["total"]["p95"], 3)
        self.assertEqual(identifier["latency_ms"]["client_round_trip"]["worst"], 3.5)
        self.assertEqual(identifier["max_examined"]["vector"], 5)

    def test_qualification_requires_canonical_cold_and_setup_evidence(self):
        summary = {
            "all_total_ms": {"p95": 10},
            "classes": {
                query_class: {"latency_ms": {"total": {"p95": 10}}}
                for query_class in HARNESS.CLASS_SLOS_MS
            },
        }
        runs = [
            {
                "fresh_process": True,
                "observation_count": 750,
                "peak_process_tree_rss_bytes": 1024,
                "rss_sampling_error": None,
                "summary": summary,
                "class_slos": {
                    query_class: {"pass": True}
                    for query_class in HARNESS.CLASS_SLOS_MS
                },
            }
            for _ in range(3)
        ]
        setup = {"ingest": {"passages_per_second": 1000.0}}

        passing = HARNESS.qualify_report(
            runs,
            canonical_parameters=True,
            setup=setup,
            cold_cache_available=True,
        )
        missing_cold = HARNESS.qualify_report(
            runs,
            canonical_parameters=True,
            setup=setup,
            cold_cache_available=False,
        )

        self.assertTrue(passing["pass"])
        self.assertFalse(missing_cold["pass"])

    def test_coefficient_of_variation_is_population_based(self):
        self.assertAlmostEqual(
            HARNESS.coefficient_of_variation_percent([90.0, 100.0, 110.0]),
            8.16496580927726,
        )

    def test_packet_validation_freezes_fallback_and_hybrid_classification(self):
        queries = HARNESS.load_queries()
        fallback = next(query for query in queries if query["class"] == "exact_fallback")
        hybrid = next(query for query in queries if query["class"] == "hybrid_no_fallback")
        timing = {stage: 1 for stage in HARNESS.STAGES}
        fallback_packet = {
            "requested_mode": "lexical",
            "effective_mode": "lexical",
            "degraded_mode": [],
            "results": [{}],
            "retrievers": ["exact_fallback", "lexical"],
            "timing_ms": timing,
        }
        hybrid_packet = {
            "requested_mode": "hybrid",
            "effective_mode": "hybrid",
            "degraded_mode": [],
            "results": [{}],
            "retrievers": ["lexical", "vector"],
            "timing_ms": timing,
        }

        HARNESS.validate_search_packet(fallback, fallback_packet)
        HARNESS.validate_search_packet(hybrid, hybrid_packet)
        hybrid_packet["retrievers"].append("exact_fallback")
        with self.assertRaises(HARNESS.HarnessError):
            HARNESS.validate_search_packet(hybrid, hybrid_packet)

    def test_storage_metrics_separate_live_history_and_vector_payloads(self):
        with tempfile.TemporaryDirectory() as directory:
            database_path = Path(directory) / "index.sqlite"
            connection = sqlite3.connect(database_path)
            connection.executescript(
                """
                CREATE TABLE content_blobs(id INTEGER PRIMARY KEY, original_bytes BLOB NOT NULL);
                CREATE TABLE document_versions(
                    id INTEGER PRIMARY KEY,
                    content_blob_id INTEGER NOT NULL
                );
                CREATE TABLE document_heads(
                    document_version_id INTEGER,
                    state TEXT NOT NULL
                );
                CREATE TABLE chunk_embeddings(vector_blob BLOB NOT NULL);
                """
            )
            connection.executemany(
                "INSERT INTO content_blobs(id, original_bytes) VALUES (?, ?)",
                [(1, b"live"), (2, b"history"), (3, b"unused")],
            )
            connection.executemany(
                "INSERT INTO document_versions(id, content_blob_id) VALUES (?, ?)",
                [(10, 1), (11, 2)],
            )
            connection.execute(
                "INSERT INTO document_heads(document_version_id, state) VALUES (10, 'active')"
            )
            connection.executemany(
                "INSERT INTO chunk_embeddings(vector_blob) VALUES (?)",
                [(b"1234",), (b"567890",)],
            )
            connection.commit()
            connection.close()

            metrics = HARNESS.storage_metrics(
                database_path,
                {
                    "active_passages": 2,
                    "storage": {"managed_index_bytes": 123, "reclaimable_bytes": 7},
                },
            )

        self.assertEqual(metrics["logical_live_bytes"], len(b"live"))
        self.assertEqual(metrics["logical_history_bytes"], len(b"history"))
        self.assertEqual(metrics["logical_cached_vector_payload_bytes"], 10)
        self.assertEqual(metrics["logical_active_vector_payload_bytes"], 2 * 384 * 4)
        self.assertEqual(metrics["logical_total_vector_payload_bytes"], 10 + 2 * 384 * 4)
        self.assertEqual(metrics["managed_index_bytes"], 123)

    def test_mcp_session_uses_bounded_line_framing_and_closes_cleanly(self):
        fake_server = r'''#!/usr/bin/env python3
import json
import sys

for line in sys.stdin:
    frame = json.loads(line)
    if frame.get("method") == "initialize":
        response = {"jsonrpc": "2.0", "id": frame["id"], "result": {}}
    elif frame.get("method") == "tools/call":
        arguments = frame["params"]["arguments"]
        timing = {
            "query_embedding": 0,
            "exact": 1,
            "exact_fallback": 1,
            "lexical": 1,
            "vector": 0,
            "fusion": 1,
            "body_materialization": 1,
            "total": 5,
        }
        packet = {
            "requested_mode": arguments["mode"],
            "effective_mode": arguments["mode"],
            "degraded_mode": [],
            "results": [{}],
            "retrievers": ["exact_fallback", "lexical"],
            "timing_ms": timing,
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
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "fake-hsum"
            binary.write_text(fake_server, encoding="utf-8")
            binary.chmod(0o755)
            session = HARNESS.McpSession(binary, root, root)
            time.sleep(HARNESS.RSS_SAMPLE_INTERVAL_SECONDS * 2)
            fallback = next(
                query
                for query in HARNESS.load_queries()
                if query["class"] == "exact_fallback"
            )

            packet, client_ms = session.search(fallback)
            HARNESS.validate_search_packet(fallback, packet)
            peak_rss_kib, rss_error = session.close()

        self.assertGreater(client_ms, 0)
        self.assertGreater(peak_rss_kib, 0)
        self.assertIsNone(rss_error)


if __name__ == "__main__":
    unittest.main()
