import json
import pathlib
import sys
import tempfile
import unittest
from unittest import mock

from benches.retrieval_determinism import harness


class ManifestTests(unittest.TestCase):
    def test_manifest_and_corpus_fingerprints_are_frozen(self):
        manifest = harness.load_manifest()

        self.assertEqual(
            harness.manifest_sha256(),
            "c449d6c9c53d80c7992de70fc1374c9583319f8a41f150c2e99006cc7a6dad4f",
        )
        self.assertEqual(
            harness.corpus_fingerprint(manifest),
            "c68db9681656bdae88bdd4a77a969a19332f1f4d2d56297cc887d8c2e85bc8ae",
        )
        self.assertEqual(manifest["runs_per_transport"], 5)
        self.assertEqual(len(manifest["queries"]), 5)


class NormalizationTests(unittest.TestCase):
    def test_cli_and_mcp_packets_normalize_to_one_evidence_contract(self):
        cli = packet("cli")
        mcp = packet("mcp")

        self.assertEqual(
            harness.normalize_packet(cli, "cli"),
            harness.normalize_packet(mcp, "mcp"),
        )
        duplicates = harness.normalize_packet(cli, "cli")["results"][0][
            "duplicate_citations"
        ]
        self.assertEqual(
            [item["citation_uri"] for item in duplicates],
            ["hsum://duplicate-a", "hsum://duplicate-b"],
        )

    def test_same_target_run_drift_is_rejected(self):
        baseline = {"query": {"results": ["a", "b"]}}
        changed = {"query": {"results": ["b", "a"]}}

        with self.assertRaisesRegex(harness.QualificationError, "run 2 changed"):
            harness.require_equal_runs([baseline, changed], "CLI")

    def test_empty_results_and_missing_equal_score_ties_fail_closed(self):
        empty = packet("cli")
        empty["results"] = []
        with self.assertRaisesRegex(harness.QualificationError, "no evidence"):
            harness.normalize_packet(empty, "cli")

        packet_without_tie = {
            "results": [
                {
                    "score": {
                        "lists": [
                            {"retriever": "lexical", "backend_score": -1.0},
                            {"retriever": "lexical", "backend_score": -2.0},
                        ]
                    }
                }
            ]
        }
        self.assertFalse(harness.has_equal_backend_score(packet_without_tie))
        packet_without_tie["results"][0]["score"]["lists"][1][
            "backend_score"
        ] = -1.0
        self.assertTrue(harness.has_equal_backend_score(packet_without_tie))


