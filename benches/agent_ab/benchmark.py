#!/usr/bin/env python3
"""Reproducible retrieval and same-agent hSUM A/B benchmark.

The harness is dependency-free so published results can be rerun with only
Python, ripgrep, Git, hSUM, and (for agent trials) Codex CLI.
"""

from __future__ import annotations

import argparse
import hashlib
import datetime as dt
import html
import json
import math
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from collections import Counter
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence


SCHEMA_VERSION = "hsum.benchmark.results.v1"
TASK_SCHEMA_VERSION = "hsum.benchmark.tasks.v2"
GOLD_BENCHMARK_ID = "hsum-public-gold-25-v1"
GOLD_TASK_COUNT = 25
GOLD_CLASS_DISTRIBUTION = {
    "identifier": 5,
    "exact-phrase": 5,
    "concept": 5,
    "paraphrase": 5,
    "multi-evidence": 5,
}
GOLD_RETRIEVAL_PROTOCOL = {
    "runs_per_query": 5,
    "limit": 5,
    "context_lines": 20,
    "retrievers": ["ripgrep", "git-grep", "hsum"],
}
GOLD_INDEX_BUILDS = 3
GOLD_BASELINE_PROTOCOL = {**GOLD_RETRIEVAL_PROTOCOL, "index_builds": GOLD_INDEX_BUILDS}
JUDGMENT_GRADES = {1, 2}
DEFAULT_SCOPE = [
    "src",
    "tests",
    "docs",
    "outputs/IMPLEMENTATION_STATUS.md",
    "README.md",
    "CHANGELOG.md",
    "TODOS.md",
]


def precision_at_k(relevance: Sequence[int], k: int) -> float:
    if k <= 0:
        raise ValueError("k must be positive")
    return sum(int(value > 0) for value in relevance[:k]) / k


def recall_at_k(relevance: Sequence[int], k: int, relevant_total: int) -> float:
    if relevant_total <= 0:
        return 0.0
    hits = sum(int(value > 0) for value in relevance[:k])
    return min(hits, relevant_total) / relevant_total


def hit_at_k(relevance: Sequence[int], k: int) -> float:
    return float(any(relevance[:k]))


def reciprocal_rank(relevance: Sequence[int]) -> float:
    for index, value in enumerate(relevance, start=1):
        if value:
            return 1.0 / index
    return 0.0


def ndcg_at_k(
    relevance: Sequence[int],
    k: int,
    relevant_total: int | None = None,
    *,
    ideal_relevance: Sequence[int] | None = None,
) -> float:
    def dcg(values: Sequence[int]) -> float:
        return sum(
            ((2**value) - 1) / math.log2(index + 2)
            for index, value in enumerate(values)
        )

    if ideal_relevance is None:
        ideal_count = min(k, relevant_total or 0)
        ideal_values = [1] * ideal_count
    else:
        ideal_values = sorted(ideal_relevance, reverse=True)[:k]
    ideal = dcg(ideal_values)
    if ideal == 0.0:
        return 0.0
    return dcg(relevance[:k]) / ideal


