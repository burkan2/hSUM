#!/usr/bin/env python3
"""Run the frozen hSUM citation round trip through pinned real MCP clients."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import select
import subprocess
import tempfile
import time
from typing import Any


BENCH_DIR = pathlib.Path(__file__).resolve().parent
REPOSITORY_ROOT = BENCH_DIR.parents[1]
DEFAULT_MANIFEST = BENCH_DIR / "manifest.json"
DEFAULT_CHECKSUM = BENCH_DIR / "manifest.sha256"
DEFAULT_CODEX = pathlib.Path("/Applications/ChatGPT.app/Contents/Resources/codex")
DEFAULT_CLAUDE = pathlib.Path("/Users/b.k./.local/bin/claude")
RAW_EVIDENCE_NAMES = (
    "answer.schema.json",
    "codex-answer.json",
    "codex.jsonl",
    "codex.stderr",
    "claude-mcp.json",
    "claude.jsonl",
    "claude.stderr",
)


class QualificationError(RuntimeError):
    """A pinned client or citation-round-trip requirement was not satisfied."""


def canonical_json(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_manifest(path: pathlib.Path, checksum_path: pathlib.Path) -> dict[str, Any]:
    expected_fields = checksum_path.read_text(encoding="utf-8").split()
    if len(expected_fields) != 2 or expected_fields[1] != "manifest.json":
        raise QualificationError("manifest checksum file must name manifest.json")
    actual = sha256(path)
    if actual != expected_fields[0]:
        raise QualificationError(
            f"manifest checksum mismatch: expected {expected_fields[0]}, observed {actual}"
        )
    manifest = json.loads(path.read_text(encoding="utf-8"))
    if manifest.get("schema_version") != "hsum.client-dogfood-manifest.v1":
        raise QualificationError("unsupported client dogfood manifest schema")
    if manifest.get("required_tools") != ["evidence_search", "evidence_get"]:
        raise QualificationError("manifest must freeze the search/get tool sequence")
    acceptance = manifest.get("acceptance", {})
    if acceptance.get("persistent_client_configuration_changes_allowed") is not False:
        raise QualificationError("persistent client configuration must remain forbidden")
    return manifest


def run_process(
    command: list[str],
    *,
    cwd: pathlib.Path,
    env: dict[str, str],
    timeout: float,
    input_text: str | None = None,
) -> dict[str, Any]:
    started = time.perf_counter_ns()
    completed = subprocess.run(
        command,
        cwd=cwd,
        env=env,
        input=input_text,
        text=True,
        capture_output=True,
        timeout=timeout,
        check=False,
    )
    return {
        "returncode": completed.returncode,
        "stdout": completed.stdout,
        "stderr": completed.stderr,
        "elapsed_ms": (time.perf_counter_ns() - started) / 1_000_000,
    }


def require_success(name: str, observation: dict[str, Any]) -> None:
    if observation["returncode"] != 0:
        diagnostic = observation["stderr"] or observation["stdout"]
        raise QualificationError(
            f"{name} exited {observation['returncode']}: {diagnostic[-4000:]}"
        )


def parse_jsonl(output: str, name: str) -> list[dict[str, Any]]:
    events = []
    for line_number, line in enumerate(output.splitlines(), start=1):
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError as error:
            raise QualificationError(
                f"{name} emitted invalid JSONL on line {line_number}: {error}"
            ) from error
        if not isinstance(event, dict):
            raise QualificationError(f"{name} JSONL event {line_number} is not an object")
        events.append(event)
    if not events:
        raise QualificationError(f"{name} emitted no JSONL events")
    return events


def codex_tool_events(events: list[dict[str, Any]]) -> list[dict[str, Any]]:
    tools = []
    for event in events:
        item = event.get("item")
        if not isinstance(item, dict) or item.get("type") != "mcp_tool_call":
            continue
        tool = item.get("tool") or item.get("name") or item.get("tool_name")
        server = item.get("server") or item.get("server_name")
        tools.append({"server": server, "tool": tool, "event": event})
    return tools


def claude_tool_events(events: list[dict[str, Any]]) -> list[dict[str, Any]]:
    tools = []
    results: dict[str, dict[str, Any]] = {}
    for event in events:
        message = event.get("message")
        if not isinstance(message, dict) or not isinstance(message.get("content"), list):
            continue
        for block in message["content"]:
            if not isinstance(block, dict) or block.get("type") != "tool_result":
                continue
            tool_use_id = block.get("tool_use_id")
            if isinstance(tool_use_id, str):
                results[tool_use_id] = {"block": block, "event": event}
    for event in events:
        message = event.get("message")
        if not isinstance(message, dict):
            continue
        content = message.get("content")
        if not isinstance(content, list):
            continue
        for block in content:
            if not isinstance(block, dict) or block.get("type") != "tool_use":
                continue
            name = block.get("name")
            if not isinstance(name, str) or not name.startswith("mcp__hsum__"):
                continue
            tool_use_id = block.get("id")
            tools.append({
                "server": "hsum",
                "tool": name.removeprefix("mcp__hsum__"),
                "event": {
                    "tool_use": {"block": block, "event": event},
                    "tool_result": results.get(tool_use_id) if isinstance(tool_use_id, str) else None,
                },
            })
    return tools


def require_tool_round_trip(
    name: str,
    tools: list[dict[str, Any]],
    token: str,
) -> list[str]:
    names = [str(tool["tool"]) for tool in tools]
    try:
        search_index = names.index("evidence_search")
        get_index = names.index("evidence_get", search_index + 1)
    except ValueError as error:
        raise QualificationError(
            f"{name} did not emit ordered evidence_search/evidence_get events: {names}"
        ) from error
    get_wire = canonical_json(tools[get_index]["event"])
    if token not in get_wire:
        raise QualificationError(f"{name} get event did not contain the fixture token")
    return names


def structured_answer_schema(token: str) -> dict[str, Any]:
    return {
        "type": "object",
        "additionalProperties": False,
        "properties": {
            "status": {"const": "ok"},
            "token": {"const": token},
            "citation_uri": {"type": "string", "pattern": "^hsum://"},
        },
        "required": ["status", "token", "citation_uri"],
    }


def validate_answer(answer: Any, token: str, name: str) -> dict[str, Any]:
    if isinstance(answer, str):
        try:
            answer = json.loads(answer)
        except json.JSONDecodeError as error:
            raise QualificationError(f"{name} final answer is not JSON") from error
    if not isinstance(answer, dict):
        raise QualificationError(f"{name} final answer is not an object")
    if answer.get("status") != "ok" or answer.get("token") != token:
        raise QualificationError(f"{name} final answer did not confirm the fixture token")
    citation = answer.get("citation_uri")
    if not isinstance(citation, str) or not citation.startswith("hsum://"):
        raise QualificationError(f"{name} final answer omitted an hsum citation")
    return answer


def generic_round_trip(
    hsum: pathlib.Path,
    repository: pathlib.Path,
    env: dict[str, str],
    manifest: dict[str, Any],
) -> dict[str, Any]:
    process = subprocess.Popen(
        [str(hsum), "--no-color", "--no-progress", "mcp"],
        cwd=repository,
        env=env,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )

    def send(frame: dict[str, Any]) -> None:
        assert process.stdin is not None
        process.stdin.write(canonical_json(frame) + "\n")
        process.stdin.flush()

    def receive(timeout: float = 15) -> dict[str, Any]:
        assert process.stdout is not None
        readable, _, _ = select.select([process.stdout], [], [], timeout)
        if not readable:
            raise QualificationError("generic MCP client timed out")
        line = process.stdout.readline()
        if not line:
            stderr = process.stderr.read() if process.stderr else ""
            raise QualificationError(f"generic MCP server disconnected: {stderr}")
        return json.loads(line)

    def call(request_id: int, tool: str, arguments: dict[str, Any]) -> dict[str, Any]:
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "tools/call",
            "params": {"name": tool, "arguments": arguments},
        })
        response = receive()
        if response.get("id") != request_id or "error" in response:
            raise QualificationError(f"generic MCP {tool} failed: {response}")
        return response["result"]["structuredContent"]

    token = manifest["fixture_token"]
    protocol_version = manifest["clients"]["generic_mcp"]["protocol_version"]
    send({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": protocol_version,
            "capabilities": {},
            "clientInfo": {"name": "hsum-client-dogfood", "version": "1"},
        },
    })
    initialized = receive()
    if initialized.get("result", {}).get("serverInfo", {}).get("name") != "hsum":
        raise QualificationError(f"generic MCP initialize failed: {initialized}")
    send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    search = call(2, "evidence_search", {
        "query": token,
        "mode": "lexical",
        "limit": 1,
        "timeout_ms": 3_000,
        "explain": True,
    })
    citation = search["results"][0]["citation_uri"]
    get = call(3, "evidence_get", {
        "citation_uri": citation,
        "verify_source_hash": True,
    })
    if token not in get["content"] or get["source_hash_verification"] != "unchanged":
        raise QualificationError("generic MCP get did not verify the fixture bytes")
    assert process.stdin is not None
    process.stdin.close()
    if process.wait(timeout=5) != 0:
        raise QualificationError("generic MCP server did not exit cleanly")
    stderr = process.stderr.read() if process.stderr else ""
    if stderr:
        raise QualificationError(f"generic MCP server wrote stderr: {stderr}")
    return {
        "passed": True,
        "protocol_version": protocol_version,
        "tool_events": ["evidence_search", "evidence_get"],
        "citation_uri": citation,
        "source_hash_verification": get["source_hash_verification"],
    }


def prompt_for(token: str) -> str:
    return (
        "Use only the hsum MCP tools. Call evidence_search for the exact query "
        f"{token!r} in lexical mode with limit 1. Then call evidence_get with the "
        "returned citation_uri and verify_source_hash true. Inspect the actual get "
        "content. Return the required JSON object only after the content contains the "
        "exact query token. Do not use shell, filesystem, or source-control tools."
    )


def run_codex(
    codex: pathlib.Path,
    hsum: pathlib.Path,
    repository: pathlib.Path,
    env: dict[str, str],
    manifest: dict[str, Any],
    root: pathlib.Path,
    timeout: float,
) -> dict[str, Any]:
    token = manifest["fixture_token"]
    schema_path = root / "answer.schema.json"
    answer_path = root / "codex-answer.json"
    original_codex_home = pathlib.Path(
        os.environ.get("CODEX_HOME", pathlib.Path.home() / ".codex")
    ).resolve()
    auth_source = original_codex_home / "auth.json"
    if not auth_source.is_file():
        raise QualificationError(f"Codex authentication is unavailable: {auth_source}")
    isolated_home = root / "codex-user-home"
    isolated_codex_home = isolated_home / ".codex"
    isolated_codex_home.mkdir(parents=True)
    (isolated_codex_home / "auth.json").symlink_to(auth_source)
    codex_env = env.copy()
    codex_env["HOME"] = str(isolated_home)
    codex_env["CODEX_HOME"] = str(isolated_codex_home)
    schema_path.write_text(canonical_json(structured_answer_schema(token)) + "\n", encoding="utf-8")
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
        str(repository),
        "--output-schema",
        str(schema_path),
        "--output-last-message",
        str(answer_path),
        "--config",
        f"mcp_servers.hsum.command={json.dumps(str(hsum))}",
        "--config",
        'mcp_servers.hsum.args=["mcp"]',
        "--config",
        "mcp_servers.hsum.required=true",
        "-",
    ]
    observation = run_process(
        command,
        cwd=repository,
        env=codex_env,
        timeout=timeout,
        input_text=prompt_for(token),
    )
    (root / "codex.jsonl").write_text(observation["stdout"], encoding="utf-8")
    (root / "codex.stderr").write_text(observation["stderr"], encoding="utf-8")
    events = parse_jsonl(observation["stdout"], "Codex")
    if observation["returncode"] != 0:
        failures = [
            event.get("error", {}).get("message") or event.get("message")
            for event in events
            if event.get("type") in {"error", "turn.failed"}
        ]
        message = next((value for value in reversed(failures) if value), "unknown failure")
        raise QualificationError(f"Codex turn failed: {message}")
    require_success("Codex", observation)
    tools = codex_tool_events(events)
    tool_names = require_tool_round_trip("Codex", tools, token)
    answer = validate_answer(answer_path.read_text(encoding="utf-8"), token, "Codex")
    return {
        "passed": True,
        "elapsed_ms": observation["elapsed_ms"],
        "event_count": len(events),
        "tool_events": tool_names,
        "answer": answer,
    }


def claude_result(events: list[dict[str, Any]]) -> tuple[Any, dict[str, Any]]:
    results = [event for event in events if event.get("type") == "result"]
    if len(results) != 1:
        raise QualificationError(f"Claude emitted {len(results)} result events")
    result = results[0]
    answer = result.get("structured_output")
    if answer is None:
        answer = result.get("result")
    return answer, result


def run_claude(
    claude: pathlib.Path,
    hsum: pathlib.Path,
    repository: pathlib.Path,
    env: dict[str, str],
    manifest: dict[str, Any],
    root: pathlib.Path,
    timeout: float,
) -> dict[str, Any]:
    token = manifest["fixture_token"]
    schema = structured_answer_schema(token)
    config_path = root / "claude-mcp.json"
    config_path.write_text(canonical_json({
        "mcpServers": {"hsum": {"command": str(hsum), "args": ["mcp"]}}
    }) + "\n", encoding="utf-8")
    command = [
        str(claude),
        "--print",
        "--output-format",
        "stream-json",
        "--verbose",
        "--mcp-config",
        str(config_path),
        "--strict-mcp-config",
        "--allowedTools",
        "mcp__hsum__evidence_search,mcp__hsum__evidence_get",
        "--no-session-persistence",
        "--setting-sources",
        "project",
        "--disable-slash-commands",
        "--no-chrome",
        "--max-budget-usd",
        str(manifest["clients"]["claude_code"]["max_budget_usd"]),
        "--json-schema",
        canonical_json(schema),
        prompt_for(token),
    ]
    observation = run_process(command, cwd=repository, env=env, timeout=timeout)
    (root / "claude.jsonl").write_text(observation["stdout"], encoding="utf-8")
    (root / "claude.stderr").write_text(observation["stderr"], encoding="utf-8")
    events = parse_jsonl(observation["stdout"], "Claude Code")
    answer_raw, result = claude_result(events)
    if result.get("is_error") is True:
        raise QualificationError(f"Claude Code result error: {result.get('result')}")
    require_success("Claude Code", observation)
    tools = claude_tool_events(events)
    tool_names = require_tool_round_trip("Claude Code", tools, token)
    answer = validate_answer(answer_raw, token, "Claude Code")
    return {
        "passed": True,
        "elapsed_ms": observation["elapsed_ms"],
        "event_count": len(events),
        "tool_events": tool_names,
        "answer": answer,
        "total_cost_usd": result.get("total_cost_usd"),
    }


def require_version(
    name: str,
    executable: pathlib.Path,
    expected: str,
    env: dict[str, str],
) -> str:
    observation = run_process(
        [str(executable), "--version"],
        cwd=REPOSITORY_ROOT,
        env=env,
        timeout=30,
    )
    require_success(name, observation)
    actual = observation["stdout"].strip()
    if actual != expected:
        raise QualificationError(f"{name} version mismatch: expected {expected!r}, observed {actual!r}")
    return actual


def execute(args: argparse.Namespace, manifest: dict[str, Any]) -> dict[str, Any]:
    hsum = args.hsum.resolve()
    codex = args.codex.resolve()
    claude = args.claude.resolve()
    output = args.output.resolve()
    for name, path in (("hSUM", hsum), ("Codex", codex), ("Claude Code", claude)):
        if not path.is_file() or not os.access(path, os.X_OK):
            raise QualificationError(f"{name} executable is unavailable: {path}")
    output.parent.mkdir(parents=True, exist_ok=True)
    evidence_destinations = [
        output.with_name(f"{output.stem}-{name}") for name in RAW_EVIDENCE_NAMES
    ]
    existing = [path for path in (output, *evidence_destinations) if path.exists()]
    if existing:
        raise QualificationError(
            "dogfood evidence paths are write-once; choose a new output path: "
            + ", ".join(str(path) for path in existing)
        )

    with tempfile.TemporaryDirectory(prefix="hsum-client-dogfood-") as directory:
        root = pathlib.Path(directory)
        repository = root / "repository"
        repository.mkdir()
        (repository / "README.md").write_text(
            "# hSUM client dogfood fixture\n\n"
            f"The immutable qualification token is {manifest['fixture_token']}.\n",
            encoding="utf-8",
        )
        subprocess.run(["git", "init", "--quiet", str(repository)], check=True)
        env = os.environ.copy()
        env["HSUM_HOME"] = str(root / "hsum-home")
        env["HSUM_OFFLINE"] = "1"
        init = run_process(
            [str(hsum), "--no-color", "--no-progress", "init"],
            cwd=repository,
            env=env,
            timeout=args.timeout,
        )
        require_success("hSUM init", init)

        clients: dict[str, Any] = {}
        failures: dict[str, str] = {}
        started = time.perf_counter_ns()
        try:
            try:
                clients["generic_mcp"] = generic_round_trip(
                    hsum, repository, env, manifest
                )
            except (QualificationError, subprocess.TimeoutExpired) as error:
                failures["generic_mcp"] = str(error)
                clients["generic_mcp"] = {"passed": False, "error": str(error)}

            try:
                codex_version = require_version(
                    "Codex", codex, manifest["clients"]["codex"]["version"], env
                )
                clients["codex"] = {
                    "version": codex_version,
                    "executable_sha256": sha256(codex),
                    "configuration": manifest["clients"]["codex"]["configuration"],
                    **run_codex(
                        codex, hsum, repository, env, manifest, root, args.timeout
                    ),
                }
            except (QualificationError, subprocess.TimeoutExpired) as error:
                failures["codex"] = str(error)
                clients["codex"] = {
                    "passed": False,
                    "expected_version": manifest["clients"]["codex"]["version"],
                    "executable_sha256": sha256(codex),
                    "configuration": manifest["clients"]["codex"]["configuration"],
                    "error": str(error),
                }

            try:
                claude_version = require_version(
                    "Claude Code",
                    claude,
                    manifest["clients"]["claude_code"]["version"],
                    env,
                )
                clients["claude_code"] = {
                    "version": claude_version,
                    "executable_sha256": sha256(claude),
                    "configuration": manifest["clients"]["claude_code"]["configuration"],
                    **run_claude(
                        claude, hsum, repository, env, manifest, root, args.timeout
                    ),
                }
            except (QualificationError, subprocess.TimeoutExpired) as error:
                failures["claude_code"] = str(error)
                clients["claude_code"] = {
                    "passed": False,
                    "expected_version": manifest["clients"]["claude_code"]["version"],
                    "executable_sha256": sha256(claude),
                    "configuration": manifest["clients"]["claude_code"]["configuration"],
                    "error": str(error),
                }
            elapsed_ms = (time.perf_counter_ns() - started) / 1_000_000
        finally:
            for evidence_name in RAW_EVIDENCE_NAMES:
                source = root / evidence_name
                if source.is_file():
                    output.with_name(f"{output.stem}-{evidence_name}").write_bytes(
                        source.read_bytes()
                    )

    report = {
        "schema_version": "hsum.client-dogfood-report.v1",
        "passed": not failures and all(
            clients.get(name, {}).get("passed") is True
            for name in ("generic_mcp", "codex", "claude_code")
        ),
        "protocol_id": manifest["protocol_id"],
        "manifest_sha256": sha256(args.manifest.resolve()),
        "candidate": {
            "version": run_process(
                [str(hsum), "--version"],
                cwd=REPOSITORY_ROOT,
                env=os.environ.copy(),
                timeout=30,
            )["stdout"].strip(),
            "sha256": sha256(hsum),
        },
        "clients": clients,
        "failures": failures,
        "persistent_client_configuration_changed": False,
        "total_elapsed_ms": elapsed_ms,
    }
    output.write_text(canonical_json(report) + "\n", encoding="utf-8")
    return report


def parser() -> argparse.ArgumentParser:
    argument_parser = argparse.ArgumentParser(description=__doc__)
    argument_parser.add_argument("--manifest", type=pathlib.Path, default=DEFAULT_MANIFEST)
    argument_parser.add_argument("--checksum", type=pathlib.Path, default=DEFAULT_CHECKSUM)
    subparsers = argument_parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("validate", help="validate the checksum-pinned protocol only")
    run = subparsers.add_parser("run", help="invoke all three pinned clients")
    run.add_argument("--hsum", type=pathlib.Path, required=True)
    run.add_argument("--codex", type=pathlib.Path, default=DEFAULT_CODEX)
    run.add_argument("--claude", type=pathlib.Path, default=DEFAULT_CLAUDE)
    run.add_argument("--output", type=pathlib.Path, required=True)
    run.add_argument("--timeout", type=float, default=300.0)
    return argument_parser


def main() -> None:
    args = parser().parse_args()
    manifest = load_manifest(args.manifest.resolve(), args.checksum.resolve())
    if args.command == "validate":
        print(canonical_json({
            "status": "valid",
            "protocol_id": manifest["protocol_id"],
            "manifest_sha256": sha256(args.manifest.resolve()),
        }))
        return
    report = execute(args, manifest)
    print(canonical_json({
        "status": "passed" if report["passed"] else "incomplete",
        "output": str(args.output.resolve()),
        "clients": sorted(report["clients"]),
    }))
    if not report["passed"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
