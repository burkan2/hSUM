#!/usr/bin/env python3
"""Freeze and compare hSUM retrieval ordering across native processes/targets."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import pathlib
import platform
import select
import shutil
import sqlite3
import subprocess
import sys
from typing import Any


ROOT = pathlib.Path(__file__).resolve().parent
MANIFEST_PATH = ROOT / "manifest.json"
CORPUS_ROOT = ROOT / "corpus"
SCHEMA_VERSION = "hsum.retrieval-determinism-report.v1"
COMPARISON_SCHEMA_VERSION = "hsum.retrieval-determinism-comparison.v1"
INDEX_NAME = "determinism"
PROJECT_NAME = "default"
FLOAT_TOLERANCE = 1e-6
REPORT_FIELDS = {
    "schema_version",
    "passed",
    "commit_sha",
    "platform",
    "manifest_sha256",
    "corpus_fingerprint_sha256",
    "index_sha256",
    "binary_sha256",
    "binary_version",
    "runs_per_transport",
    "queries",
    "checks",
    "observations",
}
CHECK_FIELDS = {
    "cli_fresh_processes_identical",
    "mcp_fresh_processes_identical",
    "cli_mcp_equivalent",
    "lexical_query_embedding_absent",
    "duplicate_set_exercised",
    "multiple_explanation_lists_exercised",
    "equal_backend_score_tie_exercised",
}


class QualificationError(RuntimeError):
    """One frozen qualification invariant failed."""


def sha256_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def load_manifest() -> dict[str, Any]:
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    expected_keys = {
        "schema_version",
        "fixture_repository",
        "fixed_mtime_unix",
        "runs_per_transport",
        "limit",
        "timeout_ms",
        "corpus",
        "queries",
    }
    if set(manifest) != expected_keys:
        raise QualificationError("manifest fields changed")
    if manifest["schema_version"] != "hsum.retrieval-determinism-manifest.v1":
        raise QualificationError("unsupported manifest schema")
    fixture_repository = pathlib.Path(manifest["fixture_repository"])
    if not fixture_repository.is_absolute() or fixture_repository != pathlib.Path(
        "/opt/hsum-retrieval-determinism-repository"
    ):
        raise QualificationError("fixture repository path is not frozen")
    if manifest["runs_per_transport"] != 5:
        raise QualificationError("exactly five fresh processes are required")
    if not 1 <= manifest["limit"] <= 50:
        raise QualificationError("result limit is invalid")
    if not 100 <= manifest["timeout_ms"] <= 10_000:
        raise QualificationError("search timeout is invalid")
    if not isinstance(manifest["fixed_mtime_unix"], int):
        raise QualificationError("fixture timestamp is invalid")

    corpus_entries = manifest["corpus"]
    if not isinstance(corpus_entries, list) or len(corpus_entries) != 5:
        raise QualificationError("corpus inventory changed")
    expected_paths: list[str] = []
    for entry in corpus_entries:
        if set(entry) != {"path", "sha256"}:
            raise QualificationError("corpus entry fields changed")
        relative = pathlib.PurePosixPath(entry["path"])
        if relative.is_absolute() or len(relative.parts) != 1 or relative.name != entry["path"]:
            raise QualificationError("corpus path is unsafe")
        if len(entry["sha256"]) != 64:
            raise QualificationError("corpus digest is invalid")
        expected_paths.append(entry["path"])
        actual = sha256_file(CORPUS_ROOT / entry["path"])
        if actual != entry["sha256"]:
            raise QualificationError(f"corpus digest drifted: {entry['path']}")
    actual_paths = sorted(path.name for path in CORPUS_ROOT.iterdir() if path.is_file())
    if actual_paths != sorted(expected_paths):
        raise QualificationError("corpus file set changed")

    queries = manifest["queries"]
    if not isinstance(queries, list) or len(queries) != 5:
        raise QualificationError("query inventory changed")
    query_ids: set[str] = set()
    for query in queries:
        if set(query) != {"id", "query"}:
            raise QualificationError("query fields changed")
        if not query["id"] or not query["query"] or query["id"] in query_ids:
            raise QualificationError("query identity is invalid")
        query_ids.add(query["id"])
    return manifest


def manifest_sha256() -> str:
    return sha256_file(MANIFEST_PATH)


def corpus_fingerprint(manifest: dict[str, Any]) -> str:
    digest = hashlib.sha256()
    for entry in manifest["corpus"]:
        digest.update(entry["path"].encode("utf-8"))
        digest.update(b"\0")
        digest.update(bytes.fromhex(entry["sha256"]))
    return digest.hexdigest()


def require_absolute(path: pathlib.Path, label: str) -> pathlib.Path:
    if not path.is_absolute():
        raise QualificationError(f"{label} must be absolute")
    return path


def run_command(
    arguments: list[str],
    *,
    cwd: pathlib.Path,
    environment: dict[str, str],
) -> subprocess.CompletedProcess[str]:
    try:
        result = subprocess.run(
            arguments,
            cwd=cwd,
            env=environment,
            capture_output=True,
            text=True,
            timeout=60,
        )
    except subprocess.TimeoutExpired as error:
        raise QualificationError(f"command timed out: {' '.join(arguments)}") from error
    if result.returncode != 0:
        raise QualificationError(
            f"command failed ({result.returncode}): {' '.join(arguments)}\n"
            f"stdout: {result.stdout[:4096]}\nstderr: {result.stderr[:4096]}"
        )
    return result


def materialize_repository(destination: pathlib.Path) -> None:
    manifest = load_manifest()
    destination = require_absolute(destination, "repository")
    if destination != pathlib.Path(manifest["fixture_repository"]):
        raise QualificationError("repository must use the frozen canonical path")
    if not destination.is_dir():
        raise QualificationError("workflow must create the private fixture directory")
    if any(destination.iterdir()):
        raise QualificationError("fixture repository must start empty")
    destination.chmod(0o700)

    for entry in manifest["corpus"]:
        target = destination / entry["path"]
        shutil.copyfile(CORPUS_ROOT / entry["path"], target)
        target.chmod(0o600)
        timestamp = manifest["fixed_mtime_unix"]
        os.utime(target, (timestamp, timestamp))

    environment = os.environ.copy()
    environment.update(
        {
            "GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z",
            "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z",
        }
    )
    run_command(["git", "init", "--quiet"], cwd=destination, environment=environment)
    run_command(
        ["git", "config", "user.email", "determinism@hsum.invalid"],
        cwd=destination,
        environment=environment,
    )
    run_command(
        ["git", "config", "user.name", "hSUM determinism"],
        cwd=destination,
        environment=environment,
    )
    run_command(["git", "add", "."], cwd=destination, environment=environment)
    run_command(
        ["git", "commit", "--quiet", "-m", "frozen retrieval corpus"],
        cwd=destination,
        environment=environment,
    )


def selected_environment(home: pathlib.Path) -> dict[str, str]:
    environment = os.environ.copy()
    environment.update(
        {
            "HSUM_HOME": str(home),
            "HSUM_INDEX": INDEX_NAME,
            "HSUM_PROJECT": PROJECT_NAME,
            "HSUM_OFFLINE": "1",
        }
    )
    return environment


def checkpoint_closed_index(index: pathlib.Path) -> None:
    try:
        connection = sqlite3.connect(index, timeout=10)
        try:
            checkpoint = connection.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()
        finally:
            connection.close()
    except sqlite3.Error as error:
        raise QualificationError(f"fixture checkpoint failed: {error}") from error
    if checkpoint != (0, 0, 0):
        raise QualificationError(f"fixture checkpoint remained busy: {checkpoint}")


def prepare_fixture(
    binary: pathlib.Path,
    home: pathlib.Path,
    repository: pathlib.Path,
    output: pathlib.Path,
) -> None:
    manifest = load_manifest()
    binary = require_absolute(binary, "hsum binary")
    home = require_absolute(home, "hsum home")
    output = require_absolute(output, "fixture report")
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise QualificationError("hsum binary is not executable")
    if home.exists() or output.exists():
        raise QualificationError("fixture outputs must not exist")
    materialize_repository(repository)
    environment = os.environ.copy()
    environment.update({"HSUM_HOME": str(home), "HSUM_OFFLINE": "1"})
    environment.pop("HSUM_INDEX", None)
    environment.pop("HSUM_PROJECT", None)
    common = [str(binary), "--no-color", "--no-progress"]
    run_command(
        common
        + [
            "init",
            ".",
            "--index",
            INDEX_NAME,
            "--project",
            PROJECT_NAME,
        ],
        cwd=repository,
        environment=environment,
    )
    run_command(common + ["doctor"], cwd=repository, environment=environment)

    index = home / "data" / "indexes" / INDEX_NAME / "index.sqlite"
    if not index.is_file():
        raise QualificationError("prepared fixture index is missing")
    checkpoint_closed_index(index)
    for suffix in ("-wal", "-shm", "-journal"):
        if pathlib.Path(f"{index}{suffix}").exists():
            raise QualificationError(f"prepared fixture retained SQLite sidecar {suffix}")
    report = {
        "schema_version": "hsum.retrieval-determinism-fixture.v1",
        "manifest_sha256": manifest_sha256(),
        "corpus_fingerprint_sha256": corpus_fingerprint(manifest),
        "index_sha256": sha256_file(index),
        "binary_sha256": sha256_file(binary),
        "repository": str(repository),
        "index_relative_path": str(index.relative_to(home)),
    }
    write_new_json(output, report)


def normalize_score(score: Any, transport: str) -> Any:
    if score is None:
        return None
    list_name = "name" if transport == "cli" else "retriever"
    return {
        "fusion_units": score["fusion_units"],
        "fused": score["fused"],
        "lists": [
            {
                "retriever": item[list_name],
                "rank": item["rank"],
                "contribution_units": item["contribution_units"],
                "backend_score": item["backend_score"],
            }
            for item in score["lists"]
        ],
    }


def normalize_packet(packet: dict[str, Any], transport: str) -> dict[str, Any]:
    if packet["requested_mode"] != "lexical" or packet["effective_mode"] != "lexical":
        raise QualificationError("lexical qualification changed retrieval mode")
    if packet["timing_ms"]["query_embedding"] != 0:
        raise QualificationError("lexical qualification invoked query embedding")
    if not packet["results"]:
        raise QualificationError("frozen query returned no evidence")
    results = []
    for result in packet["results"]:
        span = result["span"] if transport == "cli" else result["byte_span"]
        start = span["start_byte"] if transport == "cli" else span["start"]
        end = span["end_byte"] if transport == "cli" else span["end"]
        line_span = result["span"] if transport == "cli" else result["line_span"]
        start_line = (
            line_span["start_line"] if transport == "cli" else line_span["start"]
        )
        end_line = line_span["end_line"] if transport == "cli" else line_span["end"]
        duplicates = sorted(
            (
                {
                    "citation_uri": duplicate["citation_uri"],
                    "reason": duplicate["reason"],
                }
                for duplicate in result["duplicate_citations"]
            ),
            key=lambda item: (item["citation_uri"], item["reason"]),
        )
        results.append(
            {
                "citation_uri": result["citation_uri"],
                "index_id": result["index_id"],
                "source_id": result["source_id"],
                "document_id": result["document_id"],
                "revision_sha256": result["revision_sha256"],
                "source_uri": result["source_uri"],
                "title": result["title"],
                "start_byte": start,
                "end_byte": end,
                "start_line": start_line,
                "end_line": end_line,
                "content": result["content"],
                "content_sha256": result["content_sha256"],
                "source_updated_at": result["source_updated_at"],
                "indexed_at": result["indexed_at"],
                "head_generation": result["head_generation"],
                "source_state": result["source_state"],
                "untrusted_content": result["untrusted_content"],
                "duplicate_citations": duplicates,
                "score": normalize_score(result["score"], transport),
            }
        )
    return {
        "schema_version": packet["schema_version"],
        "generation": packet["generation"],
        "index_epoch": packet["index_epoch"],
        "project_id": packet["project_id"],
        "scope_revision": packet["scope_revision"],
        "requested_mode": packet["requested_mode"],
        "effective_mode": packet["effective_mode"],
        "retrievers": packet["retrievers"],
        "degraded_mode": packet["degraded_mode"],
        "hints": packet["hints"],
        "stop_reason": packet["stop_reason"],
        "next_cursor": packet["next_cursor"],
        "examined": packet["examined"],
        "results": results,
    }


def run_cli_query(
    binary: pathlib.Path,
    repository: pathlib.Path,
    environment: dict[str, str],
    manifest: dict[str, Any],
    query: dict[str, str],
) -> dict[str, Any]:
    result = run_command(
        [
            str(binary),
            "--no-color",
            "--no-progress",
            "search",
            query["query"],
            "--mode",
            "lexical",
            "--limit",
            str(manifest["limit"]),
            "--timeout-ms",
            str(manifest["timeout_ms"]),
            "--explain",
            "--json",
        ],
        cwd=repository,
        environment=environment,
    )
    if result.stderr:
        raise QualificationError("successful CLI query wrote stderr")
    return normalize_packet(json.loads(result.stdout), "cli")


def send_frame(process: subprocess.Popen[str], value: dict[str, Any]) -> None:
    assert process.stdin is not None
    process.stdin.write(json.dumps(value, separators=(",", ":")) + "\n")
    process.stdin.flush()


def receive_frame(process: subprocess.Popen[str], timeout: float = 15) -> dict[str, Any]:
    assert process.stdout is not None
    readable, _, _ = select.select([process.stdout], [], [], timeout)
    if not readable:
        raise QualificationError(f"MCP response timed out with status {process.poll()}")
    line = process.stdout.readline()
    if not line:
        raise QualificationError(f"MCP process disconnected with status {process.poll()}")
    return json.loads(line)


def run_mcp_queries(
    binary: pathlib.Path,
    repository: pathlib.Path,
    environment: dict[str, str],
    manifest: dict[str, Any],
) -> dict[str, dict[str, Any]]:
    process = subprocess.Popen(
        [str(binary), "--no-color", "--no-progress", "mcp"],
        cwd=repository,
        env=environment,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        send_frame(
            process,
            {
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "retrieval-determinism", "version": "1"},
                },
            },
        )
        initialized = receive_frame(process)
        if initialized.get("id") != 1 or initialized.get("result", {}).get(
            "serverInfo", {}
        ).get("name") != "hsum":
            raise QualificationError("MCP initialization failed")
        send_frame(
            process,
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
        )

        packets: dict[str, dict[str, Any]] = {}
        for request_id, query in enumerate(manifest["queries"], start=2):
            send_frame(
                process,
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "method": "tools/call",
                    "params": {
                        "name": "evidence_search",
                        "arguments": {
                            "query": query["query"],
                            "mode": "lexical",
                            "limit": manifest["limit"],
                            "timeout_ms": manifest["timeout_ms"],
                            "explain": True,
                        },
                    },
                },
            )
            response = receive_frame(process)
            if response.get("id") != request_id or "error" in response:
                raise QualificationError(f"MCP query failed: {response}")
            packets[query["id"]] = normalize_packet(
                response["result"]["structuredContent"], "mcp"
            )

        assert process.stdin is not None
        process.stdin.close()
        if process.wait(timeout=10) != 0:
            raise QualificationError("MCP process exited unsuccessfully")
        assert process.stderr is not None
        if process.stderr.read():
            raise QualificationError("successful MCP session wrote stderr")
        return packets
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)


def require_equal_runs(runs: list[dict[str, Any]], label: str) -> dict[str, Any]:
    if not runs:
        raise QualificationError(f"{label} produced no runs")
    baseline = runs[0]
    for ordinal, run in enumerate(runs[1:], start=2):
        if run != baseline:
            raise QualificationError(f"{label} run {ordinal} changed ordering or evidence")
    return baseline


def platform_identity() -> dict[str, str]:
    system = platform.system().lower()
    if system == "darwin":
        system = "macos"
    return {"os": system, "arch": platform.machine()}


def run_qualification(
    binary: pathlib.Path,
    home: pathlib.Path,
    repository: pathlib.Path,
    fixture_report: pathlib.Path,
    output: pathlib.Path,
) -> None:
    manifest = load_manifest()
    binary = require_absolute(binary, "hsum binary")
    home = require_absolute(home, "hsum home")
    repository = require_absolute(repository, "repository")
    fixture_report = require_absolute(fixture_report, "fixture report")
    output = require_absolute(output, "report output")
    if output.exists():
        raise QualificationError("report output already exists")
    if repository != pathlib.Path(manifest["fixture_repository"]):
        raise QualificationError("repository path differs from the frozen fixture")
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise QualificationError("hsum binary is not executable")
    index = home / "data" / "indexes" / INDEX_NAME / "index.sqlite"
    if not index.is_file():
        raise QualificationError("portable fixture index is missing")
    fixture = json.loads(fixture_report.read_text(encoding="utf-8"))
    expected_fixture_fields = {
        "schema_version",
        "manifest_sha256",
        "corpus_fingerprint_sha256",
        "index_sha256",
        "binary_sha256",
        "repository",
        "index_relative_path",
    }
    if set(fixture) != expected_fixture_fields:
        raise QualificationError("portable fixture receipt fields changed")
    if fixture.get("schema_version") != "hsum.retrieval-determinism-fixture.v1":
        raise QualificationError("portable fixture receipt is invalid")
    if fixture.get("manifest_sha256") != manifest_sha256():
        raise QualificationError("portable fixture manifest differs")
    if fixture.get("corpus_fingerprint_sha256") != corpus_fingerprint(manifest):
        raise QualificationError("portable fixture corpus differs")
    if fixture.get("index_sha256") != sha256_file(index):
        raise QualificationError("portable fixture index digest differs")
    if fixture.get("repository") != str(repository):
        raise QualificationError("portable fixture repository differs")
    if fixture.get("index_relative_path") != index.relative_to(home).as_posix():
        raise QualificationError("portable fixture index path differs")
    for suffix in ("-wal", "-shm", "-journal"):
        if pathlib.Path(f"{index}{suffix}").exists():
            raise QualificationError(f"portable fixture retained SQLite sidecar {suffix}")
    for entry in manifest["corpus"]:
        if sha256_file(repository / entry["path"]) != entry["sha256"]:
            raise QualificationError(f"materialized corpus drifted: {entry['path']}")

    environment = selected_environment(home)
    cli_runs = []
    mcp_runs = []
    for _ in range(manifest["runs_per_transport"]):
        cli_runs.append(
            {
                query["id"]: run_cli_query(
                    binary, repository, environment, manifest, query
                )
                for query in manifest["queries"]
            }
        )
        mcp_runs.append(
            run_mcp_queries(binary, repository, environment, manifest)
        )
    cli = require_equal_runs(cli_runs, "CLI")
    mcp = require_equal_runs(mcp_runs, "MCP")
    if cli != mcp:
        raise QualificationError("CLI and MCP normalized evidence differ")
    duplicate_set_exercised = any(
        result["duplicate_citations"]
        for packet in cli.values()
        for result in packet["results"]
    )
    if not duplicate_set_exercised:
        raise QualificationError("fixture did not exercise duplicate citations")
    explanation_membership_exercised = any(
        result["score"] is not None and len(result["score"]["lists"]) > 1
        for packet in cli.values()
        for result in packet["results"]
    )
    if not explanation_membership_exercised:
        raise QualificationError("fixture did not exercise multiple explanation lists")
    equal_backend_tie_exercised = any(
        has_equal_backend_score(packet) for packet in cli.values()
    )
    if not equal_backend_tie_exercised:
        raise QualificationError("fixture did not exercise an equal backend-score tie")
    if sha256_file(index) != fixture["index_sha256"]:
        raise QualificationError("qualification mutated the frozen index")

    version = run_command(
        [str(binary), "--version"], cwd=repository, environment=environment
    ).stdout.strip()
    report = {
        "schema_version": SCHEMA_VERSION,
        "passed": True,
        "commit_sha": os.environ.get("GITHUB_SHA", "unknown"),
        "platform": platform_identity(),
        "manifest_sha256": manifest_sha256(),
        "corpus_fingerprint_sha256": corpus_fingerprint(manifest),
        "index_sha256": sha256_file(index),
        "binary_sha256": sha256_file(binary),
        "binary_version": version,
        "runs_per_transport": manifest["runs_per_transport"],
        "queries": [query["id"] for query in manifest["queries"]],
        "checks": {
            "cli_fresh_processes_identical": True,
            "mcp_fresh_processes_identical": True,
            "cli_mcp_equivalent": True,
            "lexical_query_embedding_absent": True,
            "duplicate_set_exercised": duplicate_set_exercised,
            "multiple_explanation_lists_exercised": explanation_membership_exercised,
            "equal_backend_score_tie_exercised": equal_backend_tie_exercised,
        },
        "observations": cli,
    }
    write_new_json(output, report)


def has_equal_backend_score(packet: dict[str, Any]) -> bool:
    scores: dict[str, list[float]] = {}
    for result in packet["results"]:
        if result["score"] is None:
            continue
        for explanation in result["score"]["lists"]:
            backend = explanation["backend_score"]
            if backend is not None:
                scores.setdefault(explanation["retriever"], []).append(backend)
    return any(
        any(left == right for index, left in enumerate(values) for right in values[index + 1 :])
        for values in scores.values()
    )


def compare_values(left: Any, right: Any, path: str = "root") -> float:
    if type(left) is not type(right):
        raise QualificationError(f"cross-target type mismatch at {path}")
    if isinstance(left, bool) or isinstance(right, bool):
        if left != right:
            raise QualificationError(f"cross-target mismatch at {path}")
        return 0.0
    if isinstance(left, float):
        if not math.isfinite(left) or not math.isfinite(right):
            raise QualificationError(f"non-finite cross-target score at {path}")
        if not path.endswith(".backend_score"):
            if left != right:
                raise QualificationError(f"cross-target mismatch at {path}")
            return 0.0
        delta = abs(float(left) - float(right))
        if delta > FLOAT_TOLERANCE:
            raise QualificationError(f"numeric cross-target drift at {path}: {delta}")
        return delta
    if isinstance(left, dict):
        if set(left) != set(right):
            raise QualificationError(f"cross-target fields differ at {path}")
        return max(
            (compare_values(left[key], right[key], f"{path}.{key}") for key in left),
            default=0.0,
        )
    if isinstance(left, list):
        if len(left) != len(right):
            raise QualificationError(f"cross-target length differs at {path}")
        return max(
            (
                compare_values(left_item, right_item, f"{path}[{index}]")
                for index, (left_item, right_item) in enumerate(zip(left, right))
            ),
            default=0.0,
        )
    if left != right:
        raise QualificationError(f"cross-target mismatch at {path}")
    return 0.0


def compare_reports(
    linux_path: pathlib.Path,
    macos_path: pathlib.Path,
    output: pathlib.Path,
) -> None:
    reports = [load_report(linux_path), load_report(macos_path)]
    linux, macos = reports
    if {(report["platform"]["os"], report["platform"]["arch"]) for report in reports} != {
        ("linux", "x86_64"),
        ("macos", "arm64"),
    }:
        raise QualificationError("native platform set is incomplete")
    for key in (
        "commit_sha",
        "manifest_sha256",
        "corpus_fingerprint_sha256",
        "index_sha256",
        "binary_version",
        "runs_per_transport",
        "queries",
        "checks",
    ):
        if linux[key] != macos[key]:
            raise QualificationError(f"cross-target report identity differs: {key}")
    maximum_numeric_delta = compare_values(
        linux["observations"], macos["observations"], "observations"
    )
    summary = {
        "schema_version": COMPARISON_SCHEMA_VERSION,
        "passed": True,
        "commit_sha": linux["commit_sha"],
        "manifest_sha256": linux["manifest_sha256"],
        "index_sha256": linux["index_sha256"],
        "runs_per_transport": linux["runs_per_transport"],
        "query_count": len(linux["queries"]),
        "platforms": [linux["platform"], macos["platform"]],
        "lexical_citation_order_identical": True,
        "duplicate_sets_identical": True,
        "degradation_and_explanations_identical": True,
        "maximum_numeric_delta": maximum_numeric_delta,
        "numeric_tolerance": FLOAT_TOLERANCE,
        "report_sha256": {
            "linux_x86_64": sha256_file(linux_path),
            "macos_arm64": sha256_file(macos_path),
        },
    }
    write_new_json(output, summary)


def load_report(path: pathlib.Path) -> dict[str, Any]:
    report = json.loads(path.read_text(encoding="utf-8"))
    if set(report) != REPORT_FIELDS:
        raise QualificationError(f"qualification report fields changed: {path}")
    if report.get("schema_version") != SCHEMA_VERSION or report.get("passed") is not True:
        raise QualificationError(f"invalid qualification report: {path}")
    if set(report["checks"]) != CHECK_FIELDS or not all(report["checks"].values()):
        raise QualificationError(f"qualification report checks are incomplete: {path}")
    return report


def write_new_json(path: pathlib.Path, value: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x", encoding="utf-8") as output:
        json.dump(value, output, sort_keys=True, separators=(",", ":"))
        output.write("\n")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("validate")

    materialize = commands.add_parser("materialize")
    materialize.add_argument("--repository", type=pathlib.Path, required=True)

    prepare = commands.add_parser("prepare")
    prepare.add_argument("--hsum", type=pathlib.Path, required=True)
    prepare.add_argument("--home", type=pathlib.Path, required=True)
    prepare.add_argument("--repository", type=pathlib.Path, required=True)
    prepare.add_argument("--output", type=pathlib.Path, required=True)

    run = commands.add_parser("run")
    run.add_argument("--hsum", type=pathlib.Path, required=True)
    run.add_argument("--home", type=pathlib.Path, required=True)
    run.add_argument("--repository", type=pathlib.Path, required=True)
    run.add_argument("--fixture-report", type=pathlib.Path, required=True)
    run.add_argument("--output", type=pathlib.Path, required=True)

    compare = commands.add_parser("compare")
    compare.add_argument("linux", type=pathlib.Path)
    compare.add_argument("macos", type=pathlib.Path)
    compare.add_argument("--output", type=pathlib.Path, required=True)
    return parser.parse_args()


def main() -> int:
    try:
        arguments = parse_args()
        if arguments.command == "validate":
            manifest = load_manifest()
            print(
                json.dumps(
                    {
                        "manifest_sha256": manifest_sha256(),
                        "corpus_fingerprint_sha256": corpus_fingerprint(manifest),
                        "queries": len(manifest["queries"]),
                        "runs_per_transport": manifest["runs_per_transport"],
                    },
                    sort_keys=True,
                )
            )
        elif arguments.command == "materialize":
            materialize_repository(arguments.repository)
        elif arguments.command == "prepare":
            prepare_fixture(
                arguments.hsum.resolve(),
                arguments.home.resolve(),
                arguments.repository.resolve(),
                arguments.output.resolve(),
            )
        elif arguments.command == "run":
            run_qualification(
                arguments.hsum.resolve(),
                arguments.home.resolve(),
                arguments.repository.resolve(),
                arguments.fixture_report.resolve(),
                arguments.output.resolve(),
            )
        elif arguments.command == "compare":
            compare_reports(
                arguments.linux.resolve(),
                arguments.macos.resolve(),
                arguments.output.resolve(),
            )
        else:  # pragma: no cover - argparse owns the command inventory.
            raise QualificationError("unknown command")
        return 0
    except (OSError, ValueError, KeyError, TypeError, QualificationError) as error:
        print(f"retrieval determinism qualification failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