def percentile(values: Sequence[float], quantile: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    if len(ordered) == 1:
        return float(ordered[0])
    position = (len(ordered) - 1) * quantile
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return float(ordered[lower])
    weight = position - lower
    return float(ordered[lower] * (1 - weight) + ordered[upper] * weight)


def normalize_path(value: str) -> str:
    value = value.strip()
    if value.startswith("repo://"):
        value = value[len("repo://") :]
    return value.removeprefix("./")


def portable_path(path: Path, root: Path) -> str:
    try:
        return path.resolve().relative_to(root.resolve()).as_posix()
    except ValueError:
        return str(path.resolve())


def document_relevance(
    ranked: Sequence[dict[str, Any]],
    judgments: Mapping[str, int] | set[str],
    limit: int,
) -> list[int]:
    if isinstance(judgments, set):
        grades = {normalize_path(path): 1 for path in judgments}
    else:
        grades = {normalize_path(path): grade for path, grade in judgments.items()}
    seen: set[str] = set()
    labels: list[int] = []
    for item in ranked:
        path = normalize_path(str(item.get("source_uri") or item.get("path") or ""))
        if not path or path in seen:
            continue
        seen.add(path)
        labels.append(grades.get(path, 0))
        if len(labels) == limit:
            break
    return labels


def ranked_document_paths(ranked: Sequence[dict[str, Any]], limit: int) -> list[str]:
    paths: list[str] = []
    seen: set[str] = set()
    for item in ranked:
        path = normalize_path(str(item.get("source_uri") or item.get("path") or ""))
        if not path or path in seen:
            continue
        seen.add(path)
        paths.append(path)
        if len(paths) == limit:
            break
    return paths


def task_manifest_digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def validate_manifest_checksum(path: Path, checksum_path: Path) -> str:
    expected = checksum_path.read_text(encoding="utf-8").strip().split()[0]
    actual = task_manifest_digest(path)
    if not re.fullmatch(r"[0-9a-f]{64}", expected):
        raise ValueError("task manifest checksum must be one lowercase SHA-256 digest")
    if actual != expected:
        raise ValueError(
            f"task manifest checksum mismatch: expected {expected}, observed {actual}"
        )
    return actual


def load_task_manifest(path: Path, *, repo: Path | None = None) -> dict[str, Any]:
    document = json.loads(path.read_text(encoding="utf-8"))
    if document.get("schema_version") != TASK_SCHEMA_VERSION:
        raise ValueError("unsupported task schema")
    if document.get("benchmark_id") != GOLD_BENCHMARK_ID:
        raise ValueError(f"benchmark_id must be {GOLD_BENCHMARK_ID}")
    if not isinstance(document.get("frozen_at"), str):
        raise ValueError("task manifest requires frozen_at")

    protocol = document.get("protocol")
    if not isinstance(protocol, dict):
        raise ValueError("task manifest requires a protocol object")
    if protocol.get("query_count") != GOLD_TASK_COUNT:
        raise ValueError(f"protocol query_count must be {GOLD_TASK_COUNT}")
    if protocol.get("class_distribution") != GOLD_CLASS_DISTRIBUTION:
        raise ValueError("protocol class_distribution does not match the frozen taxonomy")
    if protocol.get("cutoffs") != [1, 5] or protocol.get("primary_metric") != "ndcg@5":
        raise ValueError("protocol must freeze cutoffs [1, 5] and primary_metric ndcg@5")
    if protocol.get("baseline_parameters") != GOLD_BASELINE_PROTOCOL:
        raise ValueError("protocol baseline_parameters do not match the frozen runner contract")
    if protocol.get("corpus_scope") != DEFAULT_SCOPE:
        raise ValueError("protocol corpus_scope does not match the frozen corpus contract")

    tasks = document.get("tasks")
    if not isinstance(tasks, list) or len(tasks) != GOLD_TASK_COUNT:
        raise ValueError(f"task file must contain exactly {GOLD_TASK_COUNT} tasks")
    seen: set[str] = set()
    classes: Counter[str] = Counter()
    normalized_tasks: list[dict[str, Any]] = []
    for task in tasks:
        if not isinstance(task, dict):
            raise ValueError("every task must be an object")
        identifier = task.get("id")
        if not isinstance(identifier, str) or not identifier:
            raise ValueError("every task requires an id")
        if identifier in seen:
            raise ValueError(f"duplicate task id: {identifier}")
        seen.add(identifier)
        task_class = task.get("class")
        if task_class not in GOLD_CLASS_DISTRIBUTION:
            raise ValueError(f"task {identifier} has unsupported class: {task_class}")
        classes[str(task_class)] += 1
        if not task.get("query") or not task.get("question"):
            raise ValueError(f"task {identifier} requires query and question")

        patterns = task.get("required_patterns")
        if not isinstance(patterns, list) or not patterns:
            raise ValueError(f"task {identifier} requires answer patterns")
        for pattern in patterns:
            if not isinstance(pattern, str) or not pattern:
                raise ValueError(f"task {identifier} has an invalid answer pattern")
            try:
                re.compile(pattern)
            except re.error as error:
                raise ValueError(f"task {identifier} has invalid regex: {error}") from error

        judgments = task.get("judgments")
        if not isinstance(judgments, list) or not judgments:
            raise ValueError(f"task {identifier} requires graded judgments")
        relevance_grades: dict[str, int] = {}
        for judgment in judgments:
            if not isinstance(judgment, dict):
                raise ValueError(f"task {identifier} has a non-object judgment")
            judged_path = judgment.get("path")
            grade = judgment.get("grade")
            if not isinstance(judged_path, str) or not judged_path:
                raise ValueError(f"task {identifier} has a judgment without a path")
            normalized_path = normalize_path(judged_path)
            if normalized_path != judged_path or Path(judged_path).is_absolute() or ".." in Path(judged_path).parts:
                raise ValueError(f"task {identifier} judgment paths must be normalized and relative")
            if normalized_path in relevance_grades:
                raise ValueError(f"task {identifier} repeats judgment path: {normalized_path}")
            if grade not in JUDGMENT_GRADES:
                raise ValueError(f"task {identifier} judgment grades must be 1 or 2")
            if repo is not None and not (repo / normalized_path).is_file():
                raise ValueError(f"task {identifier} judgment path does not exist: {normalized_path}")
            relevance_grades[normalized_path] = int(grade)

        normalized = dict(task)
        normalized["relevance_grades"] = relevance_grades
        normalized["relevant_paths"] = list(relevance_grades)
        normalized_tasks.append(normalized)

    if dict(classes) != GOLD_CLASS_DISTRIBUTION:
        raise ValueError(
            f"task class distribution must be {GOLD_CLASS_DISTRIBUTION}, observed {dict(classes)}"
        )
    return {**document, "tasks": normalized_tasks}


def load_tasks(path: Path, *, repo: Path | None = None) -> list[dict[str, Any]]:
    return list(load_task_manifest(path, repo=repo)["tasks"])


def run_process(
    command: Sequence[str],
    *,
    cwd: Path,
    input_text: str | None = None,
    env: dict[str, str] | None = None,
    timeout: float = 120.0,
) -> dict[str, Any]:
    started = time.perf_counter_ns()
    completed = subprocess.run(
        list(command),
        cwd=cwd,
        input=input_text,
        text=True,
        capture_output=True,
        env=env,
        timeout=timeout,
        check=False,
    )
    elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
    return {
        "command": list(command),
        "returncode": completed.returncode,
        "stdout": completed.stdout,
        "stderr": completed.stderr,
        "elapsed_ms": elapsed_ms,
    }


def parse_match_paths(output: str) -> list[dict[str, str]]:
    ranked: list[dict[str, str]] = []
    for line in output.splitlines():
        match = re.match(r"^([^:]+):\d+:", line)
        if match:
            ranked.append({"source_uri": f"repo://{normalize_path(match.group(1))}"})
    return ranked


def retriever_command(
    name: str,
    *,
    query: str,
    scope: Sequence[str],
    hsum: Path,
    limit: int,
    context_lines: int,
) -> list[str]:
    if name == "hsum":
        return [
            str(hsum),
            "search",
            query,
            "--limit",
            str(limit),
            "--timeout-ms",
            "10000",
            "--json",
        ]
    if name == "ripgrep":
        # Stable ordering is part of the benchmark contract. It also avoids a
        # locally reproduced ripgrep 15.1.0 parallel-traversal failure in which
        # identical invocations intermittently exit 1 with no output.
        return [
            "rg",
            "--threads",
            "1",
            "--sort",
            "path",
            "-n",
            f"-C{context_lines}",
            "--fixed-strings",
            "--",
            query,
            *scope,
        ]
    if name == "git-grep":
        return [
            "git",
            "grep",
            "--untracked",
            "--exclude-standard",
            "-n",
            f"-C{context_lines}",
            "-F",
            "--",
            query,
            "--",
            *scope,
        ]
    raise ValueError(f"unknown retriever: {name}")


def execute_retriever(
    name: str,
    task: dict[str, Any],
    *,
    repo: Path,
    hsum: Path,
    limit: int,
    context_lines: int,
    runs: int,
    process_env: dict[str, str] | None = None,
) -> dict[str, Any]:
    query = str(task["query"])
    scope = [item for item in DEFAULT_SCOPE if (repo / item).exists()]
    samples: list[float] = []
    representative: dict[str, Any] | None = None

    for _ in range(runs):
        command = retriever_command(
            name,
            query=query,
            scope=scope,
            hsum=hsum,
            limit=limit,
            context_lines=context_lines,
        )
        observation = run_process(command, cwd=repo, env=process_env)
        if observation["returncode"] not in (0, 1):
            raise RuntimeError(
                f"{name} failed for {task['id']}: {observation['stderr'].strip()}"
            )
        samples.append(observation["elapsed_ms"])
        representative = observation

    assert representative is not None
    stdout = str(representative["stdout"])
    if name == "hsum":
        payload = json.loads(stdout)
        ranked = list(payload.get("results", []))
    else:
        ranked = parse_match_paths(stdout)

    graded_relevance = document_relevance(ranked, task["relevance_grades"], limit)
    relevance = [int(grade > 0) for grade in graded_relevance]
    relevant_total = len({normalize_path(path) for path in task["relevant_paths"]})
    relevant_hits = sum(relevance[:limit])
    payload_bytes = len(stdout.encode("utf-8"))
    return {
        "task_id": task["id"],
        "task_class": task.get("class", "unknown"),
        "query": query,
        "ranked_paths": ranked_document_paths(ranked, limit),
        "relevance": relevance,
        "graded_relevance": graded_relevance,
        "metrics": {
            f"precision@{limit}": precision_at_k(relevance, limit),
            f"recall@{limit}": recall_at_k(relevance, limit, relevant_total),
            f"hit@1": hit_at_k(relevance, 1),
            f"hit@{limit}": hit_at_k(relevance, limit),
            "mrr": reciprocal_rank(relevance),
            f"ndcg@{limit}": ndcg_at_k(
                graded_relevance,
                limit,
                ideal_relevance=list(task["relevance_grades"].values()),
            ),
            "relevant_document_hits": relevant_hits,
            "payload_bytes": payload_bytes,
            "bytes_per_relevant_hit": payload_bytes / relevant_hits if relevant_hits else None,
            "latency_median_ms": statistics.median(samples),
            "latency_p95_ms": percentile(samples, 0.95),
        },
        "latency_samples_ms": samples,
        "exit_code": representative["returncode"],
    }


def macro_average(results: Sequence[dict[str, Any]], metric: str) -> float:
    values = [result["metrics"].get(metric) for result in results]
    numeric = [float(value) for value in values if isinstance(value, (int, float))]
    return statistics.mean(numeric) if numeric else 0.0


def aggregate_retrieval(results: Sequence[dict[str, Any]], limit: int) -> dict[str, Any]:
    all_latencies = [
        float(sample)
        for result in results
        for sample in result.get("latency_samples_ms", [])
    ]
    total_bytes = sum(int(result["metrics"]["payload_bytes"]) for result in results)
    total_hits = sum(int(result["metrics"]["relevant_document_hits"]) for result in results)
    return {
        "task_count": len(results),
        f"precision@{limit}": macro_average(results, f"precision@{limit}"),
        f"recall@{limit}": macro_average(results, f"recall@{limit}"),
        "hit@1": macro_average(results, "hit@1"),
        f"hit@{limit}": macro_average(results, f"hit@{limit}"),
        "mrr": macro_average(results, "mrr"),
        f"ndcg@{limit}": macro_average(results, f"ndcg@{limit}"),
        "payload_bytes": total_bytes,
        "bytes_per_relevant_hit": total_bytes / total_hits if total_hits else None,
        "latency_median_ms": statistics.median(all_latencies) if all_latencies else 0.0,
        "latency_p95_ms": percentile(all_latencies, 0.95),
    }


def git_state(repo: Path) -> tuple[str, bool]:
    git_sha = run_process(["git", "rev-parse", "HEAD"], cwd=repo)["stdout"].strip()
    git_status = run_process(
        ["git", "status", "--porcelain", "--untracked-files=all"], cwd=repo
    )["stdout"]
    return git_sha, bool(git_status.strip())


def repository_metadata(
    repo: Path, hsum: Path, *, source_repo: Path | None = None
) -> dict[str, Any]:
    git_sha, git_dirty = git_state(repo)
    hsum_version = run_process([str(hsum), "--version"], cwd=repo)["stdout"].strip()
    metadata = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "repository": repo.name,
        "git_sha": git_sha,
        "git_dirty": git_dirty,
        "machine": platform.machine(),
        "platform": platform.platform(),
        "python": platform.python_version(),
        "hsum": hsum_version,
    }
    if source_repo is not None:
        source_sha, source_dirty = git_state(source_repo)
        metadata.update(
            {
                "corpus_kind": "isolated-scope-snapshot",
                "corpus_scope": DEFAULT_SCOPE,
                "source_git_sha": source_sha,
                "source_git_dirty": source_dirty,
            }
        )
    return metadata


