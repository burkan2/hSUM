from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from eval import harness


class MetricTests(unittest.TestCase):
    def test_span_scoring_uses_four_point_gain_and_grade_two_relevance(self) -> None:
        task = {
            "accepted": [
                harness.AcceptedSpan("answer.md", 10, 20, 3),
                harness.AcceptedSpan("context.md", 0, 8, 1),
                harness.AcceptedSpan("partial.md", 3, 9, 2),
            ]
        }
        ranked = [
            {"path": "context.md", "start_byte": 0, "end_byte": 8},
            {"path": "partial.md", "start_byte": 3, "end_byte": 9},
            {"path": "answer.md", "start_byte": 12, "end_byte": 16},
        ]
        metrics = harness.score_task(task, ranked)
        self.assertAlmostEqual(metrics["mrr@10"], 0.5)
        self.assertEqual(metrics["recall@10"], 1.0)
        self.assertEqual(metrics["exact_top3_recall"], 1.0)
        self.assertGreater(metrics["ndcg@10"], 0.0)
        self.assertLess(metrics["ndcg@10"], 1.0)

    def test_bootstrap_is_paired_seeded_and_nearest_ranked(self) -> None:
        first = harness.paired_bootstrap(
            [0.7, 0.3, 0.9, 0.2],
            [0.5, 0.4, 0.8, 0.2],
            seed=17,
            resamples=10_000,
            confidence=0.95,
        )
        second = harness.paired_bootstrap(
            [0.7, 0.3, 0.9, 0.2],
            [0.5, 0.4, 0.8, 0.2],
            seed=17,
            resamples=10_000,
            confidence=0.95,
        )
        self.assertEqual(first, second)
        self.assertAlmostEqual(first["estimate"], 0.05)


def result_fixture(*, semantic_gain: float) -> dict:
    tasks = []
    for index in range(100):
        subset = (
            "exact-token"
            if index < 35
            else "semantic-paraphrase"
            if index < 70
            else "scenario"
        )
        tasks.append({"id": f"task-{index}", "subset": subset})

    def rows(name: str) -> list[dict]:
        values = []
        for task in tasks:
            ndcg = 0.50
            mrr = 0.50
            if name == "hsum-semantic":
                ndcg = 0.40
                mrr = 0.40
            elif name == "hsum-hybrid":
                ndcg = 0.56
                mrr = 0.56
                if task["subset"] == "semantic-paraphrase":
                    ndcg = 0.50 + semantic_gain
            values.append(
                {
                    "task_id": task["id"],
                    "metrics": {
                        "ndcg@10": ndcg,
                        "mrr@10": mrr,
                        "exact_top3_recall": 1.0,
                    },
                }
            )
        return values

    return {
        "protocol": {
            "bootstrap_seed": 2_026_080_201,
            "bootstrap_resamples": 10_000,
            "confidence": 0.95,
            "noninferiority_margin": -0.02,
            "semantic_gain": 0.05,
        },
        "tasks": tasks,
        "retrievers": {
            name: {"tasks": rows(name)} for name in harness.HSUM_RETRIEVERS
        },
    }


class PromotionTests(unittest.TestCase):
    def test_promotion_requires_noninferiority_positive_value_and_exact_gate(self) -> None:
        decision = harness.promotion_decision(result_fixture(semantic_gain=0.10))
        self.assertTrue(decision["promoted"])
        self.assertEqual(decision["disposition"], "promote-hybrid")

        failed = harness.promotion_decision(result_fixture(semantic_gain=0.02))
        self.assertFalse(failed["promoted"])
        self.assertEqual(failed["disposition"], "stable-lexical-hybrid-beta")

    def test_exact_query_without_top_three_evidence_blocks_promotion(self) -> None:
        result = result_fixture(semantic_gain=0.10)
        result["retrievers"]["hsum-hybrid"]["tasks"][0]["metrics"][
            "exact_top3_recall"
        ] = 0.0
        decision = harness.promotion_decision(result)
        self.assertFalse(decision["promoted"])
        self.assertFalse(decision["exact_token"]["passed"])


