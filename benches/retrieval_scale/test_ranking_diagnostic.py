"""Failure-retention checks for the reduced diagnostic orchestrator."""

import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from benches.retrieval_scale import ranking_diagnostic as diagnostic


class DiagnosticEvidenceTests(unittest.TestCase):
    def run_diagnostic(self, root, *, fail_candidate=False, fail_start=False, wrong_corpus=False, identical_binary=False):
        output = root / "report.json"
        binaries = [root / "baseline", root / "candidate"]
        for binary in binaries:
            binary.write_bytes(b"same" if identical_binary else binary.name.encode())

        corpus = root / "corpus"
        corpus.mkdir()
        (corpus / ".hsum-retrieval-scale-corpus.json").write_text(json.dumps({
            "fingerprint_sha256": "wrong" if wrong_corpus else diagnostic.h.corpus_fingerprint(diagnostic.h.load_queries())}))

        class Session:
            def __init__(self, binary, corpus, home):
                if fail_start:
                    raise RuntimeError("startup failed")
                self.binary = binary

            def search(self, query):
                changed = fail_candidate and self.binary.name == "candidate"
                return {"results": [{"citation": "changed" if changed else query["id"]}],
                        "timing_ms": {stage: 1 for stage in diagnostic.h.STAGES}}, 1

            def close(self):
                return 100, None

        arguments = ["diagnostic", "--baseline", str(binaries[0]), "--candidate", str(binaries[1]),
                     "--home", str(root / "home"), "--corpus", str(root / "corpus"),
                     "--output", str(output)]
        with patch("sys.argv", arguments), patch.object(diagnostic.h, "McpSession", Session), \
                patch.object(diagnostic.h, "run_json_command", return_value={"active_passages": 100000}), \
                patch.object(diagnostic.h, "git_revision", return_value={}), \
                patch.object(diagnostic.h, "machine_report", return_value={}), \
                patch.object(diagnostic.h, "validate_search_packet"), patch("builtins.print"):
            if fail_candidate or fail_start or wrong_corpus or identical_binary:
                with self.assertRaises(RuntimeError):
                    diagnostic.main()
            else:
                self.assertEqual(diagnostic.main(), 0)
        return json.loads(output.read_text()) if output.exists() else None

    def test_success_retains_recomputable_equivalence_for_every_call(self):
        with tempfile.TemporaryDirectory() as directory:
            report = self.run_diagnostic(Path(directory))
        self.assertFalse(report["qualification"])
        self.assertTrue(report["completed"])
        self.assertEqual(len(report["phases"]), 3)
        for phase in report["phases"]:
            self.assertTrue(phase["completed"])
            self.assertEqual(len(phase["calls"]), 12 * 19)
            self.assertEqual(len(phase["observations"]), 10 * 19)
            self.assertEqual(phase["peak_process_tree_rss_bytes"], 102400)
            for call in phase["calls"]:
                expected = report["baseline_results"][call["query_id"]]
                self.assertEqual(call["result_sha256"], diagnostic.h.sha256_bytes(diagnostic.h.canonical_json_bytes(expected)))

    def test_mismatch_retains_prior_calls_and_the_actual_failed_packet(self):
        with tempfile.TemporaryDirectory() as directory:
            report = self.run_diagnostic(Path(directory), fail_candidate=True)
        self.assertFalse(report["completed"])
        self.assertTrue(report["phases"][0]["completed"])
        failure = report["phases"][1]
        self.assertFalse(failure["completed"])
        self.assertIn("changed", failure["error"])
        self.assertEqual(failure["calls"][0]["failed_packet"]["results"], [{"citation": "changed"}])

    def test_identical_binary_comparison_is_rejected_before_search(self):
        with tempfile.TemporaryDirectory() as directory:
            self.assertIsNone(self.run_diagnostic(Path(directory), identical_binary=True))

    def test_wrong_corpus_is_rejected_before_search(self):
        with tempfile.TemporaryDirectory() as directory:
            self.assertIsNone(self.run_diagnostic(Path(directory), wrong_corpus=True))

    def test_startup_failure_still_retains_an_incomplete_report(self):
        with tempfile.TemporaryDirectory() as directory:
            report = self.run_diagnostic(Path(directory), fail_start=True)
        self.assertFalse(report["completed"])
        self.assertIn("startup failed", report["phases"][0]["error"])


if __name__ == "__main__":
    unittest.main()