def run_retrieval(
    args: argparse.Namespace,
    *,
    process_env: dict[str, str] | None = None,
    source_repo: Path | None = None,
    metadata_extra: Mapping[str, Any] | None = None,
) -> dict[str, Any]:
    repo = args.repo.resolve()
    hsum = args.hsum.resolve()
    manifest = load_task_manifest(args.tasks.resolve(), repo=repo)
    tasks = list(manifest["tasks"])
    retrievers = [name.strip() for name in args.retrievers.split(",") if name.strip()]
    observed_protocol = {
        "runs_per_query": args.runs,
        "limit": args.limit,
        "context_lines": args.context_lines,
        "retrievers": retrievers,
    }
    if observed_protocol != GOLD_RETRIEVAL_PROTOCOL:
        raise ValueError(
            "retrieval arguments do not match the frozen baseline protocol: "
            f"expected {GOLD_RETRIEVAL_PROTOCOL}, observed {observed_protocol}"
        )
    unavailable = [
        name
        for name in retrievers
        if (name == "ripgrep" and shutil.which("rg") is None)
        or (name == "git-grep" and shutil.which("git") is None)
        or (name == "hsum" and not hsum.is_file())
    ]
    if unavailable:
        raise RuntimeError(f"unavailable retrievers: {', '.join(unavailable)}")

    by_retriever: dict[str, Any] = {}
    for name in retrievers:
        print(f"benchmarking {name} ({len(tasks)} tasks x {args.runs} runs)", file=sys.stderr)
        task_results = [
            execute_retriever(
                name,
                task,
                repo=repo,
                hsum=hsum,
                limit=args.limit,
                context_lines=args.context_lines,
                runs=args.runs,
                process_env=process_env,
            )
            for task in tasks
        ]
        by_retriever[name] = {
            "aggregate": aggregate_retrieval(task_results, args.limit),
            "by_class": {
                task_class: aggregate_retrieval(
                    [result for result in task_results if result["task_class"] == task_class],
                    args.limit,
                )
                for task_class in GOLD_CLASS_DISTRIBUTION
            },
            "tasks": task_results,
        }

    metadata = repository_metadata(repo, hsum, source_repo=source_repo)
    if metadata_extra is not None:
        metadata.update(metadata_extra)
    result = {
        "schema_version": SCHEMA_VERSION,
        "benchmark": "retrieval",
        "metadata": metadata,
        "protocol": {
            "tasks": portable_path(args.tasks, source_repo or repo),
            "task_schema_version": manifest["schema_version"],
            "benchmark_id": manifest["benchmark_id"],
            "tasks_sha256": task_manifest_digest(args.tasks.resolve()),
            "task_count": len(tasks),
            "class_distribution": manifest["protocol"]["class_distribution"],
            "runs_per_query": args.runs,
            "limit": args.limit,
            "context_lines": args.context_lines,
            "relevance_unit": "deduplicated document path",
            "judgment_scale": manifest["protocol"]["judgment_scale"],
            "primary_metric": manifest["protocol"]["primary_metric"],
        },
        "retrievers": by_retriever,
    }
    write_json(args.output, result)
    print_retrieval_table(result, args.limit)
    return result


def materialize_corpus(source_repo: Path, destination: Path) -> None:
    destination.mkdir(parents=True)
    for relative in DEFAULT_SCOPE:
        source = source_repo / relative
        target = destination / relative
        if source.is_dir():
            shutil.copytree(source, target, symlinks=True)
        elif source.is_file():
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
        else:
            raise ValueError(f"corpus scope path is missing: {relative}")

    initialized = run_process(["git", "init", "-q"], cwd=destination)
    if initialized["returncode"] != 0:
        raise RuntimeError(f"corpus git init failed: {initialized['stderr'].strip()}")
    added = run_process(["git", "add", "--all"], cwd=destination)
    if added["returncode"] != 0:
        raise RuntimeError(f"corpus git add failed: {added['stderr'].strip()}")
    commit_env = os.environ.copy()
    commit_env.update(
        {
            "GIT_AUTHOR_DATE": "2026-08-01T00:00:00+00:00",
            "GIT_COMMITTER_DATE": "2026-08-01T00:00:00+00:00",
        }
    )
    committed = run_process(
        [
            "git",
            "-c",
            "user.name=hSUM Benchmark",
            "-c",
            "user.email=benchmark@hsum.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            GOLD_BENCHMARK_ID,
        ],
        cwd=destination,
        env=commit_env,
    )
    if committed["returncode"] != 0:
        raise RuntimeError(f"corpus git commit failed: {committed['stderr'].strip()}")


def run_baseline(args: argparse.Namespace) -> dict[str, Any]:
    source_repo = args.repo.resolve()
    hsum = args.hsum.resolve()
    with tempfile.TemporaryDirectory(prefix="hsum-gold-25-") as directory:
        temporary_root = Path(directory)
        corpus = temporary_root / source_repo.name
        materialize_corpus(source_repo, corpus)
        process_env = os.environ.copy()
        process_env["HSUM_HOME"] = str(temporary_root / "hsum-home")
        initialized = run_process(
            [
                str(hsum),
                "init",
                ".",
                "--index",
                GOLD_BENCHMARK_ID,
                "--project",
                "default",
                "--no-color",
                "--no-progress",
            ],
            cwd=corpus,
            env=process_env,
        )
        if initialized["returncode"] != 0:
            raise RuntimeError(
                f"isolated corpus initialization failed: {initialized['stderr'].strip()}"
            )
        status = run_process(
            [str(hsum), "status", "--json"],
            cwd=corpus,
            env=process_env,
        )
        if status["returncode"] != 0:
            raise RuntimeError(f"isolated corpus status failed: {status['stderr'].strip()}")
        status_payload = json.loads(str(status["stdout"]))

        retrieval_args = argparse.Namespace(**vars(args))
        retrieval_args.repo = corpus
        return run_retrieval(
            retrieval_args,
            process_env=process_env,
            source_repo=source_repo,
            metadata_extra={
                "indexed_documents": status_payload["active_documents"],
                "indexed_passages": status_payload["active_passages"],
                "index_epoch": status_payload["index_epoch"],
            },
        )


def summarize_build_metrics(
    metric_maps: Sequence[Mapping[str, Any]],
) -> tuple[dict[str, Any], dict[str, dict[str, float]]]:
    summary: dict[str, Any] = {}
    ranges: dict[str, dict[str, float]] = {}
    for metric in metric_maps[0]:
        values = [item.get(metric) for item in metric_maps]
        numeric = [float(value) for value in values if isinstance(value, (int, float))]
        if len(numeric) != len(values):
            continue
        if metric == "task_count":
            summary[metric] = int(numeric[0])
        elif metric == "payload_bytes":
            summary[metric] = round(statistics.mean(numeric))
        else:
            summary[metric] = statistics.mean(numeric)
        ranges[metric] = {"min": min(numeric), "max": max(numeric)}
    return summary, ranges