class ComparisonTests(unittest.TestCase):
    def test_comparison_refuses_a_different_manifest(self) -> None:
        base = {
            "schema_version": harness.RESULT_SCHEMA,
            "evaluation_id": harness.EVALUATION_ID,
            "manifest_sha256": "a" * 64,
            "binary_commit": "b" * 40,
            "protocol": {},
            "tasks": [],
            "retrievers": {},
        }
        changed = {**base, "manifest_sha256": "c" * 64}
        with tempfile.TemporaryDirectory() as directory:
            left = Path(directory) / "left.json"
            right = Path(directory) / "right.json"
            left.write_text(json.dumps(base), encoding="utf-8")
            right.write_text(json.dumps(changed), encoding="utf-8")
            with self.assertRaisesRegex(harness.EvaluationError, "manifest_sha256 differs"):
                harness.compare_results(left, right)


class ObservationTests(unittest.TestCase):
    def test_qmd_command_preserves_a_leading_dash_query(self) -> None:
        command = harness.qmd_query_command(
            Path("/bin/qmd"),
            {"corpus": "operator-docs", "query": "--allow-unnotarized-alpha"},
        )
        self.assertEqual(command[-2:], ["--", "--allow-unnotarized-alpha"])

    def test_qmd_json_error_includes_bounded_diagnostics(self) -> None:
        with self.assertRaisesRegex(harness.EvaluationError, "stdout='not-json'"):
            harness.parse_qmd_json(
                {"stdout": "not-json", "stderr": "generation model was acquired"},
                "fixture",
            )

    def test_markdown_renderer_projects_persisted_evidence(self) -> None:
        result = result_fixture(semantic_gain=0.10)
        result.update(
            {
                "manifest_sha256": "a" * 64,
                "binary_commit": "b" * 40,
                "external_comparison_complete": False,
                "setup": {"hsum": {"setup_ms": 12.5}},
                "promotion": harness.promotion_decision(result),
            }
        )
        for retriever in result["retrievers"].values():
            retriever["aggregate"] = {
                "ndcg@10": 0.5,
                "mrr@10": 0.4,
                "recall@10": 0.3,
                "exact_top3_recall": 1.0,
                "latency_median_ms": 2.5,
                "context_bytes": 123,
                "citation_correctness": 1.0,
            }
        rendered = harness.render_result(result)
        self.assertIn("**promote-hybrid**", rendered)
        self.assertIn("| hsum-hybrid | 0.5000 | 0.4000 |", rendered)
        self.assertIn("path-scored", rendered)

    def test_materialized_corpus_has_a_repository_boundary(self) -> None:
        evaluation = harness.LoadedEvaluation(
            root=Path("/unused"),
            manifest={},
            manifest_sha256="0" * 64,
            corpora={"fixture": {"bodies": {"src/lib.rs": b"fn fixture() {}\n"}}},
            tasks=[],
        )
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "corpora"
            roots = harness.materialize_corpora(evaluation, output)
            self.assertTrue((roots["fixture"] / ".git").is_dir())
            self.assertEqual(
                (roots["fixture"] / "src/lib.rs").read_bytes(), b"fn fixture() {}\n"
            )

    def test_citation_correctness_round_trips_and_uses_the_cache(self) -> None:
        citation = "hsum://v1/index/source/document?rev=abc#bytes=0-4"
        completed = {
            "exit_code": 0,
            "stdout": json.dumps(
                {
                    "requested_citation_uri": citation,
                    "returned_citation_uri": citation,
                    "content": "body",
                }
            ),
            "stderr": "",
            "elapsed_ms": 1.0,
        }
        cache: dict[tuple[str, str], bool] = {}
        with mock.patch.object(harness, "run_process", return_value=completed) as run:
            first = harness.verify_hsum_citations(
                [{"citation_uri": citation}],
                hsum=Path("/bin/hsum"),
                root=Path("/corpus"),
                env={},
                cache=cache,
            )
            second = harness.verify_hsum_citations(
                [{"citation_uri": citation}],
                hsum=Path("/bin/hsum"),
                root=Path("/corpus"),
                env={},
                cache=cache,
            )
        self.assertEqual(first, 1.0)
        self.assertEqual(second, 1.0)
        run.assert_called_once()

    def test_result_writer_refuses_to_replace_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "result.json"
            harness.write_json(output, {"first": True})
            with self.assertRaisesRegex(harness.EvaluationError, "refusing to overwrite"):
                harness.write_json(output, {"second": True})
            self.assertEqual(json.loads(output.read_text()), {"first": True})


if __name__ == "__main__":
    unittest.main()
