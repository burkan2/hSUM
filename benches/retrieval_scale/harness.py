#!/usr/bin/env python3
"""Canonical 100k-chunk retrieval qualification harness for hSUM.

The measured path is one long-lived `hsum mcp` subprocess per run.  That keeps
process startup and first model initialization in the separately reported cold
measurements instead of charging them to every warm query.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import selectors
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any, Iterable


SCHEMA_VERSION = "hsum.retrieval-scale-report.v1"
SETUP_SCHEMA_VERSION = "hsum.retrieval-scale-setup.v1"
QUERY_SCHEMA_VERSION = "hsum.retrieval-scale-queries.v1"
INDEX_NAME = "retrieval-scale-100k"
PROJECT_NAME = "default"
DOCUMENT_COUNT = 10_000
DOCUMENT_BYTES = 15_000
EXPECTED_CHUNKS_PER_DOCUMENT = 10
EXPECTED_PASSAGES = DOCUMENT_COUNT * EXPECTED_CHUNKS_PER_DOCUMENT
CANONICAL_RUNS = 3
CANONICAL_WARMUPS = 5
CANONICAL_PASSES = 30
RSS_SAMPLE_INTERVAL_SECONDS = 0.05
MCP_RESPONSE_TIMEOUT_SECONDS = 30.0
QUERY_PATH = Path(__file__).with_name("queries.json")
SETUP_REPORT_NAME = ".hsum-retrieval-scale-setup.json"
STAGES = (
    "query_embedding",
    "exact",
    "exact_fallback",
    "lexical",
    "vector",
    "fusion",
    "body_materialization",
    "total",
)
CLASS_SLOS_MS = {
    "identifier": 100,
    "ordinary_lexical": 100,
    "exact_fallback": 750,
    "hybrid_no_fallback": 500,
}
EXPECTED_CLASS_COUNTS = {
    "identifier": 7,
    "ordinary_lexical": 6,
    "exact_fallback": 6,
    "hybrid_no_fallback": 6,
}
FILLER = " local evidence archive bounded retrieval immutable generation vector citation "


class HarnessError(RuntimeError):
    """A fail-closed qualification or orchestration error."""


def canonical_json_bytes(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def read_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise HarnessError(f"unable to read JSON from {path}: {error}") from error


def write_json_atomic(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp-{os.getpid()}")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(temporary, path)


def load_queries(path: Path = QUERY_PATH) -> list[dict[str, str]]:
    packet = read_json(path)
    if not isinstance(packet, dict) or packet.get("schema_version") != QUERY_SCHEMA_VERSION:
        raise HarnessError(f"unsupported query manifest schema in {path}")
    queries = packet.get("queries")
    if not isinstance(queries, list):
        raise HarnessError("query manifest must contain a queries array")
    required = {"id", "class", "mode", "query", "corpus_phrase"}
    identifiers: set[str] = set()
    validated: list[dict[str, str]] = []
    for position, query in enumerate(queries):
        if not isinstance(query, dict) or set(query) != required:
            raise HarnessError(f"query {position} must have exactly {sorted(required)}")
        if not all(isinstance(query[field], str) and query[field] for field in required):
            raise HarnessError(f"query {position} contains an empty or non-string field")
        if query["id"] in identifiers:
            raise HarnessError(f"duplicate query id: {query['id']}")
        identifiers.add(query["id"])
        expected_mode = "hybrid" if query["class"] == "hybrid_no_fallback" else "lexical"
        if query["mode"] != expected_mode:
            raise HarnessError(f"query {query['id']} must use {expected_mode} mode")
        if any(character in query["corpus_phrase"] for character in "\n\r.!?"):
            raise HarnessError(
                f"query {query['id']} corpus phrase changes the frozen chunk boundary contract"
            )
        validated.append(dict(query))
    counts = Counter(query["class"] for query in validated)
    if dict(counts) != EXPECTED_CLASS_COUNTS:
        raise HarnessError(
            f"query class counts must be {EXPECTED_CLASS_COUNTS}, found {dict(counts)}"
        )
    if len(validated) * CANONICAL_PASSES < 750:
        raise HarnessError("canonical measured passes must contain at least 750 observations")
    return validated


def unbroken_chunk_count(
    body_bytes: int,
    target_bytes: int = 1_200,
    maximum_bytes: int = 1_800,
    overlap_bytes: int = 180,
) -> int:
    """Mirror chunk progression for one line with no preferred boundaries."""
    start = 0
    chunks = 0
    while start < body_bytes:
        remaining = body_bytes - start
        end = body_bytes if remaining <= target_bytes else min(start + maximum_bytes, body_bytes)
        chunks += 1
        if end == body_bytes:
            break
        start = max(end - overlap_bytes, start + 1)
    return chunks


def corpus_body(document_index: int, queries: list[dict[str, str]]) -> bytes:
    phrase = queries[document_index % len(queries)]["corpus_phrase"]
    prefix = f"{phrase} scale_document_{document_index:05d}"
    body = (prefix + FILLER * ((DOCUMENT_BYTES // len(FILLER)) + 2))[:DOCUMENT_BYTES]
    encoded = body.encode("ascii")
    if len(encoded) != DOCUMENT_BYTES:
        raise HarnessError("generated corpus body length drifted")
    return encoded


def corpus_fingerprint(queries: list[dict[str, str]]) -> str:
    contract = {
        "documents": DOCUMENT_COUNT,
        "bytes_per_document": DOCUMENT_BYTES,
        "chunks_per_document": EXPECTED_CHUNKS_PER_DOCUMENT,
        "filler": FILLER,
        "queries": queries,
    }
    return sha256_bytes(canonical_json_bytes(contract))


def create_corpus(root: Path, queries: list[dict[str, str]]) -> dict[str, Any]:
    if root.exists() and any(root.iterdir()):
        raise HarnessError(f"corpus directory must be absent or empty: {root}")
    root.mkdir(parents=True, exist_ok=True)
    root.joinpath(".git").mkdir()
    started = time.perf_counter()
    total_bytes = 0
    for document_index in range(DOCUMENT_COUNT):
        body = corpus_body(document_index, queries)
        root.joinpath(f"document-{document_index:05d}.txt").write_bytes(body)
        total_bytes += len(body)
    elapsed = time.perf_counter() - started
    manifest = {
        "schema_version": "hsum.retrieval-scale-corpus.v1",
        "fingerprint_sha256": corpus_fingerprint(queries),
        "documents": DOCUMENT_COUNT,
        "bytes": total_bytes,
        "bytes_per_document": DOCUMENT_BYTES,
        "expected_chunks_per_document": EXPECTED_CHUNKS_PER_DOCUMENT,
        "expected_passages": EXPECTED_PASSAGES,
        "generation_seconds": elapsed,
    }
    write_json_atomic(root / ".hsum-retrieval-scale-corpus.json", manifest)
    return manifest


def command_environment(home: Path) -> dict[str, str]:
    environment = os.environ.copy()
    environment["HSUM_HOME"] = str(home)
    environment["HSUM_OFFLINE"] = "1"
    environment.pop("HSUM_INDEX", None)
    environment.pop("HSUM_PROJECT", None)
    return environment


def run_command(
    arguments: list[str],
    *,
    cwd: Path,
    home: Path,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    completed = subprocess.run(
        arguments,
        cwd=cwd,
        env=command_environment(home),
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if check and completed.returncode != 0:
        raise HarnessError(
            f"command failed ({completed.returncode}): {' '.join(arguments)}\n"
            f"stdout:\n{completed.stdout[-4000:]}\nstderr:\n{completed.stderr[-4000:]}"
        )
    return completed


def run_json_command(arguments: list[str], *, cwd: Path, home: Path) -> dict[str, Any]:
    completed = run_command(arguments, cwd=cwd, home=home)
    try:
        packet = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise HarnessError(f"command returned malformed JSON: {' '.join(arguments)}") from error
    if not isinstance(packet, dict):
        raise HarnessError(f"command returned a non-object JSON packet: {' '.join(arguments)}")
    return packet


def hsum_version(binary: Path, *, cwd: Path, home: Path) -> str:
    output = run_command([str(binary), "--version"], cwd=cwd, home=home).stdout.strip()
    if not output:
        raise HarnessError("hsum --version returned an empty response")
    return output


def prepare(args: argparse.Namespace) -> int:
    binary = args.hsum.resolve()
    corpus = args.corpus.resolve()
    home = args.home.resolve()
    output = (args.output or corpus / SETUP_REPORT_NAME).resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise HarnessError(f"hsum binary is not executable: {binary}")
    if not home.is_dir():
        raise HarnessError(f"HSUM_HOME must already contain the staged model: {home}")
    if not corpus.parent.is_dir():
        raise HarnessError(f"corpus parent directory does not exist: {corpus.parent}")
    if corpus.exists() and any(corpus.iterdir()):
        raise HarnessError(f"corpus directory must be absent or empty: {corpus}")
    database_path = home / "data" / "indexes" / args.index / "index.sqlite"
    if database_path.exists():
        raise HarnessError(f"benchmark index already exists: {database_path}")
    if unbroken_chunk_count(DOCUMENT_BYTES) != EXPECTED_CHUNKS_PER_DOCUMENT:
        raise HarnessError("frozen corpus no longer produces ten chunks per document")
    queries = load_queries(args.queries.resolve())

    # Model acquisition is intentionally outside this command. Verification is
    # offline and runs before corpus generation, so a missing model does not
    # leave a 150 MB partial setup behind.
    run_command(
        [str(binary), "model", "verify", args.model_id],
        cwd=corpus.parent,
        home=home,
    )
    corpus_manifest = create_corpus(corpus, queries)
    init_started = time.perf_counter()
    run_command(
        [
            str(binary),
            "init",
            str(corpus),
            "--index",
            args.index,
            "--project",
            PROJECT_NAME,
            "--allow-large-source",
            "--embedding-model",
            args.model_id,
        ],
        cwd=corpus,
        home=home,
    )
    ingest_seconds = time.perf_counter() - init_started
    status = run_json_command([str(binary), "status", "--json"], cwd=corpus, home=home)
    active_passages = status.get("active_passages")
    if active_passages != EXPECTED_PASSAGES:
        raise HarnessError(
            f"prepared index must contain {EXPECTED_PASSAGES} passages, found {active_passages}"
        )

    reembed_started = time.perf_counter()
    run_command([str(binary), "ingest", "--reembed"], cwd=corpus, home=home)
    reembed_seconds = time.perf_counter() - reembed_started
    semantic_probe = run_json_command(
        [
            str(binary),
            "search",
            queries[-1]["query"],
            "--mode",
            "hybrid",
            "--limit",
            "10",
            "--json",
        ],
        cwd=corpus,
        home=home,
    )
    if semantic_probe.get("effective_mode") != "hybrid" or semantic_probe.get("degraded_mode"):
        raise HarnessError("prepared index did not reach non-degraded hybrid readiness")

    report = {
        "schema_version": SETUP_SCHEMA_VERSION,
        "created_at_unix_seconds": time.time(),
        "hsum_version": hsum_version(binary, cwd=corpus, home=home),
        "binary_sha256": sha256_file(binary),
        "model_id": args.model_id,
        "index_name": args.index,
        "project_name": PROJECT_NAME,
        "corpus": corpus_manifest,
        "query_manifest_sha256": sha256_file(args.queries.resolve()),
        "ingest": {
            "seconds": ingest_seconds,
            "passages_per_second": EXPECTED_PASSAGES / ingest_seconds,
        },
        "reembed": {
            "seconds": reembed_seconds,
            "passages_per_second": EXPECTED_PASSAGES / reembed_seconds,
        },
        "status": status,
    }
    write_json_atomic(output, report)
    print(f"prepared {EXPECTED_PASSAGES} passages; setup report: {output}")
    return 0


def process_tree_rss_kib(root_pid: int) -> int:
    completed = subprocess.run(
        ["ps", "-axo", "pid=,ppid=,rss="],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    if completed.returncode != 0:
        raise HarnessError("ps failed while sampling process-tree RSS")
    rows: dict[int, tuple[int, int]] = {}
    for line in completed.stdout.splitlines():
        fields = line.split()
        if len(fields) != 3:
            continue
        try:
            pid, parent_pid, rss_kib = map(int, fields)
        except ValueError:
            continue
        rows[pid] = (parent_pid, rss_kib)
    descendants = {root_pid}
    changed = True
    while changed:
        changed = False
        for pid, (parent_pid, _) in rows.items():
            if parent_pid in descendants and pid not in descendants:
                descendants.add(pid)
                changed = True
    return sum(rows.get(pid, (0, 0))[1] for pid in descendants)


class RssSampler:
    def __init__(self, root_pid: int) -> None:
        self.root_pid = root_pid
        self.peak_kib = 0
        self.error: str | None = None
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._sample, daemon=True)

    def start(self) -> None:
        self._thread.start()

    def stop(self) -> None:
        self._stop.set()
        self._thread.join(timeout=2)

    def _sample(self) -> None:
        while not self._stop.is_set():
            try:
                self.peak_kib = max(self.peak_kib, process_tree_rss_kib(self.root_pid))
            except HarnessError as error:
                self.error = str(error)
                return
            self._stop.wait(RSS_SAMPLE_INTERVAL_SECONDS)


class McpSession:
    def __init__(self, binary: Path, corpus: Path, home: Path) -> None:
        self._next_id = 2
        self._closed = False
        self._stderr = tempfile.TemporaryFile(mode="w+", encoding="utf-8")
        started = time.perf_counter()
        try:
            self.process = subprocess.Popen(
                [str(binary), "mcp"],
                cwd=corpus,
                env=command_environment(home),
                text=True,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=self._stderr,
                bufsize=1,
            )
        except Exception:
            self._stderr.close()
            raise
        if self.process.stdin is None or self.process.stdout is None:
            raise HarnessError("unable to open MCP stdio pipes")
        self.sampler = RssSampler(self.process.pid)
        self.sampler.start()
        try:
            self._write(
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "clientInfo": {"name": "retrieval-scale-harness", "version": "1"},
                    },
                }
            )
            initialized = self._read_response(1)
            if "error" in initialized:
                raise HarnessError(f"MCP initialize failed: {initialized['error']}")
            self._write({"jsonrpc": "2.0", "method": "notifications/initialized"})
            self.process_start_ms = (time.perf_counter() - started) * 1000.0
        except Exception:
            self._shutdown()
            self._stderr.close()
            raise

    def _write(self, frame: dict[str, Any]) -> None:
        assert self.process.stdin is not None
        self.process.stdin.write(json.dumps(frame, separators=(",", ":")) + "\n")
        self.process.stdin.flush()

    def _read_response(self, request_id: int) -> dict[str, Any]:
        assert self.process.stdout is not None
        deadline = time.monotonic() + MCP_RESPONSE_TIMEOUT_SECONDS
        with selectors.DefaultSelector() as selector:
            selector.register(self.process.stdout, selectors.EVENT_READ)
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0 or not selector.select(remaining):
                    raise HarnessError(
                        f"MCP response {request_id} exceeded the "
                        f"{MCP_RESPONSE_TIMEOUT_SECONDS:.0f}-second harness deadline"
                    )
                line = self.process.stdout.readline()
                if not line:
                    raise HarnessError(
                        f"MCP process exited before response {request_id}; stderr:\n"
                        f"{self._stderr_text()[-4000:]}"
                    )
                try:
                    frame = json.loads(line)
                except json.JSONDecodeError as error:
                    raise HarnessError(f"MCP emitted malformed JSON: {line[:200]}") from error
                if frame.get("id") == request_id:
                    return frame

    def search(
        self,
        query: dict[str, str],
        *,
        mode_override: str | None = None,
    ) -> tuple[dict[str, Any], float]:
        request_id = self._next_id
        self._next_id += 1
        frame = {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/call",
            "params": {
                "name": "evidence_search",
                "arguments": {
                    "query": query["query"],
                    "mode": mode_override or query["mode"],
                    "limit": 10,
                    "timeout_ms": 10_000,
                    "explain": True,
                },
            },
        }
        started = time.perf_counter()
        self._write(frame)
        response = self._read_response(request_id)
        client_ms = (time.perf_counter() - started) * 1000.0
        if "error" in response:
            raise HarnessError(f"MCP search {query['id']} failed: {response['error']}")
        packet = response.get("result", {}).get("structuredContent")
        if not isinstance(packet, dict):
            raise HarnessError(f"MCP search {query['id']} omitted structuredContent")
        return packet, client_ms

    def _stderr_text(self) -> str:
        self._stderr.flush()
        self._stderr.seek(0)
        return self._stderr.read()

    def _shutdown(self) -> None:
        if self._closed:
            return
        self._closed = True
        if self.process.stdin is not None:
            try:
                self.process.stdin.close()
            except (BrokenPipeError, OSError):
                pass
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
        if self.process.stdout is not None:
            self.process.stdout.close()
        self.sampler.stop()
        self._stderr.flush()

    def close(self) -> tuple[int, str | None]:
        self._shutdown()
        stderr = self._stderr_text()
        result = (self.sampler.peak_kib, self.sampler.error)
        self._stderr.close()
        if self.process.returncode != 0:
            raise HarnessError(
                f"MCP process exited with {self.process.returncode}; stderr:\n{stderr[-4000:]}"
            )
        return result


def validate_search_packet(
    query: dict[str, str],
    packet: dict[str, Any],
    *,
    expected_mode: str | None = None,
) -> None:
    mode = expected_mode or query["mode"]
    if packet.get("requested_mode") != mode or packet.get("effective_mode") != mode:
        raise HarnessError(
            f"query {query['id']} measured {packet.get('requested_mode')}/"
            f"{packet.get('effective_mode')} instead of {mode}"
        )
    if packet.get("degraded_mode"):
        raise HarnessError(f"query {query['id']} degraded: {packet['degraded_mode']}")
    if not isinstance(packet.get("results"), list) or not packet["results"]:
        raise HarnessError(f"query {query['id']} returned no evidence")
    retrievers = packet.get("retrievers")
    if not isinstance(retrievers, list):
        raise HarnessError(f"query {query['id']} omitted retriever evidence")
    if query["class"] == "exact_fallback" and "exact_fallback" not in retrievers:
        raise HarnessError(f"query {query['id']} did not exercise exact_fallback")
    if query["class"] == "hybrid_no_fallback" and "exact_fallback" in retrievers:
        raise HarnessError(f"query {query['id']} unexpectedly exercised exact_fallback")
    timings = packet.get("timing_ms")
    if not isinstance(timings, dict) or any(
        not isinstance(timings.get(stage), (int, float)) for stage in STAGES
    ):
        raise HarnessError(f"query {query['id']} omitted frozen stage timing fields")


def compact_observation(
    query: dict[str, str],
    packet: dict[str, Any],
    client_ms: float,
    pass_index: int,
) -> dict[str, Any]:
    return {
        "pass": pass_index,
        "query_id": query["id"],
        "class": query["class"],
        "client_round_trip_ms": client_ms,
        "timing_ms": {stage: packet["timing_ms"][stage] for stage in STAGES},
        "examined": packet.get("examined"),
        "retrievers": packet.get("retrievers"),
        "stop_reason": packet.get("stop_reason"),
        "result_count": len(packet["results"]),
    }


def run_cache_helper(helper: Path, database_path: Path, *, home: Path, corpus: Path) -> dict[str, Any]:
    started = time.perf_counter()
    completed = subprocess.run(
        [str(helper), str(database_path)],
        cwd=corpus,
        env=command_environment(home),
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    elapsed_ms = (time.perf_counter() - started) * 1000.0
    if completed.returncode != 0:
        raise HarnessError(
            f"cold-cache helper failed ({completed.returncode}): {helper}\n"
            f"stdout:\n{completed.stdout[-2000:]}\nstderr:\n{completed.stderr[-2000:]}"
        )
    return {"elapsed_ms": elapsed_ms, "stdout": completed.stdout[-1000:]}


def run_one_process(
    run_index: int,
    *,
    binary: Path,
    corpus: Path,
    home: Path,
    database_path: Path,
    queries: list[dict[str, str]],
    warmups: int,
    passes: int,
    cold_cache_helper: Path | None,
) -> dict[str, Any]:
    session = McpSession(binary, corpus, home)
    cold: dict[str, Any] = {"process_start_ms": session.process_start_ms}
    try:
        if cold_cache_helper is not None:
            cold["cache_eviction"] = run_cache_helper(
                cold_cache_helper, database_path, home=home, corpus=corpus
            )
            cold_query = next(query for query in queries if query["class"] == "ordinary_lexical")
            packet, client_ms = session.search(cold_query)
            validate_search_packet(cold_query, packet)
            cold["os_cache_lexical"] = {
                "query_id": cold_query["id"],
                "client_round_trip_ms": client_ms,
                "timing_ms": packet["timing_ms"],
            }
        else:
            cold["cache_eviction"] = None
            cold["os_cache_lexical"] = None

        hybrid_query = next(query for query in queries if query["class"] == "hybrid_no_fallback")
        packet, client_ms = session.search(hybrid_query, mode_override="semantic")
        validate_search_packet(hybrid_query, packet, expected_mode="semantic")
        cold["first_model_load"] = {
            "query_id": hybrid_query["id"],
            "client_round_trip_ms": client_ms,
            "timing_ms": packet["timing_ms"],
        }
        packet, client_ms = session.search(hybrid_query)
        validate_search_packet(hybrid_query, packet)
        cold["first_hybrid"] = {
            "query_id": hybrid_query["id"],
            "client_round_trip_ms": client_ms,
            "timing_ms": packet["timing_ms"],
        }

        for _ in range(warmups):
            for query in queries:
                packet, _ = session.search(query)
                validate_search_packet(query, packet)

        observations: list[dict[str, Any]] = []
        for pass_index in range(passes):
            for query in queries:
                packet, client_ms = session.search(query)
                validate_search_packet(query, packet)
                observations.append(
                    compact_observation(query, packet, client_ms, pass_index + 1)
                )
    finally:
        peak_rss_kib, rss_error = session.close()

    summary = summarize_observations(observations)
    class_slos = evaluate_class_slos(summary)
    return {
        "run": run_index,
        "fresh_process": True,
        "cold": cold,
        "warmup_passes": warmups,
        "measured_passes": passes,
        "observation_count": len(observations),
        "peak_process_tree_rss_bytes": peak_rss_kib * 1024,
        "rss_sample_interval_ms": RSS_SAMPLE_INTERVAL_SECONDS * 1000,
        "rss_sampling_error": rss_error,
        "summary": summary,
        "class_slos": class_slos,
        "observations": observations,
    }


def nearest_rank(values: Iterable[float], percentile: float) -> float:
    ordered = sorted(values)
    if not ordered:
        raise HarnessError("cannot compute a percentile of an empty sample")
    if not 0 < percentile <= 1:
        raise HarnessError("percentile must be in (0, 1]")
    index = max(0, math.ceil(percentile * len(ordered)) - 1)
    return ordered[index]


def distribution(values: Iterable[float]) -> dict[str, float | int]:
    sample = list(values)
    return {
        "n": len(sample),
        "p50": nearest_rank(sample, 0.50),
        "p95": nearest_rank(sample, 0.95),
        "worst": max(sample),
    }


def summarize_observations(observations: list[dict[str, Any]]) -> dict[str, Any]:
    by_class: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for observation in observations:
        by_class[observation["class"]].append(observation)
    classes: dict[str, Any] = {}
    for query_class in CLASS_SLOS_MS:
        class_observations = by_class.get(query_class, [])
        if not class_observations:
            raise HarnessError(f"no measured observations for {query_class}")
        stage_distributions = {
            stage: distribution(
                observation["timing_ms"][stage] for observation in class_observations
            )
            for stage in STAGES
        }
        stage_distributions["client_round_trip"] = distribution(
            observation["client_round_trip_ms"] for observation in class_observations
        )
        classes[query_class] = {
            "observations": len(class_observations),
            "latency_ms": stage_distributions,
            "stop_reasons": dict(Counter(item["stop_reason"] for item in class_observations)),
            "max_examined": max_examined(class_observations),
        }
    return {
        "classes": classes,
        "all_total_ms": distribution(
            observation["timing_ms"]["total"] for observation in observations
        ),
        "all_client_round_trip_ms": distribution(
            observation["client_round_trip_ms"] for observation in observations
        ),
    }


def max_examined(observations: list[dict[str, Any]]) -> dict[str, int]:
    keys = ("exact", "exact_fallback", "lexical", "vector")
    result = {key: 0 for key in keys}
    for observation in observations:
        examined = observation.get("examined")
        if not isinstance(examined, dict):
            continue
        for key in keys:
            value = examined.get(key)
            if isinstance(value, int):
                result[key] = max(result[key], value)
    return result


def evaluate_class_slos(summary: dict[str, Any]) -> dict[str, Any]:
    results: dict[str, Any] = {}
    for query_class, threshold_ms in CLASS_SLOS_MS.items():
        p95 = summary["classes"][query_class]["latency_ms"]["total"]["p95"]
        results[query_class] = {
            "threshold_ms_exclusive": threshold_ms,
            "observed_p95_ms": p95,
            "pass": p95 < threshold_ms,
        }
    return results


def coefficient_of_variation_percent(values: Iterable[float]) -> float:
    sample = list(values)
    if not sample:
        raise HarnessError("cannot compute variation of an empty sample")
    mean = statistics.fmean(sample)
    if mean == 0:
        return 0.0 if all(value == 0 for value in sample) else math.inf
    return statistics.pstdev(sample) / mean * 100.0


def storage_metrics(database_path: Path, status: dict[str, Any]) -> dict[str, Any]:
    uri = f"file:{database_path}?mode=ro"
    try:
        connection = sqlite3.connect(uri, uri=True)
        live_bytes = connection.execute(
            """
            WITH live_blobs AS (
                SELECT DISTINCT dv.content_blob_id
                FROM document_heads AS dh
                JOIN document_versions AS dv ON dv.id = dh.document_version_id
                WHERE dh.state = 'active'
            )
            SELECT COALESCE(SUM(length(cb.original_bytes)), 0)
            FROM content_blobs AS cb
            JOIN live_blobs AS live ON live.content_blob_id = cb.id
            """
        ).fetchone()[0]
        history_bytes = connection.execute(
            """
            WITH live_blobs AS (
                SELECT DISTINCT dv.content_blob_id
                FROM document_heads AS dh
                JOIN document_versions AS dv ON dv.id = dh.document_version_id
                WHERE dh.state = 'active'
            ), version_blobs AS (
                SELECT DISTINCT content_blob_id FROM document_versions
            )
            SELECT COALESCE(SUM(length(cb.original_bytes)), 0)
            FROM content_blobs AS cb
            JOIN version_blobs AS versions ON versions.content_blob_id = cb.id
            LEFT JOIN live_blobs AS live ON live.content_blob_id = cb.id
            WHERE live.content_blob_id IS NULL
            """
        ).fetchone()[0]
        vector_rows, cached_vector_bytes = connection.execute(
            "SELECT COUNT(*), COALESCE(SUM(length(vector_blob)), 0) FROM chunk_embeddings"
        ).fetchone()
        connection.close()
    except sqlite3.Error as error:
        raise HarnessError(f"unable to measure logical storage bytes: {error}") from error
    database_files = {}
    for suffix in ("", "-wal", "-shm"):
        path = Path(f"{database_path}{suffix}")
        database_files[path.name] = path.stat().st_size if path.exists() else 0
    storage = status.get("storage") if isinstance(status.get("storage"), dict) else {}
    active_passages = status.get("active_passages")
    if not isinstance(active_passages, int) or active_passages < 0:
        raise HarnessError("status omitted a valid active_passages count")
    active_vector_bytes = active_passages * 384 * 4
    return {
        "logical_live_bytes": int(live_bytes),
        "logical_history_bytes": int(history_bytes),
        "cached_vector_rows": int(vector_rows),
        "logical_cached_vector_payload_bytes": int(cached_vector_bytes),
        "logical_active_vector_payload_bytes": active_vector_bytes,
        "logical_total_vector_payload_bytes": int(cached_vector_bytes) + active_vector_bytes,
        "managed_index_bytes": storage.get("managed_index_bytes"),
        "reclaimable_bytes": storage.get("reclaimable_bytes"),
        "database_files": database_files,
        "definitions": {
            "logical_live_bytes": "distinct content blobs referenced by active document heads",
            "logical_history_bytes": "distinct version content blobs not referenced by an active head",
            "logical_cached_vector_payload_bytes": "sum of immutable chunk_embeddings.vector_blob payloads",
            "logical_active_vector_payload_bytes": "active passages multiplied by 384 float32 components",
            "logical_total_vector_payload_bytes": "cached plus active logical vector payloads; excludes SQLite and sqlite-vec page overhead",
        },
    }


def machine_report() -> dict[str, Any]:
    cpu = platform.processor() or platform.machine()
    if platform.system() == "Darwin":
        completed = subprocess.run(
            ["sysctl", "-n", "machdep.cpu.brand_string"],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        if completed.returncode == 0 and completed.stdout.strip():
            cpu = completed.stdout.strip()
    memory_bytes = None
    if platform.system() == "Darwin":
        completed = subprocess.run(
            ["sysctl", "-n", "hw.memsize"],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        if completed.returncode == 0 and completed.stdout.strip().isdigit():
            memory_bytes = int(completed.stdout.strip())
    elif Path("/proc/meminfo").is_file():
        for line in Path("/proc/meminfo").read_text(encoding="utf-8").splitlines():
            if line.startswith("MemTotal:"):
                memory_bytes = int(line.split()[1]) * 1024
                break
    return {
        "system": platform.system(),
        "release": platform.release(),
        "architecture": platform.machine(),
        "cpu": cpu,
        "memory_bytes": memory_bytes,
        "python": platform.python_version(),
    }


def git_revision() -> dict[str, Any]:
    repository = Path(__file__).resolve().parents[2]
    revision = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=repository,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    dirty = subprocess.run(
        ["git", "status", "--short"],
        cwd=repository,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return {
        "commit": revision.stdout.strip() if revision.returncode == 0 else None,
        "working_tree_dirty": bool(dirty.stdout.strip()) if dirty.returncode == 0 else None,
    }


def qualify_report(
    runs: list[dict[str, Any]],
    *,
    canonical_parameters: bool,
    setup: dict[str, Any] | None,
    cold_cache_available: bool,
) -> dict[str, Any]:
    p95_values = [run["summary"]["all_total_ms"]["p95"] for run in runs]
    variation = coefficient_of_variation_percent(p95_values)
    checks = {
        "canonical_parameters": canonical_parameters,
        "three_fresh_processes": len(runs) == CANONICAL_RUNS
        and all(run["fresh_process"] for run in runs),
        "at_least_750_observations_per_run": all(
            run["observation_count"] >= 750 for run in runs
        ),
        "all_class_slos": all(
            outcome["pass"]
            for run in runs
            for outcome in run["class_slos"].values()
        ),
        "total_p95_cv_at_most_10_percent": variation <= 10.0,
        "cold_os_cache_method_recorded": cold_cache_available,
        "setup_and_ingest_throughput_recorded": setup is not None
        and isinstance(setup.get("ingest", {}).get("passages_per_second"), (int, float)),
        "rss_complete": all(
            run["peak_process_tree_rss_bytes"] > 0 and run["rss_sampling_error"] is None
            for run in runs
        ),
    }
    return {
        "pass": all(checks.values()),
        "checks": checks,
        "run_level_total_p95_ms": p95_values,
        "total_p95_coefficient_of_variation_percent": variation,
        "maximum_cv_percent": 10.0,
    }


def run_benchmark(args: argparse.Namespace) -> int:
    binary = args.hsum.resolve()
    corpus = args.corpus.resolve()
    home = args.home.resolve()
    output = args.output.resolve()
    query_path = args.queries.resolve()
    helper = args.cold_cache_helper.resolve() if args.cold_cache_helper else None
    setup_path = (args.setup_report or corpus / SETUP_REPORT_NAME).resolve()
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise HarnessError(f"hsum binary is not executable: {binary}")
    if not corpus.is_dir() or not home.is_dir():
        raise HarnessError("corpus and HSUM_HOME must already exist")
    if helper is not None and (not helper.is_file() or not os.access(helper, os.X_OK)):
        raise HarnessError(f"cold-cache helper is not executable: {helper}")
    if helper is None and not args.allow_missing_cold_cache:
        raise HarnessError(
            "canonical qualification requires --cold-cache-helper; use "
            "--allow-missing-cold-cache only for harness development"
        )
    for name, value in (("runs", args.runs), ("warmups", args.warmups), ("passes", args.passes)):
        if value <= 0:
            raise HarnessError(f"--{name} must be positive")

    queries = load_queries(query_path)
    corpus_manifest = read_json(corpus / ".hsum-retrieval-scale-corpus.json")
    if corpus_manifest.get("fingerprint_sha256") != corpus_fingerprint(queries):
        raise HarnessError("corpus fingerprint does not match the frozen query/corpus contract")
    status = run_json_command([str(binary), "status", "--json"], cwd=corpus, home=home)
    if status.get("active_passages") != EXPECTED_PASSAGES:
        raise HarnessError(
            f"benchmark requires {EXPECTED_PASSAGES} active passages, "
            f"found {status.get('active_passages')}"
        )
    database_path = home / "data" / "indexes" / args.index / "index.sqlite"
    if not database_path.is_file():
        raise HarnessError(f"managed index database is missing: {database_path}")
    setup = read_json(setup_path) if setup_path.is_file() else None
    if setup is not None:
        if setup.get("schema_version") != SETUP_SCHEMA_VERSION:
            raise HarnessError(f"unsupported setup report: {setup_path}")
        if setup.get("corpus", {}).get("fingerprint_sha256") != corpus_manifest.get(
            "fingerprint_sha256"
        ):
            raise HarnessError("setup report was produced for a different corpus")
        expected_setup = {
            "binary_sha256": sha256_file(binary),
            "query_manifest_sha256": sha256_file(query_path),
            "index_name": args.index,
            "project_name": PROJECT_NAME,
        }
        mismatched = {
            key: {"expected": expected, "actual": setup.get(key)}
            for key, expected in expected_setup.items()
            if setup.get(key) != expected
        }
        if mismatched:
            raise HarnessError(f"setup provenance mismatch: {mismatched}")

    runs = []
    for run_index in range(1, args.runs + 1):
        print(f"run {run_index}/{args.runs}: starting fresh MCP process", file=sys.stderr)
        runs.append(
            run_one_process(
                run_index,
                binary=binary,
                corpus=corpus,
                home=home,
                database_path=database_path,
                queries=queries,
                warmups=args.warmups,
                passes=args.passes,
                cold_cache_helper=helper,
            )
        )

    canonical_parameters = (
        args.runs == CANONICAL_RUNS
        and args.warmups == CANONICAL_WARMUPS
        and args.passes == CANONICAL_PASSES
        and len(queries) * args.passes >= 750
    )
    qualification = qualify_report(
        runs,
        canonical_parameters=canonical_parameters,
        setup=setup,
        cold_cache_available=helper is not None,
    )
    report = {
        "schema_version": SCHEMA_VERSION,
        "created_at_unix_seconds": time.time(),
        "git": git_revision(),
        "machine": machine_report(),
        "hsum": {
            "version": hsum_version(binary, cwd=corpus, home=home),
            "binary_sha256": sha256_file(binary),
        },
        "protocol": {
            "runs": args.runs,
            "warmup_full_query_passes": args.warmups,
            "measured_full_query_passes": args.passes,
            "queries_per_pass": len(queries),
            "observations_per_run": len(queries) * args.passes,
            "limit": 10,
            "percentile_method": "nearest-rank ceil(p*n)",
            "fixed_query_order": [query["id"] for query in queries],
            "query_manifest_sha256": sha256_file(query_path),
            "cold_cache_helper": str(helper) if helper else None,
            "cold_cache_helper_sha256": sha256_file(helper) if helper else None,
        },
        "corpus": corpus_manifest,
        "setup": setup,
        "storage": storage_metrics(database_path, status),
        "runs": runs,
        "qualification": qualification,
    }
    write_json_atomic(output, report)
    print(
        f"qualification={'PASS' if qualification['pass'] else 'FAIL'}; report: {output}",
        file=sys.stderr,
    )
    return 0 if qualification["pass"] else 1


def validate_command(args: argparse.Namespace) -> int:
    queries = load_queries(args.queries.resolve())
    chunks = unbroken_chunk_count(DOCUMENT_BYTES)
    if chunks != EXPECTED_CHUNKS_PER_DOCUMENT:
        raise HarnessError(f"frozen corpus chunk count drifted to {chunks}")
    print(
        json.dumps(
            {
                "queries": len(queries),
                "observations_per_canonical_run": len(queries) * CANONICAL_PASSES,
                "chunks_per_document": chunks,
                "expected_passages": EXPECTED_PASSAGES,
                "corpus_fingerprint_sha256": corpus_fingerprint(queries),
            },
            sort_keys=True,
        )
    )
    return 0


def parser() -> argparse.ArgumentParser:
    command = argparse.ArgumentParser(description=__doc__)
    subcommands = command.add_subparsers(dest="command", required=True)

    validate_parser = subcommands.add_parser("validate", help="validate frozen inputs")
    validate_parser.add_argument("--queries", type=Path, default=QUERY_PATH)
    validate_parser.set_defaults(handler=validate_command)

    prepare_parser = subcommands.add_parser("prepare", help="build and embed the 100k corpus")
    prepare_parser.add_argument("--hsum", type=Path, default=Path("target/release/hsum"))
    prepare_parser.add_argument("--corpus", type=Path, required=True)
    prepare_parser.add_argument("--home", type=Path, required=True)
    prepare_parser.add_argument("--model-id", required=True)
    prepare_parser.add_argument("--index", default=INDEX_NAME)
    prepare_parser.add_argument("--queries", type=Path, default=QUERY_PATH)
    prepare_parser.add_argument("--output", type=Path)
    prepare_parser.set_defaults(handler=prepare)

    run_parser = subcommands.add_parser("run", help="run cold and warm qualification")
    run_parser.add_argument("--hsum", type=Path, default=Path("target/release/hsum"))
    run_parser.add_argument("--corpus", type=Path, required=True)
    run_parser.add_argument("--home", type=Path, required=True)
    run_parser.add_argument("--index", default=INDEX_NAME)
    run_parser.add_argument("--queries", type=Path, default=QUERY_PATH)
    run_parser.add_argument("--output", type=Path, required=True)
    run_parser.add_argument("--setup-report", type=Path)
    run_parser.add_argument("--cold-cache-helper", type=Path)
    run_parser.add_argument("--allow-missing-cold-cache", action="store_true")
    run_parser.add_argument("--runs", type=int, default=CANONICAL_RUNS)
    run_parser.add_argument("--warmups", type=int, default=CANONICAL_WARMUPS)
    run_parser.add_argument("--passes", type=int, default=CANONICAL_PASSES)
    run_parser.set_defaults(handler=run_benchmark)
    return command


def main() -> int:
    args = parser().parse_args()
    try:
        return args.handler(args)
    except HarnessError as error:
        print(f"retrieval-scale: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
