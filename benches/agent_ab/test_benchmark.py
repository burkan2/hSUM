#!/usr/bin/env python3

import json
import math
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import benchmark

BENCH_DIR = Path(__file__).resolve().parent


class RetrievalMetricTests(unittest.TestCase):
    def test_rank_metrics_treat_positive_grades_as_relevant(self):
        relevance = [1, 0, 1]

        self.assertAlmostEqual(benchmark.precision_at_k(relevance, 3), 2 / 3)
        self.assertAlmostEqual(benchmark.recall_at_k(relevance, 3, relevant_total=4), 0.5)
        self.assertEqual(benchmark.hit_at_k(relevance, 1), 1.0)
        self.assertEqual(benchmark.reciprocal_rank(relevance), 1.0)

        expected_dcg = 1.0 + (1.0 / math.log2(4))
        expected_idcg = 1.0 + (1.0 / math.log2(3)) + (1.0 / math.log2(4))
        self.assertAlmostEqual(
            benchmark.ndcg_at_k(relevance, 3, relevant_total=4),
            expected_dcg / expected_idcg,
        )

    def test_ndcg_rewards_authoritative_documents_ranked_first(self):
        authoritative_first = benchmark.ndcg_at_k(
            [2, 1], 2, ideal_relevance=[2, 1]
        )
        supporting_first = benchmark.ndcg_at_k(
            [1, 2], 2, ideal_relevance=[2, 1]
        )

        self.assertEqual(authoritative_first, 1.0)
        self.assertLess(supporting_first, authoritative_first)

    def test_empty_results_score_zero_without_division_errors(self):
        self.assertEqual(benchmark.precision_at_k([], 5), 0.0)
        self.assertEqual(benchmark.recall_at_k([], 5, relevant_total=1), 0.0)
        self.assertEqual(benchmark.hit_at_k([], 5), 0.0)
        self.assertEqual(benchmark.reciprocal_rank([]), 0.0)
        self.assertEqual(benchmark.ndcg_at_k([], 5, relevant_total=1), 0.0)

    def test_duplicate_passages_do_not_inflate_document_recall(self):
        ranked = [
            {"source_uri": "repo://src/mcp.rs"},
            {"source_uri": "repo://src/mcp.rs"},
            {"source_uri": "repo://README.md"},
        ]

        relevance = benchmark.document_relevance(
            ranked,
            {"src/mcp.rs", "README.md"},
            limit=3,
        )

        self.assertEqual(relevance, [1, 1])


class AnswerScoringTests(unittest.TestCase):
    def test_answer_score_requires_every_declared_fact(self):
        task = {
            "required_patterns": [r"init --rebuild", r"fingerprint"],
            "relevant_paths": ["README.md"],
        }
        response = {
            "answer": "Use hsum init --rebuild after a fingerprint mismatch.",
            "evidence": [{"path": "README.md", "citation_uri": "hsum://v1/example"}],
        }

        score = benchmark.score_agent_response(task, response, citation_checks=[True])

        self.assertEqual(score["fact_accuracy"], 1.0)
        self.assertEqual(score["evidence_precision"], 1.0)
        self.assertEqual(score["citation_validity"], 1.0)
        self.assertEqual(score["task_success"], 1.0)

    def test_correct_guess_without_relevant_evidence_is_not_success(self):
        task = {
            "required_patterns": [r"init --rebuild"],
            "relevant_paths": ["README.md"],
        }
        response = {
            "answer": "Use hsum init --rebuild.",
            "evidence": [{"path": "TODOS.md"}],
        }

        score = benchmark.score_agent_response(task, response, citation_checks=[])

        self.assertEqual(score["fact_accuracy"], 1.0)
        self.assertEqual(score["evidence_precision"], 0.0)
        self.assertEqual(score["task_success"], 0.0)

    def test_hsum_condition_requires_at_least_one_resolvable_citation(self):
        task = {
            "required_patterns": [r"init --rebuild"],
            "relevant_paths": ["README.md"],
        }
        response = {
            "answer": "Use hsum init --rebuild.",
            "evidence": [{"path": "README.md", "citation_uri": None}],
        }

        score = benchmark.score_agent_response(
            task,
            response,
            citation_checks=[],
            require_citation=True,
        )

        self.assertEqual(score["citation_validity"], 0.0)
        self.assertEqual(score["task_success"], 0.0)


