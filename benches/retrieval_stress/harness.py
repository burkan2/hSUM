#!/usr/bin/env python3
"""Run the frozen one-million-passage hSUM report-only stress protocol."""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import math
import os
import pathlib
import select
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
from typing import Any

REPOSITORY_ROOT = pathlib.Path(__file__).resolve().parents[2]
if str(REPOSITORY_ROOT) not in sys.path:
    sys.path.insert(0, str(REPOSITORY_ROOT))

from benches.retrieval_scale import harness as scale


ROOT = pathlib.Path(__file__).resolve().parent
MANIFEST_PATH = ROOT / "manifest.json"
SCHEMA_VERSION = "hsum.retrieval-stress-report.v1"
INDEX_NAME = "retrieval-stress-1m"
PROJECT_NAME = "default"
CANDIDATE_BUDGET_PER_RETRIEVER = 500
QUERY_TIMEOUT_MS = 10_000
BODY_FILLER = (
    " StressPostingLiteral stress::punctuation|quoted[slot]=>value"
    " duplicate bounded evidence archive "
)
TIMING_STAGES = scale.STAGES


class StressError(RuntimeError):
    """One frozen stress invariant failed."""


def sha256_file(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while block := source.read(1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def shared_body(body_bytes: int = 15_000) -> bytes:
    body = (BODY_FILLER * ((body_bytes // len(BODY_FILLER)) + 2))[:body_bytes]
    encoded = body.encode("ascii")
    if len(encoded) != body_bytes:
        raise StressError("shared body byte count drifted")
    return encoded


def load_manifest() -> dict[str, Any]:
    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    expected = {
        "schema_version",
        "model_id",
        "source_count",
        "jsonl_source_count",
        "document_count",
        "body_bytes",
        "body_sha256",
        "chunks_per_document",
        "expected_passages",
        "scale_source_points",
        "metadata_only_generations",
        "concurrent_old_readers",
        "probe_repetitions",
        "candidate_budget_per_retriever",
        "cancelled_requests",
        "knee_ratio_exclusive",
        "queries",
    }
    if set(manifest) != expected:
        raise StressError("stress manifest fields changed")
    if manifest["schema_version"] != "hsum.retrieval-stress-manifest.v1":
        raise StressError("unsupported stress manifest schema")
    fixed = {
        "source_count": 64,
        "jsonl_source_count": 63,
        "document_count": 100_000,
        "body_bytes": 15_000,
        "chunks_per_document": 10,
        "expected_passages": 1_000_000,
        "scale_source_points": [2, 8, 16, 32, 64],
        "metadata_only_generations": 100,
        "concurrent_old_readers": 4,
        "probe_repetitions": 5,
        "candidate_budget_per_retriever": CANDIDATE_BUDGET_PER_RETRIEVER,
        "cancelled_requests": 8,
        "knee_ratio_exclusive": 2.0,
    }
    for key, value in fixed.items():
        if manifest[key] != value:
            raise StressError(f"frozen stress field changed: {key}")
    body = shared_body(manifest["body_bytes"])
    if hashlib.sha256(body).hexdigest() != manifest["body_sha256"]:
        raise StressError("shared body digest drifted")
    if scale.unbroken_chunk_count(len(body)) != manifest["chunks_per_document"]:
        raise StressError("shared body chunk count drifted")
    if manifest["source_count"] != manifest["jsonl_source_count"] + 1:
        raise StressError("source inventory must include one filesystem authority")
    if (
        manifest["document_count"] * manifest["chunks_per_document"]
        != manifest["expected_passages"]
    ):
        raise StressError("document/passage cardinality drifted")
    queries = manifest["queries"]
    if not isinstance(queries, list) or len(queries) != 4:
        raise StressError("stress query inventory changed")
    expected_query_fields = {"id", "mode", "query"}
    identifiers: set[str] = set()
    for query in queries:
        if not isinstance(query, dict) or set(query) != expected_query_fields:
            raise StressError("stress query fields changed")
        if not all(isinstance(query[field], str) and query[field] for field in query):
            raise StressError("stress query contains an empty field")
        if query["id"] in identifiers or query["mode"] not in {"lexical", "hybrid"}:
            raise StressError("stress query identity or mode is invalid")
        identifiers.add(query["id"])
    if queries[-1]["mode"] != "hybrid" or any(
        query["mode"] != "lexical" for query in queries[:-1]
    ):
        raise StressError("stress query mode order changed")
    return manifest


def manifest_sha256() -> str:
    return sha256_file(MANIFEST_PATH)


def corpus_fingerprint(manifest: dict[str, Any]) -> str:
    contract = {
        "body_sha256": manifest["body_sha256"],
        "body_bytes": manifest["body_bytes"],
        "documents": manifest["document_count"],
        "jsonl_sources": manifest["jsonl_source_count"],
        "chunks_per_document": manifest["chunks_per_document"],
    }
    return hashlib.sha256(
        json.dumps(contract, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()


def documents_per_source(manifest: dict[str, Any]) -> list[int]:
    count, sources = manifest["document_count"], manifest["jsonl_source_count"]
    base, remainder = divmod(count, sources)
    distribution = [base + (1 if index < remainder else 0) for index in range(sources)]
    if sum(distribution) != count or max(distribution) - min(distribution) > 1:
        raise StressError("document distribution is not balanced")
    return distribution


def source_name(index: int) -> str:
    return f"stress-{index:02d}"


def record_line(
    source_index: int,
    document_index: int,
    body_json: str,
    metadata_generation: int = 0,
) -> bytes:
    value = (
        f'{{"content":{body_json},"id":"document-{source_index:02d}-{document_index:05d}",'
        f'"metadata":{{"generation":"{metadata_generation:03d}"}},'
        f'"source_uri":"stress://source-{source_index:02d}/document-{document_index:05d}",'
        f'"title":"stress source {source_index:02d} document {document_index:05d}"}}\n'
    )
    return value.encode("utf-8")


def create_snapshots(
    root: pathlib.Path, manifest: dict[str, Any]
) -> tuple[list[pathlib.Path], dict[str, Any]]:
    if root.exists() and any(root.iterdir()):
        raise StressError("snapshot directory must start empty")
    root.mkdir(parents=True, exist_ok=True)
    body_json = json.dumps(shared_body(manifest["body_bytes"]).decode("ascii"))
    distribution = documents_per_source(manifest)
    digest = hashlib.sha256()
    total_bytes = 0
    paths: list[pathlib.Path] = []
    started = time.perf_counter()
    for source_index, document_count in enumerate(distribution):
        path = root / f"{source_name(source_index)}.jsonl"
        with path.open("xb", buffering=1024 * 1024) as output:
            for document_index in range(document_count):
                line = record_line(source_index, document_index, body_json)
                output.write(line)
                digest.update(line)
                total_bytes += len(line)
        paths.append(path)
    return paths, {
        "generation_seconds": time.perf_counter() - started,
        "encoded_snapshot_bytes": total_bytes,
        "stream_sha256": digest.hexdigest(),
        "documents_per_source": distribution,
    }


def command_environment(home: pathlib.Path) -> dict[str, str]:
    return scale.command_environment(home)


def run_command(
    arguments: list[str], *, cwd: pathlib.Path, home: pathlib.Path
) -> subprocess.CompletedProcess[str]:
    return scale.run_command(arguments, cwd=cwd, home=home)


def run_json(
    arguments: list[str], *, cwd: pathlib.Path, home: pathlib.Path
) -> dict[str, Any]:
    return scale.run_json_command(arguments, cwd=cwd, home=home)


def run_monitored(
    arguments: list[str], *, cwd: pathlib.Path, home: pathlib.Path
) -> dict[str, Any]:
    with tempfile.TemporaryFile(mode="w+", encoding="utf-8") as stdout:
        with tempfile.TemporaryFile(mode="w+", encoding="utf-8") as stderr:
            started = time.perf_counter()
            process = subprocess.Popen(
                arguments,
                cwd=cwd,
                env=command_environment(home),
                text=True,
                stdout=stdout,
                stderr=stderr,
            )
            sampler = scale.RssSampler(process.pid)
            sampler.start()
            returncode = process.wait()
            elapsed = time.perf_counter() - started
            sampler.stop()
            stdout.seek(0)
            stderr.seek(0)
            stdout_tail = stdout.read()[-4000:]
            stderr_tail = stderr.read()[-4000:]
    if returncode != 0:
        raise StressError(
            f"command failed ({returncode}): {' '.join(arguments)}\n"
            f"stdout:\n{stdout_tail}\nstderr:\n{stderr_tail}"
        )
    if sampler.error is not None or sampler.peak_kib <= 0:
        raise StressError(f"RSS sampling failed: {sampler.error}")
    return {
        "seconds": elapsed,
        "peak_process_tree_rss_bytes": sampler.peak_kib * 1024,
        "stdout_tail": stdout_tail,
    }


def status_packet(binary: pathlib.Path, root: pathlib.Path, home: pathlib.Path) -> dict[str, Any]:
    return run_json([str(binary), "status", "--json"], cwd=root, home=home)


def validate_status(
    packet: dict[str, Any], *, sources: int, documents: int, passages: int
) -> None:
    if len(packet.get("sources", [])) != sources:
        raise StressError(f"expected {sources} active sources")
    if packet.get("active_documents") != documents:
        raise StressError(f"expected {documents} active documents")
    if packet.get("active_passages") != passages:
        raise StressError(f"expected {passages} active passages")
    if packet.get("problems"):
        raise StressError(f"stress index reported problems: {packet['problems']}")


def validate_search_packet(query: dict[str, str], packet: dict[str, Any]) -> None:
    if packet.get("requested_mode") != query["mode"]:
        raise StressError(f"query {query['id']} requested mode drifted")
    if packet.get("effective_mode") != query["mode"] or packet.get("degraded_mode"):
        raise StressError(f"query {query['id']} degraded or changed mode")
    if not isinstance(packet.get("results"), list) or not packet["results"]:
        raise StressError(f"query {query['id']} returned no evidence")
    examined = packet.get("examined")
    candidate_keys = {"exact", "exact_fallback", "lexical", "vector"}
    if not isinstance(examined, dict) or set(examined) != candidate_keys:
        raise StressError(f"query {query['id']} omitted candidate counts")
    if any(
        not isinstance(examined[key], int)
        or isinstance(examined[key], bool)
        or not 0 <= examined[key] <= CANDIDATE_BUDGET_PER_RETRIEVER
        for key in candidate_keys
    ):
        raise StressError(f"query {query['id']} exceeded the candidate budget")
    if not isinstance(packet.get("timing_ms"), dict) or any(
        not isinstance(packet["timing_ms"].get(stage), (int, float))
        for stage in TIMING_STAGES
    ):
        raise StressError(f"query {query['id']} omitted timing stages")
    retrievers = packet.get("retrievers")
    if not isinstance(retrievers, list):
        raise StressError(f"query {query['id']} omitted retrievers")
    if "exact" not in retrievers or "lexical" not in retrievers:
        raise StressError(f"query {query['id']} omitted lexical retrieval paths")
    if query["id"] == "punctuation-exact-fallback" and "exact_fallback" not in retrievers:
        raise StressError("punctuation query did not exercise exact fallback")
    if query["mode"] == "hybrid" and "vector" not in retrievers:
        raise StressError("hybrid query did not exercise vector retrieval")
    if query["mode"] == "hybrid" and "exact_fallback" in retrievers:
        raise StressError("hybrid query unexpectedly exercised exact fallback")
    if packet.get("stop_reason") not in {
        "limit_reached",
        "unique_exhausted",
        "work_budget_exhausted",
        "deadline",
    }:
        raise StressError(f"query {query['id']} omitted an explicit stop reason")


def probe_query(
    binary: pathlib.Path,
    root: pathlib.Path,
    home: pathlib.Path,
    query: dict[str, str],
) -> dict[str, Any]:
    started = time.perf_counter()
    packet = run_json(
        [
            str(binary),
            "--no-color",
            "--no-progress",
            "search",
            query["query"],
            "--mode",
            query["mode"],
            "--limit",
            "10",
            "--timeout-ms",
            str(QUERY_TIMEOUT_MS),
            "--json",
        ],
        cwd=root,
        home=home,
    )
    client_ms = (time.perf_counter() - started) * 1000.0
    validate_search_packet(query, packet)
    return {
        "client_round_trip_ms": client_ms,
        "timing_ms": {stage: packet["timing_ms"][stage] for stage in TIMING_STAGES},
        "examined": packet["examined"],
        "retrievers": packet["retrievers"],
        "stop_reason": packet.get("stop_reason"),
        "result_count": len(packet["results"]),
    }


def summarize_probes(observations: list[dict[str, Any]]) -> dict[str, Any]:
    return {
        "observations": observations,
        "client_round_trip_ms": scale.distribution(
            item["client_round_trip_ms"] for item in observations
        ),
        "timing_ms": {
            stage: scale.distribution(item["timing_ms"][stage] for item in observations)
            for stage in TIMING_STAGES
        },
        "max_examined": {
            key: max(item["examined"].get(key, 0) for item in observations)
            for key in ("exact", "exact_fallback", "lexical", "vector")
        },
    }


def measure_scale_point(
    binary: pathlib.Path,
    root: pathlib.Path,
    home: pathlib.Path,
    manifest: dict[str, Any],
    source_count: int,
    documents: int,
) -> dict[str, Any]:
    status = status_packet(binary, root, home)
    passages = documents * manifest["chunks_per_document"]
    validate_status(status, sources=source_count, documents=documents, passages=passages)
    queries = {}
    for query in manifest["queries"][:-1]:
        observations = [
            probe_query(binary, root, home, query)
            for _ in range(manifest["probe_repetitions"])
        ]
        queries[query["id"]] = summarize_probes(observations)
    storage = status.get("storage") if isinstance(status.get("storage"), dict) else {}
    return {
        "source_count": source_count,
        "documents": documents,
        "passages": passages,
        "active_generation": status.get("active_generation"),
        "managed_index_bytes": storage.get("managed_index_bytes"),
        "queries": queries,
    }


def first_observed_knee(
    samples: list[dict[str, Any]], ratio_exclusive: float
) -> dict[str, Any] | None:
    for previous, current in zip(samples, samples[1:]):
        for query_id in previous["queries"]:
            before = previous["queries"][query_id]["client_round_trip_ms"]["p50"]
            after = current["queries"][query_id]["client_round_trip_ms"]["p50"]
            ratio = math.inf if before == 0 and after > 0 else (after / before if before else 1.0)
            if ratio > ratio_exclusive:
                return {
                    "query_id": query_id,
                    "source_count": current["source_count"],
                    "passages": current["passages"],
                    "previous_p50_ms": before,
                    "observed_p50_ms": after,
                    "ratio": ratio,
                    "rule": "current p50 is more than 2x the preceding scale point",
                }
    return None


def update_sentinel_generation(
    snapshot: pathlib.Path, body_json: str, generation: int
) -> None:
    replacement = record_line(0, 0, body_json, generation)
    with snapshot.open("r+b") as target:
        original = target.readline()
        if len(original) != len(replacement):
            raise StressError("metadata generation line length changed")
        target.seek(0)
        target.write(replacement)
        target.flush()
        os.fsync(target.fileno())


def active_generation(connection: sqlite3.Connection) -> int:
    value = connection.execute(
        "SELECT value FROM index_meta WHERE key = 'active_generation'"
    ).fetchone()[0]
    if isinstance(value, bytes):
        value = value.decode("ascii")
    return int(value)


def summarize_operations(operations: list[dict[str, Any]]) -> dict[str, Any]:
    return {
        "count": len(operations),
        "total_seconds": sum(item["seconds"] for item in operations),
        "seconds": scale.distribution(item["seconds"] for item in operations),
        "maximum_peak_process_tree_rss_bytes": max(
            item["peak_process_tree_rss_bytes"] for item in operations
        ),
    }


def run_metadata_generations(
    binary: pathlib.Path,
    root: pathlib.Path,
    home: pathlib.Path,
    database: pathlib.Path,
    snapshot: pathlib.Path,
    manifest: dict[str, Any],
) -> dict[str, Any]:
    body_json = json.dumps(shared_body(manifest["body_bytes"]).decode("ascii"))
    operations = []
    common = [str(binary), "ingest", "--source", source_name(0), "--strict"]
    for generation in range(1, manifest["metadata_only_generations"]):
        update_sentinel_generation(snapshot, body_json, generation)
        operations.append(run_monitored(common, cwd=root, home=home))
        if generation % 10 == 0:
            print(f"metadata generation {generation}/100", file=sys.stderr)

    uri = f"file:{database}?mode=ro"
    with contextlib.ExitStack() as readers:
        old_readers = [
            readers.enter_context(
                contextlib.closing(
                    sqlite3.connect(uri, uri=True, isolation_level=None)
                )
            )
            for _ in range(manifest["concurrent_old_readers"])
        ]
        prior_generations = []
        for reader in old_readers:
            reader.execute("BEGIN")
            prior_generations.append(active_generation(reader))
        if len(set(prior_generations)) != 1:
            raise StressError("concurrent old readers did not begin at one generation")
        before = prior_generations[0]
        update_sentinel_generation(
            snapshot, body_json, manifest["metadata_only_generations"]
        )
        operations.append(run_monitored(common, cwd=root, home=home))
        retained_generations = [active_generation(reader) for reader in old_readers]
        with contextlib.closing(sqlite3.connect(uri, uri=True)) as fresh_reader:
            current = active_generation(fresh_reader)
    if any(retained != before for retained in retained_generations):
        raise StressError("an old reader did not retain its prior generation snapshot")
    if current != before + 1:
        raise StressError("the final metadata generation did not advance exactly once")
    return {
        "operations": summarize_operations(operations),
        "old_readers": {
            "count": len(old_readers),
            "prior_generations": prior_generations,
            "retained_generations": retained_generations,
            "fresh_reader_generation": current,
            "all_prior_snapshots_retained": True,
        },
    }


def final_database_counts(database: pathlib.Path) -> dict[str, int]:
    uri = f"file:{database}?mode=ro"
    with contextlib.closing(sqlite3.connect(uri, uri=True)) as connection:
        tables = {
            "sources": "sources",
            "project_sources": "project_sources",
            "documents": "documents",
            "document_versions": "document_versions",
            "content_blobs": "content_blobs",
            "chunks": "chunks",
            "generations": "generations",
            "chunk_embeddings": "chunk_embeddings",
        }
        return {
            label: int(connection.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0])
            for label, table in tables.items()
        }


def model_worker_pids(parent_pid: int) -> list[int]:
    listing = subprocess.run(
        ["ps", "-axo", "pid=,ppid=,command="],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    workers = []
    for line in listing.splitlines():
        fields = line.strip().split(None, 2)
        if len(fields) == 3 and int(fields[1]) == parent_pid and "__model-worker" in fields[2]:
            workers.append(int(fields[0]))
    return workers


def run_cancel_storm(
    binary: pathlib.Path,
    root: pathlib.Path,
    home: pathlib.Path,
    manifest: dict[str, Any],
) -> dict[str, Any]:
    with tempfile.TemporaryFile(mode="w+", encoding="utf-8") as stderr:
        process = subprocess.Popen(
            [str(binary), "--no-color", "--no-progress", "mcp"],
            cwd=root,
            env=command_environment(home),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=stderr,
            text=True,
        )
        sampler = scale.RssSampler(process.pid)
        sampler.start()
        stopped: set[int] = set()

        def send(frame: dict[str, Any]) -> None:
            assert process.stdin is not None
            process.stdin.write(json.dumps(frame, separators=(",", ":")) + "\n")
            process.stdin.flush()

        def receive(timeout: float = 60.0) -> dict[str, Any]:
            assert process.stdout is not None
            readable, _, _ = select.select([process.stdout], [], [], timeout)
            if not readable:
                raise StressError(f"MCP stress response timed out; process={process.poll()}")
            line = process.stdout.readline()
            if not line:
                raise StressError(f"MCP stress process disconnected; process={process.poll()}")
            return json.loads(line)

        def search(request_id: int, query: str, mode: str) -> None:
            send(
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "method": "tools/call",
                    "params": {
                        "name": "evidence_search",
                        "arguments": {
                            "query": query,
                            "mode": mode,
                            "limit": 10,
                            "timeout_ms": 60_000,
                            "explain": True,
                        },
                    },
                }
            )

        def resume() -> None:
            for pid in tuple(stopped):
                try:
                    os.kill(pid, signal.SIGCONT)
                except ProcessLookupError:
                    pass

        try:
            send(
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "clientInfo": {"name": "retrieval-stress", "version": "1"},
                    },
                }
            )
            initialized = receive()
            if (
                initialized.get("id") != 1
                or "error" in initialized
                or not isinstance(initialized.get("result"), dict)
            ):
                raise StressError("MCP stress initialization failed")
            send({"jsonrpc": "2.0", "method": "notifications/initialized"})
            cancellation_ids = tuple(range(2, 2 + manifest["cancelled_requests"]))
            heavy = "stress cancellation " + ("archive evidence " * 180)
            for request_id in cancellation_ids:
                search(request_id, f"{heavy}{request_id}", "semantic")
            deadline = time.monotonic() + 15
            while len(stopped) < 2 and time.monotonic() < deadline:
                for pid in model_worker_pids(process.pid):
                    if pid not in stopped:
                        try:
                            os.kill(pid, signal.SIGSTOP)
                            stopped.add(pid)
                        except ProcessLookupError:
                            pass
                time.sleep(0.005)
            if len(stopped) != 2:
                raise StressError(f"expected two real model workers, found {sorted(stopped)}")
            for request_id in cancellation_ids:
                send(
                    {
                        "jsonrpc": "2.0",
                        "method": "notifications/cancelled",
                        "params": {"requestId": request_id, "reason": "stress storm"},
                    }
                )
            resume()

            lexical_id = 100
            lexical_query = manifest["queries"][2]
            search(lexical_id, lexical_query["query"], "lexical")
            while True:
                response = receive()
                if response.get("id") in cancellation_ids:
                    raise StressError("cancelled stress request emitted a late response")
                if response.get("id") == lexical_id:
                    break
            if "error" in response:
                raise StressError(f"post-storm lexical search failed: {response['error']}")
            lexical_packet = response["result"]["structuredContent"]
            validate_search_packet(lexical_query, lexical_packet)

            hybrid_id = 101
            hybrid_query = manifest["queries"][-1]
            search(hybrid_id, hybrid_query["query"], "hybrid")
            while True:
                response = receive()
                if response.get("id") in cancellation_ids:
                    raise StressError("cancelled stress request emitted a late response")
                if response.get("id") == hybrid_id:
                    break
            if "error" in response:
                raise StressError(f"post-storm hybrid search failed: {response['error']}")
            hybrid_packet = response["result"]["structuredContent"]
            validate_search_packet(hybrid_query, hybrid_packet)

            assert process.stdin is not None
            process.stdin.close()
            if process.wait(timeout=30) != 0:
                raise StressError("MCP stress process exited unsuccessfully")
        finally:
            resume()
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
            if process.stdin is not None and not process.stdin.closed:
                process.stdin.close()
            if process.stdout is not None and not process.stdout.closed:
                process.stdout.close()
            sampler.stop()
        stderr.seek(0)
        stderr_text = stderr.read()
        if stderr_text:
            raise StressError(f"successful MCP stress process wrote stderr: {stderr_text[-2000:]}")
    if sampler.error is not None or sampler.peak_kib <= 0:
        raise StressError(f"cancel-storm RSS sampling failed: {sampler.error}")
    return {
        "cancelled_requests": len(cancellation_ids),
        "real_worker_processes_paused": len(stopped),
        "late_responses_suppressed": True,
        "lexical_recovery": True,
        "hybrid_recovery": True,
        "peak_process_tree_rss_bytes": sampler.peak_kib * 1024,
        "lexical_examined": lexical_packet["examined"],
        "hybrid_examined": hybrid_packet["examined"],
    }


def run_stress(args: argparse.Namespace) -> int:
    manifest = load_manifest()
    binary = args.hsum.resolve()
    workspace = args.workspace.resolve()
    home = args.home.resolve()
    output = args.output.resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise StressError("hsum binary is not executable")
    if not home.is_dir():
        raise StressError("isolated HSUM_HOME must already contain the pinned model")
    if workspace.exists() or output.exists():
        raise StressError("stress workspace and output must not exist")
    workspace.mkdir(parents=True, mode=0o700)
    root = workspace / "repository"
    snapshots_root = workspace / "snapshots"
    root.mkdir(mode=0o700)
    (root / ".git").mkdir()
    common = [str(binary), "--no-color", "--no-progress"]
    run_command(
        common
        + [
            "init",
            str(root),
            "--index",
            INDEX_NAME,
            "--project",
            PROJECT_NAME,
            "--embedding-model",
            manifest["model_id"],
        ],
        cwd=root,
        home=home,
    )
    paths, corpus = create_snapshots(snapshots_root, manifest)
    distribution = documents_per_source(manifest)
    scale_targets = manifest["scale_source_points"]
    next_source = 0
    cumulative_documents = 0
    scale_samples = []
    ingest_operations = []
    for total_sources in scale_targets:
        jsonl_target = total_sources - 1
        batch_names = []
        documents_before_batch = cumulative_documents
        while next_source < jsonl_target:
            name = source_name(next_source)
            run_command(
                common + ["source", "add", "jsonl", str(paths[next_source]), "--name", name],
                cwd=root,
                home=home,
            )
            batch_names.append(name)
            cumulative_documents += distribution[next_source]
            next_source += 1
        ingest_arguments = common + ["ingest", "--strict"]
        for name in batch_names:
            ingest_arguments.extend(["--source", name])
        print(
            f"stress ingest: {total_sources} sources, {cumulative_documents} documents",
            file=sys.stderr,
        )
        ingest_operations.append(
            {
                "source_count": total_sources,
                "documents": cumulative_documents,
                "passages": cumulative_documents * manifest["chunks_per_document"],
                "documents_added": cumulative_documents - documents_before_batch,
                "passages_added": (
                    cumulative_documents - documents_before_batch
                )
                * manifest["chunks_per_document"],
                **run_monitored(ingest_arguments, cwd=root, home=home),
            }
        )
        ingest_operations[-1]["passages_per_second"] = (
            ingest_operations[-1]["passages_added"]
            / ingest_operations[-1]["seconds"]
        )
        scale_samples.append(
            measure_scale_point(
                binary,
                root,
                home,
                manifest,
                total_sources,
                cumulative_documents,
            )
        )
    if cumulative_documents != manifest["document_count"]:
        raise StressError("final document count differs from the manifest")

    database = home / "data" / "indexes" / INDEX_NAME / "index.sqlite"
    metadata = run_metadata_generations(
        binary, root, home, database, paths[0], manifest
    )
    print("stress re-embed: building one-million active vector memberships", file=sys.stderr)
    reembed = run_monitored(common + ["ingest", "--reembed"], cwd=root, home=home)
    reembed["passages"] = manifest["expected_passages"]
    reembed["passages_per_second"] = (
        manifest["expected_passages"] / reembed["seconds"]
    )
    final_status = status_packet(binary, root, home)
    validate_status(
        final_status,
        sources=manifest["source_count"],
        documents=manifest["document_count"],
        passages=manifest["expected_passages"],
    )
    hybrid_query = manifest["queries"][-1]
    final_hybrid = summarize_probes(
        [
            probe_query(binary, root, home, hybrid_query)
            for _ in range(manifest["probe_repetitions"])
        ]
    )
    cancel_storm = run_cancel_storm(binary, root, home, manifest)
    counts = final_database_counts(database)
    if counts["sources"] != manifest["source_count"] or counts["documents"] != manifest[
        "document_count"
    ]:
        raise StressError("final database source/document counts drifted")
    if counts["content_blobs"] != 1:
        raise StressError("duplicate-heavy corpus did not deduplicate to one content blob")
    storage = scale.storage_metrics(database, final_status)
    managed = storage.get("managed_index_bytes")
    amplification = None
    if isinstance(managed, int):
        amplification = {
            "versus_encoded_snapshots": managed / corpus["encoded_snapshot_bytes"],
            "versus_unique_body": managed / manifest["body_bytes"],
        }
    report = {
        "schema_version": SCHEMA_VERSION,
        "passed": True,
        "created_at_unix_seconds": time.time(),
        "git": scale.git_revision(),
        "machine": scale.machine_report(),
        "hsum": {
            "version": scale.hsum_version(binary, cwd=root, home=home),
            "binary_sha256": sha256_file(binary),
        },
        "manifest_sha256": manifest_sha256(),
        "corpus_fingerprint_sha256": corpus_fingerprint(manifest),
        "corpus": corpus,
        "ingest": {
            "operations": ingest_operations,
            "total_seconds": sum(item["seconds"] for item in ingest_operations),
            "maximum_peak_process_tree_rss_bytes": max(
                item["peak_process_tree_rss_bytes"] for item in ingest_operations
            ),
        },
        "scale_samples": scale_samples,
        "first_observed_knee": first_observed_knee(
            scale_samples, manifest["knee_ratio_exclusive"]
        ),
        "metadata_only_generations": metadata,
        "reembed": reembed,
        "final_hybrid": final_hybrid,
        "cancel_storm": cancel_storm,
        "database_counts": counts,
        "storage": storage,
        "database_amplification": amplification,
        "qualification": {
            "report_only": True,
            "latency_slo_applied": False,
            "checks": {
                "one_million_active_passages": True,
                "sixty_four_sources": True,
                "duplicate_body_deduplicated": True,
                "one_hundred_metadata_only_generations": True,
                "concurrent_old_readers_retained_prior_snapshots": True,
                "final_hybrid_used_vectors": True,
                "cancel_storm_recovered": True,
                "rss_recorded": True,
            },
        },
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    with output.open("x", encoding="utf-8") as destination:
        json.dump(report, destination, sort_keys=True, separators=(",", ":"))
        destination.write("\n")
    print(f"retrieval stress PASS; report: {output}", file=sys.stderr)
    return 0


def validate() -> int:
    manifest = load_manifest()
    print(
        json.dumps(
            {
                "manifest_sha256": manifest_sha256(),
                "corpus_fingerprint_sha256": corpus_fingerprint(manifest),
                "sources": manifest["source_count"],
                "documents": manifest["document_count"],
                "passages": manifest["expected_passages"],
                "metadata_only_generations": manifest["metadata_only_generations"],
            },
            sort_keys=True,
        )
    )
    return 0


def parser() -> argparse.ArgumentParser:
    command = argparse.ArgumentParser(description=__doc__)
    subcommands = command.add_subparsers(dest="command", required=True)
    validate_parser = subcommands.add_parser("validate")
    validate_parser.set_defaults(handler=lambda _: validate())
    run_parser = subcommands.add_parser("run")
    run_parser.add_argument("--hsum", type=pathlib.Path, required=True)
    run_parser.add_argument("--workspace", type=pathlib.Path, required=True)
    run_parser.add_argument("--home", type=pathlib.Path, required=True)
    run_parser.add_argument("--output", type=pathlib.Path, required=True)
    run_parser.set_defaults(handler=run_stress)
    return command


def main() -> int:
    try:
        arguments = parser().parse_args()
        return arguments.handler(arguments)
    except (OSError, ValueError, KeyError, TypeError, sqlite3.Error, StressError) as error:
        print(f"retrieval stress failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