def run_stability(args: argparse.Namespace) -> dict[str, Any]:
    if args.index_builds != GOLD_INDEX_BUILDS:
        raise ValueError(
            f"stability requires exactly {GOLD_INDEX_BUILDS} independent index builds"
        )

    build_results: list[dict[str, Any]] = []
    with tempfile.TemporaryDirectory(prefix="hsum-gold-results-") as directory:
        for build in range(1, args.index_builds + 1):
            print(f"index build {build}/{args.index_builds}", file=sys.stderr)
            build_args = argparse.Namespace(**vars(args))
            build_args.output = Path(directory) / f"build-{build}.json"
            build_results.append(run_baseline(build_args))

    corpus_shas = {result["metadata"]["git_sha"] for result in build_results}
    if len(corpus_shas) != 1:
        raise RuntimeError("independent index builds did not use one identical corpus snapshot")

    retrievers: dict[str, Any] = {}
    for name in GOLD_RETRIEVAL_PROTOCOL["retrievers"]:
        reports = [result["retrievers"][name] for result in build_results]
        aggregate, ranges = summarize_build_metrics(
            [report["aggregate"] for report in reports]
        )
        by_class: dict[str, Any] = {}
        for task_class in GOLD_CLASS_DISTRIBUTION:
            class_summary, class_ranges = summarize_build_metrics(
                [report["by_class"][task_class] for report in reports]
            )
            by_class[task_class] = {"aggregate": class_summary, "ranges": class_ranges}
        retrievers[name] = {
            "aggregate": aggregate,
            "ranges": ranges,
            "by_class": by_class,
        }

    metadata = dict(build_results[0]["metadata"])
    metadata["generated_at"] = dt.datetime.now(dt.timezone.utc).isoformat()
    metadata["index_builds"] = args.index_builds
    protocol = dict(build_results[0]["protocol"])
    protocol.update(
        {
            "evaluation": "independent-index-build stability",
            "index_builds": args.index_builds,
            "aggregation": "arithmetic mean of per-build aggregate metrics; min/max ranges retained",
        }
    )
    result = {
        "schema_version": SCHEMA_VERSION,
        "benchmark": "retrieval",
        "metadata": metadata,
        "protocol": protocol,
        "retrievers": retrievers,
        "builds": build_results,
    }
    write_json(args.output, result)
    print_retrieval_table(result, args.limit)
    return result


def score_agent_response(
    task: dict[str, Any],
    response: dict[str, Any],
    citation_checks: Sequence[bool],
    *,
    require_citation: bool = False,
) -> dict[str, float]:
    answer = str(response.get("answer", ""))
    required = list(task.get("required_patterns", []))
    matched = [bool(re.search(pattern, answer, re.IGNORECASE)) for pattern in required]
    fact_accuracy = sum(matched) / len(required) if required else 0.0

    evidence = response.get("evidence")
    evidence_items = evidence if isinstance(evidence, list) else []
    relevant = {normalize_path(path) for path in task.get("relevant_paths", [])}
    evidence_labels = [
        int(normalize_path(str(item.get("path", ""))) in relevant)
        for item in evidence_items
        if isinstance(item, dict)
    ]
    evidence_precision = (
        sum(evidence_labels) / len(evidence_labels) if evidence_labels else 0.0
    )
    if citation_checks:
        citation_validity = sum(bool(value) for value in citation_checks) / len(citation_checks)
    else:
        citation_validity = 0.0 if require_citation else 1.0
    task_success = float(
        fact_accuracy == 1.0 and evidence_precision > 0.0 and citation_validity == 1.0
    )
    return {
        "fact_accuracy": fact_accuracy,
        "evidence_precision": evidence_precision,
        "citation_validity": citation_validity,
        "task_success": task_success,
    }


def agent_prompt(
    task: dict[str, Any],
    condition: str,
    hsum: Path,
    *,
    mode: str,
    evidence_bundle: str | None = None,
) -> str:
    common = f"""You are participating in a controlled coding-agent benchmark.
Work read-only. Answer only from evidence you inspect in the repository.

Question: {task['question']}
Suggested retrieval query: {task['query']}

Return JSON matching the supplied schema. In evidence.path use repository-relative paths.
Do not include facts you cannot support with inspected evidence.
"""
    if mode == "precollected":
        if evidence_bundle is None:
            raise ValueError("precollected agent mode requires an evidence bundle")
        policy = f"""
Condition: {'hSUM evidence' if condition == 'hsum' else 'native grep evidence'}.
The complete evidence bundle is below. Do not call tools, inspect files, or use outside knowledge.
If the bundle does not support an answer, say so rather than guessing.

<evidence_bundle>
{evidence_bundle}
</evidence_bundle>
"""
    elif condition == "native":
        policy = """
Condition: native tools only. Use ordinary file reads, Git, and ripgrep as needed.
Do not execute hsum and do not emit hsum citations.
"""
    elif condition == "hsum":
        policy = f"""
Condition: hSUM-assisted. Native tools remain available, but use hSUM for the evidence
supporting the final answer. Start with:
  {hsum} search {json.dumps(str(task['query']))} --limit 5 --json
Resolve useful results with:
  {hsum} get '<citation_uri>' --max-bytes 1024 --verify-source-hash --json
In evidence.citation_uri copy the citation that names the evidence used in the final answer.
"""
    else:
        raise ValueError(f"unknown agent condition: {condition}")
    return common + policy


def collect_evidence_bundle(
    *,
    task: dict[str, Any],
    condition: str,
    repo: Path,
    hsum: Path,
    limit: int,
    context_lines: int,
    byte_budget: int,
) -> dict[str, Any]:
    scope = [item for item in DEFAULT_SCOPE if (repo / item).exists()]
    retriever = "hsum" if condition == "hsum" else "ripgrep"
    command = retriever_command(
        retriever,
        query=str(task["query"]),
        scope=scope,
        hsum=hsum,
        limit=limit,
        context_lines=context_lines,
    )
    observation = run_process(command, cwd=repo)
    if observation["returncode"] not in (0, 1):
        raise RuntimeError(
            f"{retriever} context collection failed for {task['id']}: "
            f"{str(observation['stderr']).strip()}"
        )

    if condition == "native":
        bundle = str(observation["stdout"])
    else:
        payload = json.loads(str(observation["stdout"]))
        blocks: list[str] = []
        for index, item in enumerate(payload.get("results", []), start=1):
            span = item.get("span", {})
            block = (
                f"[{index}] {normalize_path(str(item.get('source_uri', '')))}:"
                f"{span.get('start_line', '?')}-{span.get('end_line', '?')}\n"
                f"citation: {item.get('citation_uri', '')}\n"
                f"source_state: {item.get('source_state', 'unknown')}\n"
                f"{item.get('content', '')}\n"
            )
            if len(("\n".join(blocks + [block])).encode("utf-8")) > byte_budget:
                break
            blocks.append(block)
        bundle = "\n".join(blocks)

    encoded = bundle.encode("utf-8")
    if len(encoded) > byte_budget:
        encoded = encoded[:byte_budget]
        bundle = encoded.decode("utf-8", errors="ignore")
    return {
        "retriever": retriever,
        "bundle": bundle,
        "context_bytes": len(bundle.encode("utf-8")),
        "retrieval_latency_ms": observation["elapsed_ms"],
        "retrieval_exit_code": observation["returncode"],
    }