class ResultContractTests(unittest.TestCase):
    def test_checked_in_manifest_has_frozen_shape_and_existing_paths(self):
        manifest = benchmark.load_task_manifest(
            BENCH_DIR / "tasks.json", repo=BENCH_DIR.parents[1]
        )

        self.assertEqual(manifest["benchmark_id"], benchmark.GOLD_BENCHMARK_ID)
        self.assertEqual(len(manifest["tasks"]), 25)
        self.assertEqual(
            manifest["protocol"]["baseline_parameters"],
            benchmark.GOLD_BASELINE_PROTOCOL,
        )
        self.assertEqual(
            {name: sum(task["class"] == name for task in manifest["tasks"])
             for name in benchmark.GOLD_CLASS_DISTRIBUTION},
            benchmark.GOLD_CLASS_DISTRIBUTION,
        )
        self.assertTrue(
            all(set(task["relevance_grades"].values()) <= {1, 2}
                for task in manifest["tasks"])
        )

    def test_checked_in_manifest_matches_frozen_checksum(self):
        digest = benchmark.validate_manifest_checksum(
            BENCH_DIR / "tasks.json", BENCH_DIR / "tasks.sha256"
        )

        self.assertEqual(len(digest), 64)

    def test_load_tasks_rejects_duplicate_ids(self):
        document = json.loads((BENCH_DIR / "tasks.json").read_text(encoding="utf-8"))
        document["tasks"][1]["id"] = document["tasks"][0]["id"]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "tasks.json"
            path.write_text(json.dumps(document), encoding="utf-8")

            with self.assertRaisesRegex(ValueError, "duplicate task id"):
                benchmark.load_tasks(path)

    def test_load_tasks_rejects_invalid_grade(self):
        document = json.loads((BENCH_DIR / "tasks.json").read_text(encoding="utf-8"))
        document["tasks"][0]["judgments"][0]["grade"] = 3
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "tasks.json"
            path.write_text(json.dumps(document), encoding="utf-8")

            with self.assertRaisesRegex(ValueError, "grades must be 1 or 2"):
                benchmark.load_tasks(path)

    def test_ripgrep_runner_is_stably_ranked(self):
        command = benchmark.retriever_command(
            "ripgrep",
            query="needle",
            scope=["src"],
            hsum=Path("hsum"),
            limit=5,
            context_lines=10,
        )
        self.assertEqual(command[1:5], ["--threads", "1", "--sort", "path"])

    def test_git_grep_includes_untracked_files_from_the_same_working_tree(self):
        command = benchmark.retriever_command(
            "git-grep",
            query="needle",
            scope=["src"],
            hsum=Path("hsum"),
            limit=5,
            context_lines=10,
        )

        self.assertIn("--untracked", command)
        self.assertIn("--exclude-standard", command)

    def test_retrieval_rejects_protocol_drift(self):
        arguments = benchmark.parser().parse_args(["retrieval", "--runs", "1"])

        with self.assertRaisesRegex(ValueError, "frozen baseline protocol"):
            benchmark.run_retrieval(arguments)

    def test_materialized_corpus_excludes_benchmark_files(self):
        original_scope = benchmark.DEFAULT_SCOPE
        try:
            benchmark.DEFAULT_SCOPE = ["src", "README.md"]
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                source = root / "source"
                destination = root / "snapshot"
                (source / "src").mkdir(parents=True)
                (source / "src/lib.rs").write_text("pub fn evidence() {}\n", encoding="utf-8")
                (source / "README.md").write_text("evidence\n", encoding="utf-8")
                (source / "benches").mkdir()
                (source / "benches/tasks.json").write_text("leaked query\n", encoding="utf-8")

                benchmark.materialize_corpus(source, destination)

                self.assertTrue((destination / "src/lib.rs").is_file())
                self.assertFalse((destination / "benches").exists())
                self.assertEqual(benchmark.git_state(destination)[1], False)
        finally:
            benchmark.DEFAULT_SCOPE = original_scope

    def test_build_summary_retains_mean_and_range(self):
        summary, ranges = benchmark.summarize_build_metrics(
            [
                {"task_count": 25, "hit@1": 0.36, "payload_bytes": 100},
                {"task_count": 25, "hit@1": 0.44, "payload_bytes": 120},
                {"task_count": 25, "hit@1": 0.40, "payload_bytes": 110},
            ]
        )

        self.assertEqual(summary["task_count"], 25)
        self.assertAlmostEqual(summary["hit@1"], 0.40)
        self.assertEqual(summary["payload_bytes"], 110)
        self.assertEqual(ranges["hit@1"], {"min": 0.36, "max": 0.44})

    def test_precollected_context_respects_byte_budget(self):
        original = benchmark.run_process
        try:
            benchmark.run_process = lambda *args, **kwargs: {
                "returncode": 0,
                "stdout": "é" * 100,
                "stderr": "",
                "elapsed_ms": 1.0,
            }
            collected = benchmark.collect_evidence_bundle(
                task={"id": "x", "query": "x"},
                condition="native",
                repo=Path.cwd(),
                hsum=Path("hsum"),
                limit=5,
                context_lines=10,
                byte_budget=21,
            )
        finally:
            benchmark.run_process = original

        self.assertLessEqual(collected["context_bytes"], 21)
        collected["bundle"].encode("utf-8")

    def test_extract_integer_constant_is_name_scoped(self):
        text = "const OTHER: u32 = 2;\npub const RETRY_LIMIT: u32 = 9;\n"
        self.assertEqual(benchmark.extract_integer_constant(text, "RETRY_LIMIT"), 9)
        self.assertIsNone(benchmark.extract_integer_constant(text, "MISSING"))

    def test_svg_renderer_emits_shareable_asset(self):
        result = {
            "benchmark": "drift-demo",
            "metadata": {"git_sha": "abc123"},
            "conditions": {
                "native": {
                    "historical_value": None,
                    "current_value": 9,
                    "source_state": "unknown",
                    "metrics": {"historical_accuracy": 0.0},
                },
                "hsum": {
                    "historical_value": 3,
                    "current_value": 9,
                    "source_state": "changed",
                    "metrics": {"historical_accuracy": 1.0},
                },
            },
        }
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "card.svg"
            benchmark.render_svg(result, output)
            rendered = output.read_text(encoding="utf-8")

        self.assertIn("previous value recovered", rendered)
        self.assertIn("WITH HSUM", rendered)

    def test_retrieval_svg_uses_result_task_count(self):
        result = {
            "benchmark": "retrieval",
            "metadata": {"git_sha": "abc123"},
            "retrievers": {
                name: {
                    "aggregate": {
                        "task_count": 25,
                        "hit@1": 0.5,
                        "ndcg@5": 0.6,
                        "latency_median_ms": 1.0,
                    }
                }
                for name in ("hsum", "ripgrep", "git-grep")
            },
        }
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "card.svg"
            benchmark.render_svg(result, output)
            rendered = output.read_text(encoding="utf-8")

        self.assertIn("25 frozen tasks", rendered)


if __name__ == "__main__":
    unittest.main()