class ComparisonTests(unittest.TestCase):
    def test_cross_target_comparison_allows_only_bounded_numeric_drift(self):
        left = {
            "backend_score": 0.125,
            "fused": 0.42,
            "citations": ["a", "b"],
        }
        right = {
            "backend_score": 0.1250005,
            "fused": 0.42,
            "citations": ["a", "b"],
        }
        self.assertAlmostEqual(harness.compare_values(left, right), 0.0000005)

        with self.assertRaisesRegex(harness.QualificationError, "numeric cross-target drift"):
            harness.compare_values(
                left,
                {
                    "backend_score": 0.126,
                    "fused": 0.42,
                    "citations": ["a", "b"],
                },
            )
        with self.assertRaisesRegex(harness.QualificationError, "cross-target mismatch"):
            harness.compare_values(
                left,
                {
                    "backend_score": 0.125,
                    "fused": 0.42,
                    "citations": ["b", "a"],
                },
            )
        with self.assertRaisesRegex(harness.QualificationError, "cross-target mismatch"):
            harness.compare_values(
                left,
                {
                    "backend_score": 0.125,
                    "fused": 0.4200005,
                    "citations": ["a", "b"],
                },
            )

    def test_native_reports_require_one_index_and_identical_citation_order(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            linux_path = root / "linux.json"
            macos_path = root / "macos.json"
            output = root / "comparison.json"
            linux = report("linux", "x86_64", ["hsum://one", "hsum://two"])
            macos = report("macos", "arm64", ["hsum://one", "hsum://two"])
            linux_path.write_text(json.dumps(linux), encoding="utf-8")
            macos_path.write_text(json.dumps(macos), encoding="utf-8")

            harness.compare_reports(linux_path, macos_path, output)

            comparison = json.loads(output.read_text(encoding="utf-8"))
            self.assertTrue(comparison["passed"])
            self.assertTrue(comparison["lexical_citation_order_identical"])
            self.assertEqual(comparison["maximum_numeric_delta"], 0.0)


class DispatchTests(unittest.TestCase):
    def test_prepare_and_run_dispatch_every_path_to_the_correct_slot(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            binary = root / "hsum"
            home = root / "home"
            repository = root / "repository"
            fixture = root / "fixture.json"
            output = root / "output.json"

            with mock.patch.object(harness, "prepare_fixture") as prepare:
                with mock.patch.object(
                    sys,
                    "argv",
                    [
                        "harness.py",
                        "prepare",
                        "--hsum",
                        str(binary),
                        "--home",
                        str(home),
                        "--repository",
                        str(repository),
                        "--output",
                        str(output),
                    ],
                ):
                    self.assertEqual(harness.main(), 0)
                prepare.assert_called_once_with(
                    binary.resolve(),
                    home.resolve(),
                    repository.resolve(),
                    output.resolve(),
                )

            with mock.patch.object(harness, "run_qualification") as run:
                with mock.patch.object(
                    sys,
                    "argv",
                    [
                        "harness.py",
                        "run",
                        "--hsum",
                        str(binary),
                        "--home",
                        str(home),
                        "--repository",
                        str(repository),
                        "--fixture-report",
                        str(fixture),
                        "--output",
                        str(output),
                    ],
                ):
                    self.assertEqual(harness.main(), 0)
                run.assert_called_once_with(
                    binary.resolve(),
                    home.resolve(),
                    repository.resolve(),
                    fixture.resolve(),
                    output.resolve(),
                )


def packet(transport):
    result = {
        "citation_uri": "hsum://one",
        "index_id": "index",
        "source_id": "source",
        "document_id": "document",
        "revision_sha256": "22" * 32,
        "source_uri": "repo://alpha.md",
        "title": "alpha.md",
        "content": "alpha body",
        "content_sha256": "11" * 32,
        "source_updated_at": None,
        "indexed_at": "2026-01-01T00:00:00Z",
        "head_generation": 1,
        "source_state": "metadata_unchanged",
        "untrusted_content": True,
        "duplicate_citations": [
            {"citation_uri": "hsum://duplicate-b", "reason": "same_content"},
            {"citation_uri": "hsum://duplicate-a", "reason": "same_content"},
        ],
        "score": {
            "fusion_units": 42,
            "fused": 0.42,
            "lists": [
                {
                    "rank": 1,
                    "contribution_units": 42,
                    "backend_score": -1.25,
                }
            ],
        },
    }
    if transport == "cli":
        result["span"] = {
            "start_byte": 0,
            "end_byte": 10,
            "start_line": 1,
            "end_line": 1,
        }
        result["score"]["lists"][0]["name"] = "lexical"
    else:
        result["byte_span"] = {"start": 0, "end": 10}
        result["line_span"] = {"start": 1, "end": 1}
        result["score"]["lists"][0]["retriever"] = "lexical"
    return {
        "schema_version": "hsum.api.v1",
        "generation": 1,
        "index_epoch": 1,
        "project_id": "project",
        "scope_revision": 0,
        "requested_mode": "lexical",
        "effective_mode": "lexical",
        "retrievers": ["lexical"],
        "degraded_mode": [],
        "hints": [],
        "stop_reason": "exhausted",
        "next_cursor": None,
        "examined": {"exact": 0, "exact_fallback": 0, "lexical": 1, "vector": 0},
        "timing_ms": {
            "query_embedding": 0,
            "exact": 0,
            "exact_fallback": 0,
            "lexical": 1,
            "vector": 0,
            "fusion": 0,
            "body_materialization": 0,
            "total": 1,
        },
        "results": [result],
    }


def report(system, architecture, citations):
    return {
        "schema_version": harness.SCHEMA_VERSION,
        "passed": True,
        "commit_sha": "commit",
        "platform": {"os": system, "arch": architecture},
        "manifest_sha256": "manifest",
        "corpus_fingerprint_sha256": "corpus",
        "index_sha256": "index",
        "binary_sha256": architecture,
        "binary_version": "hsum 0.1.0",
        "runs_per_transport": 5,
        "queries": ["query"],
        "checks": {
            "cli_fresh_processes_identical": True,
            "mcp_fresh_processes_identical": True,
            "cli_mcp_equivalent": True,
            "lexical_query_embedding_absent": True,
            "duplicate_set_exercised": True,
            "multiple_explanation_lists_exercised": True,
            "equal_backend_score_tie_exercised": True,
        },
        "observations": {"query": {"citations": citations, "score": 0.125}},
    }


if __name__ == "__main__":
    unittest.main()