def recursively_find_numbers(value: Any, key: str) -> Iterable[float]:
    if isinstance(value, dict):
        for candidate, child in value.items():
            if candidate == key and isinstance(child, (int, float)):
                yield float(child)
            yield from recursively_find_numbers(child, key)
    elif isinstance(value, list):
        for child in value:
            yield from recursively_find_numbers(child, key)


def parse_codex_jsonl(output: str) -> dict[str, Any]:
    events: list[dict[str, Any]] = []
    for line in output.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(event, dict):
            events.append(event)

    input_tokens = max(
        (number for event in events for number in recursively_find_numbers(event, "input_tokens")),
        default=0.0,
    )
    output_tokens = max(
        (number for event in events for number in recursively_find_numbers(event, "output_tokens")),
        default=0.0,
    )
    tool_calls = 0
    tool_output_bytes = 0
    for event in events:
        item = event.get("item")
        if not isinstance(item, dict):
            continue
        item_type = str(item.get("type", ""))
        if item_type in {"command_execution", "mcp_tool_call", "tool_call"}:
            tool_calls += 1
        for key in ("aggregated_output", "output", "content"):
            content = item.get(key)
            if isinstance(content, str):
                tool_output_bytes += len(content.encode("utf-8"))
                break
    return {
        "input_tokens": int(input_tokens),
        "output_tokens": int(output_tokens),
        "tool_calls": tool_calls,
        "tool_output_bytes": tool_output_bytes,
        "event_count": len(events),
    }


def validate_citations(
    response: dict[str, Any], *, hsum: Path, repo: Path
) -> list[bool]:
    checks: list[bool] = []
    for item in response.get("evidence", []):
        if not isinstance(item, dict):
            continue
        citation = item.get("citation_uri")
        if not isinstance(citation, str) or not citation.startswith("hsum://v1/"):
            continue
        observation = run_process(
            [str(hsum), "get", citation, "--max-bytes", "4096", "--verify-source-hash", "--json"],
            cwd=repo,
        )
        checks.append(observation["returncode"] == 0)
    return checks


def execute_agent_trial(
    *,
    task: dict[str, Any],
    condition: str,
    repo: Path,
    hsum: Path,
    codex: Path,
    schema: Path,
    model: str | None,
    timeout: float,
    agent_mode: str,
    context_budget: int,
    retrieval_limit: int,
    context_lines: int,
) -> dict[str, Any]:
    collected = None
    if agent_mode == "precollected":
        collected = collect_evidence_bundle(
            task=task,
            condition=condition,
            repo=repo,
            hsum=hsum,
            limit=retrieval_limit,
            context_lines=context_lines,
            byte_budget=context_budget,
        )

    with tempfile.TemporaryDirectory(prefix="hsum-agent-empty-") as empty_directory, tempfile.NamedTemporaryFile(
        prefix="hsum-agent-answer-", suffix=".json"
    ) as answer_file:
        agent_cwd = Path(empty_directory) if agent_mode == "precollected" else repo
        command = [
            str(codex),
            "exec",
            "--ignore-user-config",
            "--ignore-rules",
            "--ephemeral",
            "--sandbox",
            "read-only",
            "--json",
            "--color",
            "never",
            "--cd",
            str(agent_cwd),
            "--output-schema",
            str(schema),
            "--output-last-message",
            answer_file.name,
            "-",
        ]
        if agent_mode == "precollected":
            command.insert(-1, "--skip-git-repo-check")
        if model:
            command[2:2] = ["--model", model]
        observation = run_process(
            command,
            cwd=agent_cwd,
            input_text=agent_prompt(
                task,
                condition,
                hsum,
                mode=agent_mode,
                evidence_bundle=collected["bundle"] if collected else None,
            ),
            timeout=timeout,
        )
        answer_text = Path(answer_file.name).read_text(encoding="utf-8")

    try:
        response = json.loads(answer_text)
    except json.JSONDecodeError:
        response = {"answer": answer_text, "evidence": []}
    citation_checks = validate_citations(response, hsum=hsum, repo=repo) if condition == "hsum" else []
    metrics = score_agent_response(
        task,
        response,
        citation_checks,
        require_citation=condition == "hsum",
    )
    telemetry = parse_codex_jsonl(str(observation["stdout"]))
    metrics.update(
        {
            "model_latency_ms": observation["elapsed_ms"],
            "retrieval_latency_ms": collected["retrieval_latency_ms"] if collected else 0.0,
            "end_to_end_latency_ms": observation["elapsed_ms"]
            + (collected["retrieval_latency_ms"] if collected else 0.0),
            "context_bytes": collected["context_bytes"] if collected else telemetry["tool_output_bytes"],
            "input_tokens": telemetry["input_tokens"],
            "output_tokens": telemetry["output_tokens"],
            "tool_calls": telemetry["tool_calls"],
            "tool_output_bytes": telemetry["tool_output_bytes"],
        }
    )
    return {
        "task_id": task["id"],
        "condition": condition,
        "agent_mode": agent_mode,
        "response": response,
        "metrics": metrics,
        "process": {
            "exit_code": observation["returncode"],
            "stderr_tail": str(observation["stderr"])[-2000:],
            "event_count": telemetry["event_count"],
        },
    }


def aggregate_agents(trials: Sequence[dict[str, Any]]) -> dict[str, Any]:
    metric_names = [
        "fact_accuracy",
        "evidence_precision",
        "citation_validity",
        "task_success",
        "model_latency_ms",
        "retrieval_latency_ms",
        "end_to_end_latency_ms",
        "context_bytes",
        "input_tokens",
        "output_tokens",
        "tool_calls",
        "tool_output_bytes",
    ]
    aggregate: dict[str, Any] = {"task_count": len(trials)}
    for metric in metric_names:
        values = [float(trial["metrics"][metric]) for trial in trials]
        aggregate[metric] = statistics.mean(values) if values else 0.0
    aggregate["end_to_end_latency_p95_ms"] = percentile(
        [float(trial["metrics"]["end_to_end_latency_ms"]) for trial in trials], 0.95
    )
    return aggregate


def run_agent(args: argparse.Namespace) -> dict[str, Any]:
    repo = args.repo.resolve()
    hsum = args.hsum.resolve()
    codex = args.codex.resolve()
    manifest = load_task_manifest(args.tasks.resolve(), repo=repo)
    tasks = list(manifest["tasks"])
    if args.task_ids:
        selected = set(args.task_ids.split(","))
        tasks = [task for task in tasks if task["id"] in selected]
    tasks = tasks[: args.max_tasks]
    conditions = ["native", "hsum"] if args.condition == "both" else [args.condition]
    conditions_output: dict[str, Any] = {}
    for condition in conditions:
        print(f"running {condition} agent condition ({len(tasks)} tasks)", file=sys.stderr)
        trials = [
            execute_agent_trial(
                task=task,
                condition=condition,
                repo=repo,
                hsum=hsum,
                codex=codex,
                schema=args.schema.resolve(),
                model=args.model,
                timeout=args.timeout,
                agent_mode=args.agent_mode,
                context_budget=args.context_budget,
                retrieval_limit=args.retrieval_limit,
                context_lines=args.context_lines,
            )
            for task in tasks
        ]
        conditions_output[condition] = {
            "aggregate": aggregate_agents(trials),
            "trials": trials,
        }

    result = {
        "schema_version": SCHEMA_VERSION,
        "benchmark": "agent-ab",
        "metadata": repository_metadata(repo, hsum),
        "protocol": {
            "benchmark_id": manifest["benchmark_id"],
            "tasks_sha256": task_manifest_digest(args.tasks.resolve()),
            "model": args.model or "codex-default",
            "same_model": True,
            "same_repository": True,
            "same_task_prompts": True,
            "agent_mode": args.agent_mode,
            "context_budget_bytes": args.context_budget,
            "conditions": conditions,
            "hsum_condition": (
                "precollected hSUM evidence bundle"
                if args.agent_mode == "precollected"
                else "native tools plus required hSUM evidence"
            ),
            "native_condition": (
                "precollected ripgrep evidence bundle"
                if args.agent_mode == "precollected"
                else "native read/Git/ripgrep tools; hSUM prohibited"
            ),
        },
        "conditions": conditions_output,
    }
    write_json(args.output, result)
    print_agent_table(result)
    return result


