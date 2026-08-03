#!/usr/bin/env python3
"""Frozen stable-v0.1 retrieval evaluation for hSUM.

The harness intentionally uses only the Python standard library. Product-mode
promotion is based on paired hSUM lexical/semantic/hybrid results. ripgrep and
QMD are retained as report-only external comparisons because their result and
citation contracts differ from hSUM's immutable evidence packets.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import shutil
import statistics
import subprocess
import sys
import time
import tomllib
from dataclasses import dataclass
from pathlib import Path
from random import Random
from typing import Any, Iterable, Mapping, Sequence


MANIFEST_SCHEMA = "hsum.eval.manifest.v1"
CORPORA_SCHEMA = "hsum.eval.corpora.v1"
QUERIES_SCHEMA = "hsum.eval.queries.v1"
RESULT_SCHEMA = "hsum.eval.results.v1"
EVALUATION_ID = "hsum-stable-v0.1-heldout-v1"
HSUM_RETRIEVERS = ("hsum-lexical", "hsum-semantic", "hsum-hybrid")
EXTERNAL_RETRIEVERS = ("ripgrep", "qmd")
REQUIRED_CASE_TAGS = {
    "quoted-error",
    "identifier",
    "paraphrase",
    "duplicate-content",
    "renamed-file",
    "deleted-document",
    "stale-current-conflict",
    "project-scope-change",
    "global-top50-filter-trap",
}


class EvaluationError(RuntimeError):
    """A frozen evaluation contract was not satisfied."""


@dataclass(frozen=True)
class AcceptedSpan:
    path: str
    start: int
    end: int
    grade: int


@dataclass(frozen=True)
class LoadedEvaluation:
    root: Path
    manifest: dict[str, Any]
    manifest_sha256: str
    corpora: dict[str, dict[str, Any]]
    tasks: list[dict[str, Any]]


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def canonical_json(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def framed_digest(values: Iterable[str]) -> str:
    hasher = hashlib.sha256()
    for value in values:
        encoded = value.encode("utf-8")
        hasher.update(len(encoded).to_bytes(8, "big"))
        hasher.update(encoded)
    return hasher.hexdigest()


def safe_relative(value: str) -> str:
    path = Path(value)
    if not value or path.is_absolute() or ".." in path.parts or path.as_posix() != value:
        raise EvaluationError(f"path is not normalized and relative: {value!r}")
    return value


def run_process(
    command: Sequence[str],
    *,
    cwd: Path,
    env: Mapping[str, str] | None = None,
    timeout: float = 180.0,
) -> dict[str, Any]:
    started = time.perf_counter_ns()
    try:
        completed = subprocess.run(
            list(command),
            cwd=cwd,
            env=dict(env) if env is not None else None,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
    except subprocess.TimeoutExpired as error:
        raise EvaluationError(f"command timed out: {command!r}") from error
    return {
        "command": list(command),
        "exit_code": completed.returncode,
        "stdout": completed.stdout,
        "stderr": completed.stderr,
        "elapsed_ms": (time.perf_counter_ns() - started) / 1_000_000,
    }


def git_blob(repo: Path, commit: str, path: str) -> bytes:
    completed = subprocess.run(
        ["git", "show", f"{commit}:{path}"],
        cwd=repo,
        capture_output=True,
        check=False,
    )
    if completed.returncode != 0:
        detail = completed.stderr.decode("utf-8", errors="replace").strip()
        raise EvaluationError(f"unable to read frozen blob {commit}:{path}: {detail}")
    return completed.stdout


def nth_occurrence(body: bytes, needle: bytes, occurrence: int) -> tuple[int, int]:
    if not needle:
        raise EvaluationError("accepted span needle must not be empty")
    if occurrence <= 0:
        raise EvaluationError("accepted span occurrence must be positive")
    offset = -1
    cursor = 0
    for _ in range(occurrence):
        offset = body.find(needle, cursor)
        if offset < 0:
            raise EvaluationError(
                f"accepted span occurrence {occurrence} was not found for {needle!r}"
            )
        cursor = offset + len(needle)
    return offset, offset + len(needle)


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise EvaluationError(f"unable to load {path}: {error}") from error


def require_exact_keys(value: Mapping[str, Any], required: set[str], where: str) -> None:
    observed = set(value)
    if observed != required:
        raise EvaluationError(
            f"{where} fields differ: missing={sorted(required - observed)}, "
            f"unknown={sorted(observed - required)}"
        )


def load_evaluation(root: Path) -> LoadedEvaluation:
    root = root.resolve()
    manifest_path = root / "eval" / "manifest.toml"
    manifest_bytes = manifest_path.read_bytes()
    manifest = tomllib.loads(manifest_bytes.decode("utf-8"))
    require_exact_keys(
        manifest,
        {
            "schema_version",
            "evaluation_id",
            "binary_commit",
            "migrations",
            "protocol",
            "files",
            "model",
            "retrieval",
            "tools",
        },
        "manifest",
    )
    if manifest.get("schema_version") != MANIFEST_SCHEMA:
        raise EvaluationError("unsupported evaluation manifest schema")
    if manifest.get("evaluation_id") != EVALUATION_ID:
        raise EvaluationError("unexpected evaluation ID")

    protocol = manifest.get("protocol")
    if not isinstance(protocol, dict):
        raise EvaluationError("manifest protocol must be a table")
    expected_protocol = {
        "query_count": 100,
        "minimum_corpora": 3,
        "minimum_semantic_queries": 30,
        "ranking_cutoff": 10,
        "exact_cutoff": 3,
        "bootstrap_seed": 2_026_080_201,
        "bootstrap_resamples": 10_000,
        "confidence": 0.95,
        "noninferiority_margin": -0.02,
        "semantic_gain": 0.05,
    }
    if protocol != expected_protocol:
        raise EvaluationError("manifest protocol differs from the canonical stable contract")

    files = manifest.get("files")
    if not isinstance(files, dict):
        raise EvaluationError("manifest files must be a table")
    require_exact_keys(
        files,
        {
            "corpora",
            "corpora_sha256",
            "queries",
            "queries_sha256",
            "query_order_sha256",
        },
        "manifest files",
    )
    corpora_path = root / safe_relative(str(files.get("corpora", "")))
    queries_path = root / safe_relative(str(files.get("queries", "")))
    if sha256_file(corpora_path) != files.get("corpora_sha256"):
        raise EvaluationError("corpora manifest fingerprint mismatch")
    if sha256_file(queries_path) != files.get("queries_sha256"):
        raise EvaluationError("query/label fingerprint mismatch")

    corpora_document = load_json(corpora_path)
    queries_document = load_json(queries_path)
    if not isinstance(corpora_document, dict) or not isinstance(queries_document, dict):
        raise EvaluationError("corpus and query manifests must be objects")
    require_exact_keys(corpora_document, {"schema_version", "source_commit", "corpora"}, "corpora")
    require_exact_keys(
        queries_document,
        {"schema_version", "evaluation_id", "tasks"},
        "queries",
    )
    if corpora_document.get("schema_version") != CORPORA_SCHEMA:
        raise EvaluationError("unsupported corpus manifest schema")
    if queries_document.get("schema_version") != QUERIES_SCHEMA:
        raise EvaluationError("unsupported query manifest schema")
    if queries_document.get("evaluation_id") != EVALUATION_ID:
        raise EvaluationError("query manifest evaluation ID mismatch")

    commit = str(manifest.get("binary_commit", ""))
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise EvaluationError("binary commit must be one full lowercase Git SHA")
    if corpora_document.get("source_commit") != commit:
        raise EvaluationError("corpus source commit differs from binary commit")

    corpora_rows = corpora_document.get("corpora")
    if not isinstance(corpora_rows, list) or len(corpora_rows) < protocol["minimum_corpora"]:
        raise EvaluationError("at least three corpora are required")
    corpora: dict[str, dict[str, Any]] = {}
    for row in corpora_rows:
        if not isinstance(row, dict):
            raise EvaluationError("every corpus must be an object")
        require_exact_keys(row, {"id", "description", "structure", "files"}, "corpus")
        corpus_id = str(row["id"])
        if not re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", corpus_id):
            raise EvaluationError(f"invalid corpus ID: {corpus_id!r}")
        if corpus_id in corpora:
            raise EvaluationError(f"duplicate corpus ID: {corpus_id}")
        file_rows = row["files"]
        if not isinstance(file_rows, list) or not file_rows:
            raise EvaluationError(f"corpus {corpus_id} has no files")
        files_by_path: dict[str, bytes] = {}
        for file_row in file_rows:
            if not isinstance(file_row, dict):
                raise EvaluationError(f"corpus {corpus_id} contains a non-object file")
            require_exact_keys(file_row, {"path", "sha256"}, "corpus file")
            path = safe_relative(str(file_row["path"]))
            if path in files_by_path:
                raise EvaluationError(f"corpus {corpus_id} repeats {path}")
            body = git_blob(root, commit, path)
            if sha256_bytes(body) != file_row["sha256"]:
                raise EvaluationError(f"frozen corpus hash mismatch for {path}")
            files_by_path[path] = body
        corpora[corpus_id] = {**row, "bodies": files_by_path}

    structures = {str(row["structure"]) for row in corpora_rows}
    if len(structures) < protocol["minimum_corpora"]:
        raise EvaluationError("corpora are not independently structured")

    tasks = queries_document.get("tasks")
    if not isinstance(tasks, list) or len(tasks) != protocol["query_count"]:
        raise EvaluationError("query manifest must contain exactly 100 tasks")
    seen_ids: set[str] = set()
    tags: set[str] = set()
    semantic_count = 0
    normalized_tasks: list[dict[str, Any]] = []
    for task in tasks:
        if not isinstance(task, dict):
            raise EvaluationError("every task must be an object")
        require_exact_keys(
            task,
            {"id", "corpus", "subset", "query", "tags", "accepted_spans"},
            "task",
        )
        task_id = str(task["id"])
        if not re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", task_id):
            raise EvaluationError(f"invalid task ID: {task_id!r}")
        if task_id in seen_ids:
            raise EvaluationError(f"duplicate task ID: {task_id}")
        seen_ids.add(task_id)
        corpus_id = str(task["corpus"])
        if corpus_id not in corpora:
            raise EvaluationError(f"task {task_id} names unknown corpus {corpus_id}")
        subset = str(task["subset"])
        if subset not in {"exact-token", "semantic-paraphrase", "scenario"}:
            raise EvaluationError(f"task {task_id} has unsupported subset {subset}")
        semantic_count += int(subset == "semantic-paraphrase")
        query = task["query"]
        if not isinstance(query, str) or not query.strip() or "\x00" in query:
            raise EvaluationError(f"task {task_id} has an invalid query")
        task_tags = task["tags"]
        if not isinstance(task_tags, list) or not all(
            isinstance(tag, str) and tag for tag in task_tags
        ):
            raise EvaluationError(f"task {task_id} has invalid tags")
        tags.update(task_tags)

        span_rows = task["accepted_spans"]
        if not isinstance(span_rows, list) or not span_rows:
            raise EvaluationError(f"task {task_id} requires an accepted span")
        accepted: list[AcceptedSpan] = []
        for span_row in span_rows:
            if not isinstance(span_row, dict):
                raise EvaluationError(f"task {task_id} has a non-object span")
            require_exact_keys(span_row, {"path", "needle", "occurrence", "grade"}, "span")
            path = safe_relative(str(span_row["path"]))
            body = corpora[corpus_id]["bodies"].get(path)
            if body is None:
                raise EvaluationError(f"task {task_id} span is outside corpus: {path}")
            needle = span_row["needle"]
            if not isinstance(needle, str):
                raise EvaluationError(f"task {task_id} span needle must be text")
            grade = span_row["grade"]
            if grade not in {1, 2, 3}:
                raise EvaluationError(f"task {task_id} span grade must be 1, 2, or 3")
            occurrence = span_row["occurrence"]
            if not isinstance(occurrence, int):
                raise EvaluationError(f"task {task_id} span occurrence must be an integer")
            start, end = nth_occurrence(body, needle.encode("utf-8"), occurrence)
            accepted.append(AcceptedSpan(path, start, end, int(grade)))
        normalized_tasks.append({**task, "accepted": accepted})

    if semantic_count < protocol["minimum_semantic_queries"]:
        raise EvaluationError("semantic/paraphrase subset is smaller than 30")
    missing_tags = REQUIRED_CASE_TAGS - tags
    if missing_tags:
        raise EvaluationError(f"required evaluation cases are missing: {sorted(missing_tags)}")
    if framed_digest(task["id"] for task in tasks) != files.get("query_order_sha256"):
        raise EvaluationError("frozen query order fingerprint mismatch")

    validate_frozen_inputs(root, commit, manifest)
    return LoadedEvaluation(
        root=root,
        manifest=manifest,
        manifest_sha256=sha256_bytes(manifest_bytes),
        corpora=corpora,
        tasks=normalized_tasks,
    )


def validate_frozen_inputs(root: Path, commit: str, manifest: Mapping[str, Any]) -> None:
    migrations = manifest.get("migrations")
    if not isinstance(migrations, list) or len(migrations) != 4:
        raise EvaluationError("exactly four schema migrations must be frozen")
    for row in migrations:
        if not isinstance(row, dict) or set(row) != {"path", "sha256"}:
            raise EvaluationError("invalid migration fingerprint row")
        path = safe_relative(str(row["path"]))
        if sha256_bytes(git_blob(root, commit, path)) != row["sha256"]:
            raise EvaluationError(f"migration fingerprint mismatch: {path}")
    model = manifest.get("model")
    if not isinstance(model, dict):
        raise EvaluationError("model input must be frozen")
    require_exact_keys(model, {"id", "manifest_path", "manifest_sha256"}, "model")
    path = safe_relative(str(model.get("manifest_path", "")))
    if sha256_bytes(git_blob(root, commit, path)) != model.get("manifest_sha256"):
        raise EvaluationError("model manifest fingerprint mismatch")
    retrieval = manifest.get("retrieval")
    if not isinstance(retrieval, dict):
        raise EvaluationError("retrieval settings must be frozen")
    required = {
        "config_fingerprint",
        "fts_tokenizer",
        "chunk_target_bytes",
        "chunk_max_bytes",
        "chunk_overlap_bytes",
        "rank_fusion_k",
        "rank_fusion_scale",
        "exact_weight",
        "lexical_weight",
        "vector_weight",
        "candidate_step",
        "candidate_cap_per_list",
        "result_limit",
        "metric_relevance_grades",
        "ndcg_gain",
        "ndcg_discount",
        "bootstrap_interval",
    }
    require_exact_keys(retrieval, required, "retrieval")
    tools = manifest.get("tools")
    if not isinstance(tools, dict):
        raise EvaluationError("tool inputs must be frozen")
    require_exact_keys(
        tools,
        {
            "hsum_version",
            "ripgrep_version",
            "ripgrep_args",
            "qmd_version",
            "qmd_embedding_model",
            "qmd_generation_model",
            "qmd_query_args",
        },
        "tools",
    )


def materialize_corpora(evaluation: LoadedEvaluation, output: Path) -> dict[str, Path]:
    output = output.resolve()
    if output.exists() and any(output.iterdir()):
        raise EvaluationError(f"materialization output is not empty: {output}")
    output.mkdir(parents=True, exist_ok=True)
    destinations: dict[str, Path] = {}
    for corpus_id, corpus in evaluation.corpora.items():
        destination = output / corpus_id
        destination.mkdir()
        (destination / ".git").mkdir()
        for relative, body in corpus["bodies"].items():
            target = destination / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(body)
        destinations[corpus_id] = destination
    return destinations


def spans_overlap(left_start: int, left_end: int, right_start: int, right_end: int) -> bool:
    return left_start < right_end and right_start < left_end


def normalize_result_path(value: str) -> str:
    value = value.strip().replace("\\", "/")
    for prefix in ("repo://", "qmd://"):
        if value.startswith(prefix):
            value = value[len(prefix) :]
    return value.removeprefix("./")


def grade_ranked_results(
    task: Mapping[str, Any], ranked: Sequence[Mapping[str, Any]], limit: int = 10
) -> tuple[list[int], set[int]]:
    accepted: Sequence[AcceptedSpan] = task["accepted"]
    grades: list[int] = []
    matched: set[int] = set()
    for result in ranked[:limit]:
        path = normalize_result_path(str(result.get("path") or result.get("source_uri") or ""))
        start = result.get("start_byte")
        end = result.get("end_byte")
        candidates: list[tuple[int, AcceptedSpan]] = []
        for index, span in enumerate(accepted):
            if span.path != path:
                continue
            if isinstance(start, int) and isinstance(end, int):
                if spans_overlap(start, end, span.start, span.end):
                    candidates.append((index, span))
            else:
                candidates.append((index, span))
        if candidates:
            highest = max(span.grade for _, span in candidates)
            grades.append(highest)
            matched.update(index for index, span in candidates if span.grade == highest)
        else:
            grades.append(0)
    return grades, matched


def dcg(values: Sequence[int]) -> float:
    return sum(((2**grade) - 1) / math.log2(rank + 1) for rank, grade in enumerate(values, 1))


def ndcg_at_k(grades: Sequence[int], ideal_grades: Sequence[int], k: int = 10) -> float:
    ideal = dcg(sorted(ideal_grades, reverse=True)[:k])
    return 0.0 if ideal == 0.0 else dcg(grades[:k]) / ideal


def mrr_at_k(grades: Sequence[int], k: int = 10) -> float:
    for rank, grade in enumerate(grades[:k], 1):
        if grade >= 2:
            return 1.0 / rank
    return 0.0


def top_k_recall(matched: set[int], accepted: Sequence[AcceptedSpan], k_grades: set[int]) -> float:
    relevant = {index for index, span in enumerate(accepted) if span.grade in k_grades}
    return 0.0 if not relevant else len(matched & relevant) / len(relevant)


def score_task(task: Mapping[str, Any], ranked: Sequence[Mapping[str, Any]]) -> dict[str, float]:
    grades, matched = grade_ranked_results(task, ranked, 10)
    accepted: Sequence[AcceptedSpan] = task["accepted"]
    top_three_grades, top_three_matched = grade_ranked_results(task, ranked, 3)
    return {
        "ndcg@10": ndcg_at_k(grades, [span.grade for span in accepted], 10),
        "mrr@10": mrr_at_k(grades, 10),
        "recall@10": top_k_recall(matched, accepted, {2, 3}),
        "exact_top3_recall": top_k_recall(top_three_matched, accepted, {2, 3}),
        "top3_relevant": float(any(grade >= 2 for grade in top_three_grades)),
    }


def nearest_rank(values: Sequence[float], probability: float) -> float:
    if not values:
        raise EvaluationError("cannot compute a percentile over an empty sample")
    rank = max(1, min(len(values), math.ceil(probability * len(values))))
    return sorted(values)[rank - 1]


def paired_bootstrap(
    left: Sequence[float],
    right: Sequence[float],
    *,
    seed: int,
    resamples: int,
    confidence: float,
) -> dict[str, float]:
    if len(left) != len(right) or not left:
        raise EvaluationError("paired bootstrap requires equal non-empty samples")
    differences = [a - b for a, b in zip(left, right, strict=True)]
    random = Random(seed)
    estimates = [
        statistics.mean(differences[random.randrange(len(differences))] for _ in differences)
        for _ in range(resamples)
    ]
    tail = (1.0 - confidence) / 2.0
    return {
        "estimate": statistics.mean(differences),
        "lower": nearest_rank(estimates, tail),
        "upper": nearest_rank(estimates, 1.0 - tail),
    }


def task_metrics_by_retriever(result: Mapping[str, Any], name: str) -> list[dict[str, Any]]:
    retriever = result.get("retrievers", {}).get(name)
    if not isinstance(retriever, dict) or not isinstance(retriever.get("tasks"), list):
        raise EvaluationError(f"result has no task metrics for {name}")
    return retriever["tasks"]


def promotion_decision(result: Mapping[str, Any]) -> dict[str, Any]:
    protocol = result.get("protocol")
    if not isinstance(protocol, dict):
        raise EvaluationError("result protocol is missing")
    seed = int(protocol["bootstrap_seed"])
    resamples = int(protocol["bootstrap_resamples"])
    confidence = float(protocol["confidence"])
    margin = float(protocol["noninferiority_margin"])
    required_gain = float(protocol["semantic_gain"])

    by_name = {
        name: task_metrics_by_retriever(result, name) for name in HSUM_RETRIEVERS
    }
    task_ids = [[row["task_id"] for row in rows] for rows in by_name.values()]
    if any(ids != task_ids[0] for ids in task_ids[1:]):
        raise EvaluationError("hSUM retriever task order differs")

    comparisons: dict[str, Any] = {}
    noninferiority_passes = True
    for metric in ("ndcg@10", "mrr@10"):
        lexical = [float(row["metrics"][metric]) for row in by_name["hsum-lexical"]]
        semantic = [float(row["metrics"][metric]) for row in by_name["hsum-semantic"]]
        hybrid = [float(row["metrics"][metric]) for row in by_name["hsum-hybrid"]]
        better_name, better = (
            ("hsum-lexical", lexical)
            if statistics.mean(lexical) >= statistics.mean(semantic)
            else ("hsum-semantic", semantic)
        )
        interval = paired_bootstrap(
            hybrid,
            better,
            seed=seed,
            resamples=resamples,
            confidence=confidence,
        )
        passed = interval["lower"] >= margin
        noninferiority_passes &= passed
        comparisons[metric] = {"baseline": better_name, **interval, "passed": passed}

    tasks = result.get("tasks")
    if not isinstance(tasks, list):
        raise EvaluationError("result tasks are missing")
    semantic_indices = [
        index for index, task in enumerate(tasks) if task.get("subset") == "semantic-paraphrase"
    ]
    hybrid_ndcg = [float(row["metrics"]["ndcg@10"]) for row in by_name["hsum-hybrid"]]
    lexical_ndcg = [float(row["metrics"]["ndcg@10"]) for row in by_name["hsum-lexical"]]
    semantic_interval = paired_bootstrap(
        [hybrid_ndcg[index] for index in semantic_indices],
        [lexical_ndcg[index] for index in semantic_indices],
        seed=seed,
        resamples=resamples,
        confidence=confidence,
    )
    positive_value = (
        semantic_interval["estimate"] >= required_gain and semantic_interval["lower"] >= 0.0
    )

    exact_indices = [
        index for index, task in enumerate(tasks) if task.get("subset") == "exact-token"
    ]
    lexical_exact = [
        float(by_name["hsum-lexical"][index]["metrics"]["exact_top3_recall"])
        for index in exact_indices
    ]
    hybrid_exact = [
        float(by_name["hsum-hybrid"][index]["metrics"]["exact_top3_recall"])
        for index in exact_indices
    ]
    exact_all_top3 = all(value == 1.0 for value in hybrid_exact)
    exact_no_regression = statistics.mean(hybrid_exact) >= statistics.mean(lexical_exact)

    promoted = noninferiority_passes and positive_value and exact_all_top3 and exact_no_regression
    return {
        "disposition": "promote-hybrid" if promoted else "stable-lexical-hybrid-beta",
        "promoted": promoted,
        "overall_noninferiority": {"passed": noninferiority_passes, **comparisons},
        "semantic_positive_value": {**semantic_interval, "passed": positive_value},
        "exact_token": {
            "query_count": len(exact_indices),
            "lexical_top3_recall": statistics.mean(lexical_exact),
            "hybrid_top3_recall": statistics.mean(hybrid_exact),
            "all_hybrid_queries_top3": exact_all_top3,
            "no_regression": exact_no_regression,
            "passed": exact_all_top3 and exact_no_regression,
        },
    }


def hsum_ranked(payload: Mapping[str, Any]) -> list[dict[str, Any]]:
    ranked: list[dict[str, Any]] = []
    for row in payload.get("results", []):
        if not isinstance(row, dict):
            continue
        span = row.get("span") if isinstance(row.get("span"), dict) else {}
        ranked.append(
            {
                "path": normalize_result_path(str(row.get("source_uri", ""))),
                "start_byte": span.get("start_byte"),
                "end_byte": span.get("end_byte"),
                "citation_uri": row.get("citation_uri"),
            }
        )
    return ranked


def verify_hsum_citations(
    ranked: Sequence[Mapping[str, Any]],
    *,
    hsum: Path,
    root: Path,
    env: Mapping[str, str],
    cache: dict[tuple[str, str], bool],
) -> float | None:
    citations = [row.get("citation_uri") for row in ranked]
    if not citations:
        return None
    valid = 0
    for citation in citations:
        if not isinstance(citation, str) or not citation:
            continue
        key = (str(root), citation)
        if key not in cache:
            observation = run_process(
                [
                    str(hsum),
                    "get",
                    citation,
                    "--json",
                    "--no-progress",
                    "--no-color",
                ],
                cwd=root,
                env=env,
                timeout=30.0,
            )
            resolved = False
            if observation["exit_code"] == 0:
                try:
                    payload = json.loads(observation["stdout"])
                except json.JSONDecodeError:
                    payload = None
                resolved = (
                    isinstance(payload, dict)
                    and payload.get("requested_citation_uri") == citation
                    and isinstance(payload.get("returned_citation_uri"), str)
                    and isinstance(payload.get("content"), str)
                    and bool(payload["content"])
                )
            cache[key] = resolved
        valid += int(cache[key])
    return valid / len(citations)


def ripgrep_ranked(output: str) -> list[dict[str, Any]]:
    ranked: list[dict[str, Any]] = []
    for line in output.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("type") != "match":
            continue
        data = event.get("data", {})
        path = data.get("path", {}).get("text")
        absolute_offset = data.get("absolute_offset")
        submatches = data.get("submatches", [])
        if not isinstance(path, str) or not isinstance(absolute_offset, int) or not submatches:
            continue
        first = submatches[0]
        start = absolute_offset + int(first.get("start", 0))
        end = absolute_offset + int(first.get("end", 0))
        ranked.append({"path": normalize_result_path(path), "start_byte": start, "end_byte": end})
    return ranked


def qmd_ranked(payload: Any, corpus_id: str) -> list[dict[str, Any]]:
    rows = payload if isinstance(payload, list) else payload.get("results", [])
    ranked: list[dict[str, Any]] = []
    for row in rows:
        if not isinstance(row, dict):
            continue
        value = row.get("file") or row.get("path") or row.get("uri") or row.get("source")
        if not isinstance(value, str):
            continue
        path = normalize_result_path(value)
        path = path.removeprefix(f"{corpus_id}/")
        ranked.append({"path": path})
    return ranked


def aggregate_task_rows(rows: Sequence[Mapping[str, Any]]) -> dict[str, float]:
    metrics = ("ndcg@10", "mrr@10", "recall@10", "exact_top3_recall")
    return {
        metric: statistics.mean(float(row["metrics"][metric]) for row in rows)
        for metric in metrics
    }


def evaluate_observations(
    evaluation: LoadedEvaluation,
    observations: Mapping[str, Mapping[str, Sequence[Mapping[str, Any]]]],
) -> dict[str, Any]:
    retrievers: dict[str, Any] = {}
    for name, by_task in observations.items():
        rows: list[dict[str, Any]] = []
        for task in evaluation.tasks:
            task_observations = by_task.get(task["id"])
            if not task_observations:
                raise EvaluationError(f"{name} has no observation for {task['id']}")
            representative = task_observations[-1]
            ranked = representative.get("ranked")
            if not isinstance(ranked, list):
                raise EvaluationError(f"{name}/{task['id']} has no ranked result list")
            metrics = score_task(task, ranked)
            rows.append(
                {
                    "task_id": task["id"],
                    "corpus": task["corpus"],
                    "subset": task["subset"],
                    "metrics": metrics,
                    "ranked": ranked[:10],
                    "latency_ms": [float(item.get("elapsed_ms", 0.0)) for item in task_observations],
                    "context_bytes": int(representative.get("context_bytes", 0)),
                    "citation_correctness": representative.get("citation_correctness"),
                }
            )
        latencies = [sample for row in rows for sample in row["latency_ms"]]
        retrievers[name] = {
            "aggregate": {
                **aggregate_task_rows(rows),
                "latency_median_ms": statistics.median(latencies) if latencies else 0.0,
                "context_bytes": sum(row["context_bytes"] for row in rows),
                "citation_correctness": statistics.mean(
                    float(row["citation_correctness"])
                    for row in rows
                    if isinstance(row["citation_correctness"], (int, float))
                )
                if any(isinstance(row["citation_correctness"], (int, float)) for row in rows)
                else None,
            },
            "tasks": rows,
        }
    protocol = dict(evaluation.manifest["protocol"])
    result: dict[str, Any] = {
        "schema_version": RESULT_SCHEMA,
        "evaluation_id": EVALUATION_ID,
        "manifest_sha256": evaluation.manifest_sha256,
        "binary_commit": evaluation.manifest["binary_commit"],
        "protocol": protocol,
        "tasks": [
            {
                "id": task["id"],
                "corpus": task["corpus"],
                "subset": task["subset"],
                "tags": task["tags"],
            }
            for task in evaluation.tasks
        ],
        "retrievers": retrievers,
    }
    if all(name in retrievers for name in HSUM_RETRIEVERS):
        result["promotion"] = promotion_decision(result)
    return result


def require_tool_version(command: Sequence[str], expected: str, cwd: Path) -> str:
    observation = run_process(command, cwd=cwd, timeout=30.0)
    if observation["exit_code"] != 0:
        raise EvaluationError(f"unable to identify tool version: {command!r}")
    output = (observation["stdout"] + observation["stderr"]).strip()
    if expected not in output:
        raise EvaluationError(f"tool version mismatch: expected {expected!r}, observed {output!r}")
    return output


def initialize_hsum_corpora(
    evaluation: LoadedEvaluation,
    roots: Mapping[str, Path],
    *,
    hsum: Path,
    hsum_home: Path,
) -> dict[str, Any]:
    env = os.environ.copy()
    env.update({"HSUM_HOME": str(hsum_home), "HSUM_OFFLINE": "1"})
    version = require_tool_version(
        [str(hsum), "--version"],
        str(evaluation.manifest["tools"]["hsum_version"]),
        evaluation.root,
    )
    setup_started = time.perf_counter_ns()
    verify = run_process(
        [
            str(hsum),
            "model",
            "verify",
            str(evaluation.manifest["model"]["id"]),
            "--json",
        ],
        cwd=evaluation.root,
        env=env,
    )
    if verify["exit_code"] != 0:
        raise EvaluationError(
            "the frozen embedding artifact is not installed and verified; run the explicit "
            "model preparation command before evaluation"
        )
    setup: dict[str, Any] = {
        "version": version,
        "binary_sha256": sha256_file(hsum),
        "model_verify_ms": verify["elapsed_ms"],
        "corpora": {},
    }
    for corpus_id, root in roots.items():
        initialized = run_process(
            [
                str(hsum),
                "init",
                ".",
                "--index",
                f"eval-{corpus_id}",
                "--project",
                "default",
                "--embedding-model",
                str(evaluation.manifest["model"]["id"]),
                "--no-color",
                "--no-progress",
            ],
            cwd=root,
            env=env,
            timeout=300.0,
        )
        if initialized["exit_code"] != 0:
            raise EvaluationError(f"hSUM init failed for {corpus_id}: {initialized['stderr'].strip()}")
        reembed = run_process(
            [str(hsum), "ingest", "--reembed", "--no-color", "--no-progress"],
            cwd=root,
            env=env,
            timeout=1_800.0,
        )
        if reembed["exit_code"] != 0:
            raise EvaluationError(
                f"hSUM re-embed failed for {corpus_id}: {reembed['stderr'].strip()}"
            )
        setup["corpora"][corpus_id] = {
            "init_ms": initialized["elapsed_ms"],
            "reembed_ms": reembed["elapsed_ms"],
        }
    setup["setup_ms"] = (time.perf_counter_ns() - setup_started) / 1_000_000
    setup["env"] = env
    return setup


def collect_hsum(
    evaluation: LoadedEvaluation,
    roots: Mapping[str, Path],
    *,
    hsum: Path,
    env: Mapping[str, str],
    runs: int,
) -> dict[str, dict[str, list[dict[str, Any]]]]:
    output = {name: {} for name in HSUM_RETRIEVERS}
    citation_cache: dict[tuple[str, str], bool] = {}
    for task in evaluation.tasks:
        root = roots[task["corpus"]]
        for name in HSUM_RETRIEVERS:
            mode = name.removeprefix("hsum-")
            samples: list[dict[str, Any]] = []
            for _ in range(runs):
                observation = run_process(
                    [
                        str(hsum),
                        "search",
                        "--mode",
                        mode,
                        "--limit",
                        "10",
                        "--timeout-ms",
                        "10000",
                        "--explain",
                        "--json",
                        "--",
                        task["query"],
                    ],
                    cwd=root,
                    env=env,
                    timeout=30.0,
                )
                if observation["exit_code"] != 0:
                    raise EvaluationError(
                        f"{name} failed for {task['id']}: {observation['stderr'].strip()}"
                    )
                payload = json.loads(observation["stdout"])
                ranked = hsum_ranked(payload)
                samples.append(
                    {
                        "ranked": ranked,
                        "elapsed_ms": observation["elapsed_ms"],
                        "context_bytes": len(observation["stdout"].encode("utf-8")),
                        "citation_correctness": verify_hsum_citations(
                            ranked,
                            hsum=hsum,
                            root=root,
                            env=env,
                            cache=citation_cache,
                        ),
                    }
                )
            output[name][task["id"]] = samples
    return output


def collect_ripgrep(
    evaluation: LoadedEvaluation,
    roots: Mapping[str, Path],
    *,
    runs: int,
) -> tuple[dict[str, list[dict[str, Any]]], dict[str, Any]]:
    expected = str(evaluation.manifest["tools"]["ripgrep_version"])
    version = require_tool_version(["rg", "--version"], expected, evaluation.root)
    output: dict[str, list[dict[str, Any]]] = {}
    for task in evaluation.tasks:
        root = roots[task["corpus"]]
        samples = []
        for _ in range(runs):
            observation = run_process(
                ["rg", "--json", "--threads", "1", "--sort", "path", "--fixed-strings", "--", task["query"], "."],
                cwd=root,
                timeout=30.0,
            )
            if observation["exit_code"] not in {0, 1}:
                raise EvaluationError(
                    f"ripgrep failed for {task['id']}: {observation['stderr'].strip()}"
                )
            samples.append(
                {
                    "ranked": ripgrep_ranked(observation["stdout"])[:10],
                    "elapsed_ms": observation["elapsed_ms"],
                    "context_bytes": len(observation["stdout"].encode("utf-8")),
                }
            )
        output[task["id"]] = samples
    return output, {"version": version, "setup_ms": 0.0}


def qmd_query_command(qmd: Path, task: Mapping[str, Any]) -> list[str]:
    return [
        str(qmd),
        "query",
        "-c",
        str(task["corpus"]),
        "-n",
        "10",
        "--no-rerank",
        "--format",
        "json",
        "--",
        str(task["query"]),
    ]


def parse_qmd_json(observation: Mapping[str, Any], where: str) -> Any:
    try:
        return json.loads(str(observation["stdout"]))
    except json.JSONDecodeError as error:
        stdout = str(observation.get("stdout", ""))[:400]
        stderr = str(observation.get("stderr", ""))[:400]
        raise EvaluationError(
            f"QMD returned invalid JSON for {where}: stdout={stdout!r}, stderr={stderr!r}"
        ) from error


def collect_qmd(
    evaluation: LoadedEvaluation,
    roots: Mapping[str, Path],
    *,
    qmd: Path,
    work: Path,
    runs: int,
) -> tuple[dict[str, list[dict[str, Any]]], dict[str, Any]]:
    expected = str(evaluation.manifest["tools"]["qmd_version"])
    version = require_tool_version([str(qmd), "--version"], expected, evaluation.root)
    env = os.environ.copy()
    env.update(
        {
            "QMD_CONFIG_DIR": str(work / "qmd-config"),
            "XDG_CACHE_HOME": str(work / "qmd-cache"),
            "QMD_EMBED_MODEL": str(evaluation.manifest["tools"]["qmd_embedding_model"]),
            "QMD_GENERATE_MODEL": str(
                evaluation.manifest["tools"]["qmd_generation_model"]
            ),
        }
    )
    setup_started = time.perf_counter_ns()
    for corpus_id, root in roots.items():
        observation = run_process(
            [str(qmd), "collection", "add", str(root), "--name", corpus_id, "--mask", "**/*.{rs,md}"],
            cwd=evaluation.root,
            env=env,
            timeout=300.0,
        )
        if observation["exit_code"] != 0:
            raise EvaluationError(f"QMD collection setup failed: {observation['stderr'].strip()}")
    embedded = run_process([str(qmd), "embed"], cwd=evaluation.root, env=env, timeout=3_600.0)
    if embedded["exit_code"] != 0:
        raise EvaluationError(f"QMD embedding failed: {embedded['stderr'].strip()}")
    warmup_ms = 0.0
    for attempt in range(2):
        warmup = run_process(
            qmd_query_command(qmd, evaluation.tasks[0]),
            cwd=evaluation.root,
            env=env,
            timeout=600.0,
        )
        warmup_ms += float(warmup["elapsed_ms"])
        if warmup["exit_code"] != 0:
            raise EvaluationError(f"QMD warm-up failed: {warmup['stderr'].strip()}")
        try:
            parse_qmd_json(warmup, f"warm-up attempt {attempt + 1}")
        except EvaluationError:
            if attempt == 1:
                raise
        else:
            break
    setup = {
        "version": version,
        "embedding_model": env["QMD_EMBED_MODEL"],
        "generation_model": env["QMD_GENERATE_MODEL"],
        "setup_ms": (time.perf_counter_ns() - setup_started) / 1_000_000,
        "warmup_ms": warmup_ms,
        "embedding_stdout_bytes": len(embedded["stdout"].encode("utf-8")),
    }
    output: dict[str, list[dict[str, Any]]] = {}
    for task in evaluation.tasks:
        samples = []
        for _ in range(runs):
            observation = run_process(
                qmd_query_command(qmd, task),
                cwd=evaluation.root,
                env=env,
                timeout=300.0,
            )
            if observation["exit_code"] != 0:
                raise EvaluationError(f"QMD failed for {task['id']}: {observation['stderr'].strip()}")
            payload = parse_qmd_json(observation, task["id"])
            samples.append(
                {
                    "ranked": qmd_ranked(payload, task["corpus"])[:10],
                    "elapsed_ms": observation["elapsed_ms"],
                    "context_bytes": len(observation["stdout"].encode("utf-8")),
                }
            )
        output[task["id"]] = samples
    return output, setup


def write_json(path: Path, value: Any) -> None:
    path = path.resolve()
    if path.exists():
        raise EvaluationError(f"refusing to overwrite evaluation output: {path}")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(json.dumps(value, indent=2, sort_keys=True).encode() + b"\n")


def format_metric(value: Any, *, percent: bool = False) -> str:
    if not isinstance(value, (int, float)):
        return "n/a"
    return f"{100.0 * value:.1f}%" if percent else f"{value:.4f}"


def render_result(result: Mapping[str, Any]) -> str:
    retrievers = result.get("retrievers")
    setup = result.get("setup")
    promotion = result.get("promotion")
    if not isinstance(retrievers, dict) or not isinstance(setup, dict):
        raise EvaluationError("result is missing retriever or setup evidence")
    if not isinstance(promotion, dict):
        raise EvaluationError("result has no hSUM promotion decision")

    lines = [
        "# hSUM stable-v0.1 held-out retrieval result",
        "",
        f"- Manifest: `{result.get('manifest_sha256', 'missing')}`",
        f"- Binary commit: `{result.get('binary_commit', 'missing')}`",
        f"- Queries: {len(result.get('tasks', []))}",
        f"- Disposition: **{promotion.get('disposition', 'missing')}**",
        f"- External comparison complete: {str(bool(result.get('external_comparison_complete'))).lower()}",
        "",
        "## Aggregate retrieval evidence",
        "",
        "| Retriever | NDCG@10 | MRR@10 | Recall@10 | Exact top-3 recall | Median latency | Context bytes | Citation correctness | Setup |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for name, row in retrievers.items():
        if not isinstance(row, dict) or not isinstance(row.get("aggregate"), dict):
            raise EvaluationError(f"result aggregate is missing for {name}")
        aggregate = row["aggregate"]
        tool_setup = setup.get(name.removeprefix("hsum-"))
        if name.startswith("hsum-"):
            tool_setup = setup.get("hsum")
        setup_ms = tool_setup.get("setup_ms") if isinstance(tool_setup, dict) else None
        lines.append(
            "| "
            + " | ".join(
                [
                    name,
                    format_metric(aggregate.get("ndcg@10")),
                    format_metric(aggregate.get("mrr@10")),
                    format_metric(aggregate.get("recall@10")),
                    format_metric(aggregate.get("exact_top3_recall")),
                    f"{float(aggregate.get('latency_median_ms', 0.0)):.2f} ms",
                    str(aggregate.get("context_bytes", "n/a")),
                    format_metric(aggregate.get("citation_correctness"), percent=True),
                    f"{float(setup_ms):.2f} ms" if isinstance(setup_ms, (int, float)) else "n/a",
                ]
            )
            + " |"
        )

    overall = promotion.get("overall_noninferiority", {})
    semantic = promotion.get("semantic_positive_value", {})
    exact = promotion.get("exact_token", {})
    lines.extend(
        [
            "",
            "## Promotion gates",
            "",
            "| Gate | Baseline | Estimate | 95% lower | 95% upper | Pass |",
            "|---|---|---:|---:|---:|---:|",
        ]
    )
    for metric in ("ndcg@10", "mrr@10"):
        comparison = overall.get(metric, {})
        lines.append(
            f"| Overall hybrid {metric} non-inferiority | {comparison.get('baseline', 'n/a')} "
            f"| {format_metric(comparison.get('estimate'))} "
            f"| {format_metric(comparison.get('lower'))} "
            f"| {format_metric(comparison.get('upper'))} "
            f"| {str(bool(comparison.get('passed'))).lower()} |"
        )
    lines.extend(
        [
            f"| Semantic-subset hybrid NDCG@10 gain | hsum-lexical "
            f"| {format_metric(semantic.get('estimate'))} "
            f"| {format_metric(semantic.get('lower'))} "
            f"| {format_metric(semantic.get('upper'))} "
            f"| {str(bool(semantic.get('passed'))).lower()} |",
            f"| Exact-token hybrid top-3 | hsum-lexical "
            f"| {format_metric(exact.get('hybrid_top3_recall'))} "
            f"| n/a | n/a | {str(bool(exact.get('passed'))).lower()} |",
            "",
            "Grades 2 and 3 are relevant. NDCG gain is `2^grade - 1`; discount is "
            "`log2(rank + 1)`. Confidence intervals use the manifest's deterministic "
            "10,000-resample paired bootstrap. ripgrep and QMD are report-only; QMD "
            "is path-scored because it does not expose hSUM byte-span citations.",
            "",
        ]
    )
    return "\n".join(lines)


def write_text(path: Path, value: str) -> None:
    path = path.resolve()
    if path.exists():
        raise EvaluationError(f"refusing to overwrite evaluation output: {path}")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(value, encoding="utf-8")


def run_evaluation(args: argparse.Namespace) -> dict[str, Any]:
    evaluation = load_evaluation(args.repo)
    hsum = args.hsum.resolve()
    if not hsum.is_file():
        raise EvaluationError(f"hSUM binary does not exist: {hsum}")
    work = args.work.resolve()
    if work.exists() and any(work.iterdir()):
        raise EvaluationError(f"evaluation work directory is not empty: {work}")
    work.mkdir(parents=True, exist_ok=True)
    model_home = args.model_home.resolve()
    if not model_home.is_dir():
        raise EvaluationError(f"prepared model home does not exist: {model_home}")
    shutil.copytree(model_home, work / "hsum-home")
    roots = materialize_corpora(evaluation, work / "corpora")
    setup = initialize_hsum_corpora(
        evaluation, roots, hsum=hsum, hsum_home=work / "hsum-home"
    )
    env = setup.pop("env")
    observations: dict[str, Any] = collect_hsum(
        evaluation, roots, hsum=hsum, env=env, runs=args.runs
    )
    observations["ripgrep"], ripgrep_setup = collect_ripgrep(
        evaluation, roots, runs=args.runs
    )
    qmd_setup = None
    if args.qmd is not None:
        observations["qmd"], qmd_setup = collect_qmd(
            evaluation,
            roots,
            qmd=args.qmd.resolve(),
            work=work,
            runs=args.runs,
        )
    result = evaluate_observations(evaluation, observations)
    result["setup"] = {"hsum": setup, "ripgrep": ripgrep_setup, "qmd": qmd_setup}
    result["external_comparison_complete"] = qmd_setup is not None
    write_json(args.output, result)
    return result


def prepare_model(args: argparse.Namespace) -> dict[str, Any]:
    evaluation = load_evaluation(args.repo)
    hsum = args.hsum.resolve()
    home = args.home.resolve()
    if home.exists() and any(home.iterdir()):
        raise EvaluationError(f"model preparation home is not empty: {home}")
    home.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env["HSUM_HOME"] = str(home)
    env.pop("HSUM_OFFLINE", None)
    observation = run_process(
        [
            str(hsum),
            "model",
            "install",
            "embedding",
            str(evaluation.manifest["model"]["id"]),
            "--json",
        ],
        cwd=evaluation.root,
        env=env,
        timeout=3_600.0,
    )
    if observation["exit_code"] != 0:
        raise EvaluationError(f"explicit model installation failed: {observation['stderr'].strip()}")
    return {
        "home": str(home),
        "elapsed_ms": observation["elapsed_ms"],
        "receipt": json.loads(observation["stdout"]),
    }


def load_result(path: Path) -> dict[str, Any]:
    value = load_json(path)
    if not isinstance(value, dict) or value.get("schema_version") != RESULT_SCHEMA:
        raise EvaluationError(f"unsupported result file: {path}")
    return value


def compare_results(left: Path, right: Path) -> dict[str, Any]:
    first = load_result(left)
    second = load_result(right)
    for field in ("evaluation_id", "manifest_sha256", "binary_commit", "protocol", "tasks"):
        if first.get(field) != second.get(field):
            raise EvaluationError(f"result comparison refused: {field} differs")
    comparison: dict[str, Any] = {"manifest_sha256": first["manifest_sha256"], "retrievers": {}}
    for name in sorted(set(first["retrievers"]) & set(second["retrievers"])):
        left_rows = task_metrics_by_retriever(first, name)
        right_rows = task_metrics_by_retriever(second, name)
        if [row["task_id"] for row in left_rows] != [row["task_id"] for row in right_rows]:
            raise EvaluationError(f"result comparison refused: {name} task order differs")
        comparison["retrievers"][name] = {
            metric: statistics.mean(
                float(right_row["metrics"][metric]) - float(left_row["metrics"][metric])
                for left_row, right_row in zip(left_rows, right_rows, strict=True)
            )
            for metric in ("ndcg@10", "mrr@10", "recall@10")
        }
    return comparison


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    validate = subparsers.add_parser("validate", help="validate every frozen input")
    validate.add_argument("--repo", type=Path, default=Path.cwd())

    materialize = subparsers.add_parser("materialize", help="materialize frozen Git blobs")
    materialize.add_argument("--repo", type=Path, default=Path.cwd())
    materialize.add_argument("--output", type=Path, required=True)

    prepare = subparsers.add_parser(
        "prepare-model", help="explicitly install the frozen model into an isolated seed home"
    )
    prepare.add_argument("--repo", type=Path, default=Path.cwd())
    prepare.add_argument("--hsum", type=Path, required=True)
    prepare.add_argument("--home", type=Path, required=True)

    run = subparsers.add_parser("run", help="run the frozen retrieval evaluation")
    run.add_argument("--repo", type=Path, default=Path.cwd())
    run.add_argument("--hsum", type=Path, required=True)
    run.add_argument("--model-home", type=Path, required=True)
    run.add_argument("--qmd", type=Path)
    run.add_argument("--work", type=Path, required=True)
    run.add_argument("--output", type=Path, required=True)
    run.add_argument("--runs", type=int, default=1, choices=range(1, 6))

    compare = subparsers.add_parser("compare", help="compare matching frozen results")
    compare.add_argument("left", type=Path)
    compare.add_argument("right", type=Path)

    render = subparsers.add_parser("render", help="render one result as Markdown")
    render.add_argument("result", type=Path)
    render.add_argument("--output", type=Path, required=True)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        if args.command == "validate":
            evaluation = load_evaluation(args.repo)
            print(
                json.dumps(
                    {
                        "evaluation_id": EVALUATION_ID,
                        "manifest_sha256": evaluation.manifest_sha256,
                        "corpora": len(evaluation.corpora),
                        "queries": len(evaluation.tasks),
                        "semantic_queries": sum(
                            task["subset"] == "semantic-paraphrase" for task in evaluation.tasks
                        ),
                    },
                    sort_keys=True,
                )
            )
        elif args.command == "materialize":
            materialize_corpora(load_evaluation(args.repo), args.output)
        elif args.command == "prepare-model":
            print(json.dumps(prepare_model(args), indent=2, sort_keys=True))
        elif args.command == "run":
            result = run_evaluation(args)
            print(json.dumps(result.get("promotion"), indent=2, sort_keys=True))
        elif args.command == "compare":
            print(json.dumps(compare_results(args.left, args.right), indent=2, sort_keys=True))
        elif args.command == "render":
            write_text(args.output, render_result(load_result(args.result)))
            print(args.output.resolve())
        else:  # pragma: no cover - argparse enforces the command set.
            raise EvaluationError(f"unsupported command: {args.command}")
    except (EvaluationError, OSError, UnicodeError, ValueError, json.JSONDecodeError) as error:
        print(f"evaluation failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
