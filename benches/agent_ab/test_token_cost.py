import copy
import hashlib
import json
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
import token_cost


class TokenCostTests(unittest.TestCase):
    def test_schema_adaptation_keeps_answer_constraints(self):
        path = Path(__file__).with_name("agent_output.schema.json")
        original = json.loads(path.read_text())
        adapted = token_cost.answer_schema(path)
        self.assertEqual(adapted.pop("$schema"), "http://json-schema.org/draft-07/schema#")
        original.pop("$schema")
        self.assertEqual(adapted, original)

    def test_native_evidence_requires_an_observed_grep_path(self):
        answer = {"evidence": [{"path": "src/a.rs"}]}
        self.assertTrue(token_cost.native_evidence_present(answer, "src/a.rs-4-context\n"))
        self.assertTrue(token_cost.native_evidence_present(answer, "src/a.rs:5:match\n"))
        self.assertFalse(token_cost.native_evidence_present(answer, "elsewhere:5:src/a.rs\n"))
        self.assertFalse(token_cost.native_evidence_present({"evidence": []}, ""))

    def event(self):
        return {"type": "result", "subtype": "success", "is_error": False,
                "structured_output": {"answer": "supported", "evidence": []},
                "usage": {"input_tokens": 7, "cache_creation_input_tokens": 10,
                          "cache_read_input_tokens": 20, "output_tokens": 5,
                          "iterations": [{"input_tokens": 999}]},
                "total_cost_usd": 0.01, "modelUsage": {token_cost.MODEL: {}}}

    def test_aggregate_usage_includes_cache_without_double_counting_iterations(self):
        _, metrics, errors = token_cost.result_metrics([self.event()], 0)
        self.assertEqual(metrics["total_input_tokens"], 37)
        self.assertEqual(metrics["output_tokens"], 5)
        self.assertEqual(errors, [])

    def test_missing_or_invalid_usage_never_becomes_zero_cost(self):
        for value in (-1, True, float("nan")):
            event = self.event()
            event["total_cost_usd"] = value
            with self.assertRaises(ValueError):
                token_cost.result_metrics([event], 0)
        event = self.event()
        del event["usage"]["cache_read_input_tokens"]
        with self.assertRaises(KeyError):
            token_cost.result_metrics([event], 0)

    def test_client_failure_keeps_cost_but_fails_protocol(self):
        event = self.event()
        event.update(is_error=True, subtype="error_max_budget_usd")
        _, metrics, errors = token_cost.result_metrics([event], 1)
        self.assertEqual(metrics["reported_cost_usd"], 0.01)
        self.assertTrue(errors)

    def test_tools_and_model_changes_invalidate_comparison(self):
        event = self.event()
        event["modelUsage"] = {"different-model": {}}
        call = {"type": "assistant", "message": {"content": [{"type": "tool_use", "name": "Read"}]}}
        _, _, errors = token_cost.result_metrics([call, event], 0)
        self.assertEqual(len(errors), 2)
        structured = copy.deepcopy(call)
        structured["message"]["content"][0]["name"] = "StructuredOutput"
        _, _, errors = token_cost.result_metrics([structured, self.event()], 0)
        self.assertFalse(errors)

    def test_missing_or_foreign_citations_fail_without_spawning_processes(self):
        checks = token_cost.citation_checks({"evidence": [{"path": "a", "citation_uri": None},
            {"path": "a", "citation_uri": "hsum://v1/foreign"}]}, bundle="", hsum=Path("missing"), repo=Path.cwd())
        self.assertEqual(checks, [False, False])

    def test_citation_verification_checks_whole_body_hash_and_returned_window(self):
        citation = "hsum://v1/index/source/document?rev=revision#bytes=7-11"
        answer = {"evidence": [{"path": "a.rs", "citation_uri": citation}]}
        body = b"before\nused\nafter\n"
        packet = {"source_uri": "repo://a.rs", "source_hash_verification": "unchanged",
                  "requested_citation_uri": citation, "returned_citation_uri": citation,
                  "content": "used", "body_sha256": hashlib.sha256(body).hexdigest()}
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "a.rs").write_bytes(body)
            variants = [(packet, True)]
            for field, value in (("content", "fake"), ("body_sha256", "wrong"),
                                 ("source_hash_verification", "changed"), ("source_uri", "repo://../a.rs"),
                                 ("requested_citation_uri", "different"),
                                 ("returned_citation_uri", citation.replace("7-11", "7-99")),
                                 ("returned_citation_uri", citation.replace("revision", "other"))):
                variants.append(({**packet, field: value}, False))
            for observed, expected in variants:
                with self.subTest(packet=observed), patch.object(token_cost.benchmark, "run_process",
                    return_value={"returncode": 0, "stdout": json.dumps(observed)}):
                    self.assertEqual(token_cost.citation_checks(answer, bundle=f"citation: {citation}\n",
                        hsum=Path("hsum"), repo=root), [expected])


if __name__ == "__main__":
    unittest.main()