def write_json(path: Path, value: dict[str, Any]) -> None:
    path = path.resolve()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def print_retrieval_table(result: dict[str, Any], limit: int) -> None:
    print()
    print("RETRIEVAL BENCHMARK")
    print(
        f"{'Retriever':<12} {f'nDCG@{limit}':>8} {'Hit@1':>8} {f'Recall@{limit}':>10} {'MRR':>8} "
        f"{'Median ms':>11} {'Payload':>12}"
    )
    for name, report in result["retrievers"].items():
        metric = report["aggregate"]
        print(
            f"{name:<12} {metric[f'ndcg@{limit}'] * 100:>7.1f}% "
            f"{metric['hit@1'] * 100:>7.1f}% "
            f"{metric[f'recall@{limit}'] * 100:>9.1f}% {metric['mrr']:>8.3f} "
            f"{metric['latency_median_ms']:>11.1f} {metric['payload_bytes']:>12,d}"
        )


def print_agent_table(result: dict[str, Any]) -> None:
    print()
    print("SAME-AGENT A/B BENCHMARK")
    print(f"{'Condition':<12} {'Success':>9} {'Facts':>9} {'Evidence':>10} {'Seconds':>10} {'Tokens':>10}")
    for name, report in result["conditions"].items():
        metric = report["aggregate"]
        tokens = metric["input_tokens"] + metric["output_tokens"]
        print(
            f"{name:<12} {metric['task_success'] * 100:>8.1f}% "
            f"{metric['fact_accuracy'] * 100:>8.1f}% {metric['evidence_precision'] * 100:>9.1f}% "
            f"{metric['end_to_end_latency_ms'] / 1000:>10.1f} {tokens:>10.0f}"
        )


def metric_percent(value: Any) -> str:
    return f"{float(value) * 100:.1f}%"


def metric_range(report: Mapping[str, Any], metric: str) -> str:
    value = report["aggregate"][metric]
    observed = report.get("ranges", {}).get(metric, {"min": value, "max": value})
    return f"{metric_percent(observed['min'])}–{metric_percent(observed['max'])}"


def render_dashboard(result: dict[str, Any], output: Path) -> None:
    if result.get("benchmark") == "retrieval":
        sections = []
        for name, report in result["retrievers"].items():
            metric = report["aggregate"]
            sections.append(
                {
                    "name": name,
                    "primary": metric_percent(metric["ndcg@5"]),
                    "primary_label": "nDCG@5",
                    "secondary": [
                        ("Hit@1", metric_percent(metric["hit@1"])),
                        ("Build range", metric_range(report, "ndcg@5")),
                        ("Median", f"{metric['latency_median_ms']:.0f} ms"),
                    ],
                }
            )
        title = "Can the tool find the right evidence first?"
        builds = int(result.get("protocol", {}).get("index_builds", 1))
        subtitle = f"{next(iter(result['retrievers'].values()))['aggregate']['task_count']} frozen tasks · {builds} fresh index build{'s' if builds != 1 else ''} · mean and range"
    elif result.get("benchmark") == "agent-ab":
        sections = []
        for name, report in result["conditions"].items():
            metric = report["aggregate"]
            sections.append(
                {
                    "name": "With hSUM" if name == "hsum" else "Without hSUM",
                    "primary": metric_percent(metric["task_success"]),
                    "primary_label": "grounded task success",
                    "secondary": [
                        ("Fact accuracy", metric_percent(metric["fact_accuracy"])),
                        ("Evidence precision", metric_percent(metric["evidence_precision"])),
                        ("Mean time", f"{metric['end_to_end_latency_ms'] / 1000:.1f} s"),
                    ],
                }
            )
        title = "Same agent. Same questions. One evidence layer."
        subtitle = f"{next(iter(result['conditions'].values()))['aggregate']['task_count']} frozen tasks · model held constant"
    elif result.get("benchmark") == "drift-demo":
        sections = []
        for name, report in result["conditions"].items():
            metric = report["metrics"]
            sections.append(
                {
                    "name": "With hSUM" if name == "hsum" else "Without hSUM",
                    "primary": (
                        str(report["historical_value"])
                        if report["historical_value"] is not None
                        else "?"
                    ),
                    "primary_label": "previous uncommitted value recovered",
                    "secondary": [
                        ("Current value", str(report["current_value"])),
                        ("Drift state", str(report["source_state"])),
                        ("Recovery", metric_percent(metric["historical_accuracy"])),
                    ],
                }
            )
        title = "The file changed. What did the agent see before?"
        subtitle = "Uncommitted source · no Git history · exact stored bytes"
    else:
        raise ValueError("unsupported result benchmark")

    cards = []
    for section in sections:
        secondary = "".join(
            f"<div class='mini'><span>{html.escape(label)}</span><strong>{html.escape(value)}</strong></div>"
            for label, value in section["secondary"]
        )
        cards.append(
            f"""<article class="card">
  <div class="condition">{html.escape(section['name'])}</div>
  <div class="primary">{html.escape(section['primary'])}</div>
  <div class="label">{html.escape(section['primary_label'])}</div>
  <div class="secondary">{secondary}</div>
</article>"""
        )

    metadata = result.get("metadata", {})
    document = f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{html.escape(title)}</title>
<style>
:root{{--ink:#0b1730;--blue:#1769e0;--pale:#edf5ff;--gold:#f2b84b;--paper:#fbfaf5}}
*{{box-sizing:border-box}} body{{margin:0;background:var(--paper);color:var(--ink);font-family:Inter,ui-sans-serif,system-ui,-apple-system,sans-serif}}
main{{min-height:100vh;padding:72px clamp(32px,7vw,120px);background:radial-gradient(circle at 85% 0,#dcecff 0,transparent 35%)}}
.eyebrow{{font-weight:800;letter-spacing:.18em;text-transform:uppercase;color:var(--blue)}}
h1{{font-family:Georgia,serif;font-size:clamp(48px,7vw,96px);line-height:.96;max-width:1100px;margin:22px 0 18px}}
.subtitle{{font-size:22px;color:#41506b;margin-bottom:54px}}
.grid{{display:grid;grid-template-columns:repeat(auto-fit,minmax(330px,1fr));gap:24px;max-width:1250px}}
.card{{background:white;border:1px solid #d9e2ef;border-radius:28px;padding:34px;box-shadow:0 22px 60px #1e4f8a16}}
.condition{{font-size:20px;font-weight:850;text-transform:uppercase;letter-spacing:.08em}}
.primary{{font-size:clamp(70px,9vw,128px);font-weight:900;letter-spacing:-.07em;color:var(--blue);line-height:.95;margin-top:30px}}
.label{{font-size:20px;font-weight:700;color:#52627d;margin:8px 0 38px}}
.secondary{{display:grid;grid-template-columns:repeat(3,1fr);gap:10px;border-top:1px solid #e4e9f1;padding-top:22px}}
.mini span{{display:block;color:#6b7890;font-size:13px}} .mini strong{{display:block;margin-top:7px;font-size:20px}}
footer{{margin-top:42px;color:#62708a;font-size:14px}} code{{font-family:ui-monospace,monospace}}
</style></head><body><main>
<div class="eyebrow">hSUM evidence benchmark</div><h1>{html.escape(title)}</h1>
<div class="subtitle">{html.escape(subtitle)}</div><section class="grid">{''.join(cards)}</section>
<footer>Reproducible result · <code>{html.escape(str(metadata.get('git_sha',''))[:12])}</code> · {html.escape(str(metadata.get('generated_at','')))}</footer>
</main></body></html>"""
    output = output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(document, encoding="utf-8")


def run_render(args: argparse.Namespace) -> None:
    result = json.loads(args.input.read_text(encoding="utf-8"))
    render_dashboard(result, args.output)
    svg_output = args.output.with_suffix(".svg")
    render_svg(result, svg_output)
    print(args.output.resolve())
    print(svg_output.resolve())


def render_svg(result: dict[str, Any], output: Path) -> None:
    if result.get("benchmark") == "retrieval":
        title = "Relevant evidence, ranked first"
        reports = result["retrievers"]
        task_count = next(iter(reports.values()))["aggregate"]["task_count"]
        builds = int(result.get("protocol", {}).get("index_builds", 1))
        subtitle = f"{task_count} frozen tasks · {builds} fresh index build{'s' if builds != 1 else ''} · mean and range"
        cards = [
            (
                name,
                metric_percent(reports[name]["aggregate"]["ndcg@5"]),
                "nDCG@5",
                f"{metric_range(reports[name], 'ndcg@5')} range · {reports[name]['aggregate']['latency_median_ms']:.0f} ms",
            )
            for name in ("hsum", "ripgrep", "git-grep")
            if name in reports
        ]
    elif result.get("benchmark") == "agent-ab":
        title = "Same model. Better evidence."
        subtitle = "Strict success requires correct facts, relevant evidence, and valid citations"
        cards = [
            (
                "with hSUM" if name == "hsum" else "without hSUM",
                metric_percent(report["aggregate"]["task_success"]),
                "grounded task success",
                f"{metric_percent(report['aggregate']['fact_accuracy'])} fact accuracy",
            )
            for name, report in result["conditions"].items()
        ]
    elif result.get("benchmark") == "drift-demo":
        title = "The file changed. What was there before?"
        subtitle = "Uncommitted source · no Git history · exact stored bytes"
        cards = [
            (
                "with hSUM" if name == "hsum" else "without hSUM",
                str(report["historical_value"]) if report["historical_value"] is not None else "?",
                "previous value recovered",
                f"current {report['current_value']} · {report['source_state']}",
            )
            for name, report in result["conditions"].items()
        ]
    else:
        raise ValueError("unsupported result benchmark")

    width = 1600
    height = 900
    gap = 28
    margin = 100
    card_width = (width - (2 * margin) - (gap * (len(cards) - 1))) / len(cards)
    card_y = 310
    card_height = 410
    title_size = 60 if result.get("benchmark") == "drift-demo" else 72
    primary_size = 82 if len(cards) == 3 else 108
    card_elements: list[str] = []
    for index, (name, primary, label, detail) in enumerate(cards):
        x = margin + index * (card_width + gap)
        card_elements.append(
            f"""<g>
<rect x="{x:.0f}" y="{card_y}" width="{card_width:.0f}" height="{card_height}" rx="28" fill="#ffffff" stroke="#d7e2f0" stroke-width="2"/>
<text x="{x + 34:.0f}" y="{card_y + 58}" class="condition">{html.escape(name.upper())}</text>
<text x="{x + 34:.0f}" y="{card_y + 200}" class="primary">{html.escape(primary)}</text>
<text x="{x + 34:.0f}" y="{card_y + 248}" class="label">{html.escape(label)}</text>
<line x1="{x + 34:.0f}" x2="{x + card_width - 34:.0f}" y1="{card_y + 300}" y2="{card_y + 300}" stroke="#e3eaf3"/>
<text x="{x + 34:.0f}" y="{card_y + 354}" class="detail">{html.escape(detail)}</text>
</g>"""
        )
    svg = f"""<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}">
<rect width="1600" height="900" fill="#fbfaf5"/>
<circle cx="1450" cy="-60" r="420" fill="#e2efff"/>
<style>
.eyebrow{{font-family:system-ui,sans-serif;font-size:24px;font-weight:800;letter-spacing:5px;fill:#1769e0}}
.title{{font-family:Georgia,serif;font-size:{title_size}px;font-weight:700;fill:#0b1730}}
.subtitle{{font-family:system-ui,sans-serif;font-size:26px;font-weight:400;fill:#52627d}}
.condition{{font-family:system-ui,sans-serif;font-size:20px;font-weight:800;letter-spacing:2px;fill:#0b1730}}
.primary{{font-family:system-ui,sans-serif;font-size:{primary_size}px;font-weight:900;letter-spacing:-4px;fill:#1769e0}}
.label{{font-family:system-ui,sans-serif;font-size:22px;font-weight:700;fill:#52627d}}
.detail{{font-family:system-ui,sans-serif;font-size:23px;font-weight:700;fill:#0b1730}}
.foot{{font-family:ui-monospace,monospace;font-size:17px;font-weight:500;fill:#6b7890}}
</style>
<text x="100" y="92" class="eyebrow">hSUM EVIDENCE BENCHMARK</text>
<text x="100" y="190" class="title">{html.escape(title)}</text>
<text x="100" y="245" class="subtitle">{html.escape(subtitle)}</text>
{''.join(card_elements)}
<text x="100" y="825" class="foot">Reproduce: python3 benches/agent_ab/benchmark.py · {html.escape(str(result.get('metadata', {}).get('git_sha', ''))[:12])}</text>
</svg>"""
    output = output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(svg, encoding="utf-8")


def extract_integer_constant(text: str, name: str) -> int | None:
    match = re.search(rf"\b{re.escape(name)}\b[^=]*=\s*(\d+)", text)
    return int(match.group(1)) if match else None


def run_drift(args: argparse.Namespace) -> dict[str, Any]:
    repo_under_test = args.repo.resolve()
    hsum = args.hsum.resolve()
    with tempfile.TemporaryDirectory(prefix="hsum-drift-benchmark-") as directory:
        root = Path(directory)
        fixture = root / "fixture"
        data_dir = root / "data"
        cache_dir = root / "cache"
        config_dir = root / "config"
        (fixture / "src").mkdir(parents=True)
        data_dir.mkdir()
        cache_dir.mkdir()
        config_dir.mkdir()
        source = fixture / "src/policy.rs"
        before = "pub const RETRY_LIMIT: u32 = 3;\n"
        after = "pub const RETRY_LIMIT: u32 = 9;\n"
        source.write_text(before, encoding="utf-8")
        git_init = run_process(["git", "init", "-q"], cwd=fixture)
        if git_init["returncode"] != 0:
            raise RuntimeError(str(git_init["stderr"]).strip())

        isolated_env = os.environ.copy()
        isolated_env["XDG_CONFIG_HOME"] = str(config_dir)
        init = run_process(
            [
                str(hsum),
                "init",
                ".",
                "--index",
                "drift-benchmark",
                "--project",
                "default",
                "--data-dir",
                str(data_dir),
                "--cache-dir",
                str(cache_dir),
                "--no-color",
            ],
            cwd=fixture,
            env=isolated_env,
        )
        if init["returncode"] != 0:
            raise RuntimeError(f"drift fixture init failed: {str(init['stderr']).strip()}")

        search = run_process(
            [
                str(hsum),
                "search",
                "RETRY_LIMIT",
                "--limit",
                "1",
                "--json",
                "--data-dir",
                str(data_dir),
                "--cache-dir",
                str(cache_dir),
            ],
            cwd=fixture,
            env=isolated_env,
        )
        search_payload = json.loads(str(search["stdout"]))
        citation = search_payload["results"][0]["citation_uri"]
        source.write_text(after, encoding="utf-8")

        native_started = time.perf_counter_ns()
        native_current_text = source.read_text(encoding="utf-8")
        native_latency = (time.perf_counter_ns() - native_started) / 1_000_000
        current_value = extract_integer_constant(native_current_text, "RETRY_LIMIT")

        get = run_process(
            [
                str(hsum),
                "get",
                citation,
                "--max-bytes",
                "1024",
                "--verify-source-hash",
                "--json",
                "--data-dir",
                str(data_dir),
                "--cache-dir",
                str(cache_dir),
            ],
            cwd=fixture,
            env=isolated_env,
        )
        if get["returncode"] != 0:
            raise RuntimeError(f"drift citation get failed: {str(get['stderr']).strip()}")
        get_payload = json.loads(str(get["stdout"]))
        historical_value = extract_integer_constant(str(get_payload["content"]), "RETRY_LIMIT")
        verification = get_payload.get("source_hash_verification")

        result = {
            "schema_version": SCHEMA_VERSION,
            "benchmark": "drift-demo",
            "metadata": repository_metadata(repo_under_test, hsum),
            "protocol": {
                "fixture": "uncommitted Rust source with no Git object containing the old bytes",
                "before_value": 3,
                "after_value": 9,
                "mutation": "overwrite after hSUM ingest without a second ingest",
            },
            "conditions": {
                "native": {
                    "historical_value": None,
                    "current_value": current_value,
                    "source_state": "unknown",
                    "metrics": {
                        "historical_accuracy": 0.0,
                        "current_accuracy": float(current_value == 9),
                        "drift_detection": 0.0,
                        "latency_ms": native_latency,
                    },
                },
                "hsum": {
                    "historical_value": historical_value,
                    "current_value": current_value,
                    "source_state": verification,
                    "citation_uri": citation,
                    "metrics": {
                        "historical_accuracy": float(historical_value == 3),
                        "current_accuracy": float(current_value == 9),
                        "drift_detection": float(verification == "changed"),
                        "latency_ms": get["elapsed_ms"],
                    },
                },
            },
            "video_transcript": [
                "BEFORE: RETRY_LIMIT = 3 (uncommitted)",
                "EDIT: RETRY_LIMIT = 9",
                "WITHOUT hSUM: current = 9; previous value = unknown",
                f"WITH hSUM: previous = {historical_value}; current = {current_value}; source = {verification}",
            ],
        }

    write_json(args.output, result)
    transcript_path = args.output.with_suffix(".txt")
    transcript_path.write_text("\n".join(result["video_transcript"]) + "\n", encoding="utf-8")
    print("\nSOURCE DRIFT DEMO")
    for line in result["video_transcript"]:
        print(line)
    return result


def run_validate(args: argparse.Namespace) -> dict[str, Any]:
    tasks_path = args.tasks.resolve()
    checksum_path = args.checksum.resolve()
    manifest = load_task_manifest(tasks_path, repo=args.repo.resolve())
    digest = validate_manifest_checksum(tasks_path, checksum_path)
    result = {
        "benchmark_id": manifest["benchmark_id"],
        "schema_version": manifest["schema_version"],
        "task_count": len(manifest["tasks"]),
        "class_distribution": dict(Counter(task["class"] for task in manifest["tasks"])),
        "sha256": digest,
        "status": "valid",
    }
    print(json.dumps(result, indent=2, sort_keys=True))
    return result


def parser() -> argparse.ArgumentParser:
    root = Path(__file__).resolve().parents[2]
    bench_dir = Path(__file__).resolve().parent
    argument_parser = argparse.ArgumentParser(description=__doc__)
    subparsers = argument_parser.add_subparsers(dest="command", required=True)

    validate = subparsers.add_parser(
        "validate", help="validate the frozen task manifest, labels, paths, and checksum"
    )
    validate.add_argument("--repo", type=Path, default=root)
    validate.add_argument("--tasks", type=Path, default=bench_dir / "tasks.json")
    validate.add_argument("--checksum", type=Path, default=bench_dir / "tasks.sha256")
    validate.set_defaults(handler=run_validate)

    stability = subparsers.add_parser(
        "stability",
        help="run and aggregate the frozen baseline across independent index builds",
    )
    stability.add_argument("--repo", type=Path, default=root)
    stability.add_argument("--hsum", type=Path, default=root / "target/release/hsum")
    stability.add_argument("--tasks", type=Path, default=bench_dir / "tasks.json")
    stability.add_argument("--retrievers", default="ripgrep,git-grep,hsum")
    stability.add_argument("--runs", type=int, default=5)
    stability.add_argument("--limit", type=int, default=5)
    stability.add_argument("--context-lines", type=int, default=20)
    stability.add_argument("--index-builds", type=int, default=GOLD_INDEX_BUILDS)
    stability.add_argument("--output", type=Path, default=bench_dir / "results/retrieval.json")
    stability.set_defaults(handler=run_stability)

    baseline = subparsers.add_parser(
        "baseline",
        help="materialize one isolated corpus and run the frozen three-retriever baseline",
    )
    baseline.add_argument("--repo", type=Path, default=root)
    baseline.add_argument("--hsum", type=Path, default=root / "target/release/hsum")
    baseline.add_argument("--tasks", type=Path, default=bench_dir / "tasks.json")
    baseline.add_argument("--retrievers", default="ripgrep,git-grep,hsum")
    baseline.add_argument("--runs", type=int, default=5)
    baseline.add_argument("--limit", type=int, default=5)
    baseline.add_argument("--context-lines", type=int, default=20)
    baseline.add_argument(
        "--output", type=Path, default=bench_dir / "results/retrieval-single.json"
    )
    baseline.set_defaults(handler=run_baseline)

    retrieval = subparsers.add_parser("retrieval", help="compare hSUM, ripgrep, and git grep")
    retrieval.add_argument("--repo", type=Path, default=root)
    retrieval.add_argument("--hsum", type=Path, default=root / "target/release/hsum")
    retrieval.add_argument("--tasks", type=Path, default=bench_dir / "tasks.json")
    retrieval.add_argument("--retrievers", default="ripgrep,git-grep,hsum")
    retrieval.add_argument("--runs", type=int, default=5)
    retrieval.add_argument("--limit", type=int, default=5)
    retrieval.add_argument("--context-lines", type=int, default=20)
    retrieval.add_argument(
        "--output", type=Path, default=bench_dir / "results/retrieval-single.json"
    )
    retrieval.set_defaults(handler=run_retrieval)

    agent = subparsers.add_parser("agent", help="run the same Codex tasks without and with hSUM")
    agent.add_argument("--repo", type=Path, default=root)
    agent.add_argument("--hsum", type=Path, default=root / "target/release/hsum")
    agent.add_argument("--tasks", type=Path, default=bench_dir / "tasks.json")
    agent.add_argument("--schema", type=Path, default=bench_dir / "agent_output.schema.json")
    agent.add_argument("--codex", type=Path, default=Path(shutil.which("codex") or "codex"))
    agent.add_argument("--condition", choices=["native", "hsum", "both"], default="both")
    agent.add_argument("--model")
    agent.add_argument(
        "--agent-mode",
        choices=["precollected", "tool"],
        default="precollected",
        help="precollected isolates context quality; tool also measures agent tool use",
    )
    agent.add_argument("--task-ids", help="comma-separated task ids")
    agent.add_argument("--max-tasks", type=int, default=3)
    agent.add_argument("--context-budget", type=int, default=12288)
    agent.add_argument("--retrieval-limit", type=int, default=5)
    agent.add_argument("--context-lines", type=int, default=20)
    agent.add_argument("--timeout", type=float, default=300.0)
    agent.add_argument("--output", type=Path, default=bench_dir / "results/agent-ab.json")
    agent.set_defaults(handler=run_agent)

    render = subparsers.add_parser("render", help="render a self-contained shareable dashboard")
    render.add_argument("input", type=Path)
    render.add_argument("--output", type=Path, default=bench_dir / "results/dashboard.html")
    render.set_defaults(handler=run_render)

    drift = subparsers.add_parser("drift", help="demonstrate recovery of changed uncommitted bytes")
    drift.add_argument("--repo", type=Path, default=root)
    drift.add_argument("--hsum", type=Path, default=root / "target/release/hsum")
    drift.add_argument("--output", type=Path, default=bench_dir / "results/drift-demo.json")
    drift.set_defaults(handler=run_drift)
    return argument_parser


def main() -> int:
    args = parser().parse_args()
    try:
        args.handler(args)
    except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
