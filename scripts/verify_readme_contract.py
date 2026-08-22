#!/usr/bin/env python3
"""Verify the executable README quickstart and capability-policy contract."""

from __future__ import annotations

import argparse
import json
import pathlib
import re
from typing import Any


ALLOWED_STATUSES = {"available", "beta", "planned", "unsupported"}
REQUIRED_CAPABILITIES = {
    "Local filesystem ingest": "available",
    "Named projects": "available",
    "Exact and BM25 search": "available",
    "Immutable citations and historical `get`": "available",
    "MCP stdio": "available",
    "JSONL snapshot sources": "available",
    "Codex MCP client": "beta",
    "Claude Code MCP client": "beta",
    "Claude Desktop MCP client": "planned",
    "Generic MCP harness": "available",
    "macOS arm64": "available",
    "Linux x86_64": "available",
    "Other platforms": "unsupported",
    "Offline query path": "available",
    "Watch mode": "unsupported",
    "Live connectors": "unsupported",
    "Semantic and hybrid vector retrieval": "beta",
    "Reranking": "unsupported",
    "HTTP server or web UI": "unsupported",
    "Prebuilt installation": "available",
}


class ContractError(RuntimeError):
    """The README no longer matches the frozen release contract."""


def package_version(cargo_toml: str) -> str:
    match = re.search(r'^version = "([^"]+)"$', cargo_toml, flags=re.MULTILINE)
    if match is None:
        raise ContractError("Cargo.toml package version is missing")
    return match.group(1)


def section(text: str, heading: str) -> str:
    start = text.find(heading)
    if start < 0:
        raise ContractError(f"README section is missing: {heading}")
    next_heading = text.find("\n## ", start + len(heading))
    return text[start:] if next_heading < 0 else text[start:next_heading]


def capability_rows(text: str) -> dict[str, dict[str, str]]:
    header = "| Capability | Status | Current boundary |"
    start = text.find(header)
    if start < 0:
        raise ContractError("README capability table header is missing")
    lines = text[start:].splitlines()[2:]
    rows: dict[str, dict[str, str]] = {}
    for line in lines:
        if not line.startswith("|"):
            break
        fields = [field.strip() for field in line.strip().strip("|").split("|")]
        if len(fields) != 3:
            raise ContractError(f"malformed capability row: {line}")
        capability, status, boundary = fields
        if capability in rows:
            raise ContractError(f"duplicate capability row: {capability}")
        if status not in ALLOWED_STATUSES:
            raise ContractError(
                f"invalid status for {capability}: {status!r}; allowed={sorted(ALLOWED_STATUSES)}"
            )
        rows[capability] = {"status": status, "boundary": boundary}
    return rows


def verify(readme: str, cargo_toml: str) -> dict[str, Any]:
    version = package_version(cargo_toml)
    quickstart = section(readme, "## Quickstart")
    if f"Install and smoke-test hSUM {version}" not in quickstart:
        raise ContractError("agent quickstart does not pin the Cargo package version")
    for command in ("hsum search", "hsum get"):
        if command not in quickstart:
            raise ContractError(f"quickstart command is missing: {command}")

    connect = section(readme, "## Connect your agent")
    privacy = connect.find("Privacy boundary:")
    client_commands = [
        position
        for marker in ('"$HSUM" integration install', '"$HSUM" client config')
        if (position := connect.find(marker)) >= 0
    ]
    if privacy < 0 or not client_commands or privacy > min(client_commands):
        raise ContractError("privacy boundary must precede the first client command")

    rows = capability_rows(readme)
    missing = sorted(set(REQUIRED_CAPABILITIES) - set(rows))
    if missing:
        raise ContractError(f"required capability rows are missing: {missing}")
    for capability, expected in REQUIRED_CAPABILITIES.items():
        actual = rows[capability]["status"]
        if actual != expected:
            raise ContractError(
                f"{capability} must remain {expected!r}, observed {actual!r}"
            )
    hybrid_boundary = rows["Semantic and hybrid vector retrieval"]["boundary"]
    if "not the stable default claim" not in hybrid_boundary:
        raise ContractError("hybrid beta row must preserve the lexical-first disposition")
    if "MCP stdio is the only transport" not in rows["HTTP server or web UI"]["boundary"]:
        raise ContractError("transport boundary no longer names MCP stdio as the only transport")

    return {
        "schema_version": "hsum.readme-contract.v1",
        "passed": True,
        "package_version": version,
        "capability_count": len(rows),
        "allowed_statuses": sorted(ALLOWED_STATUSES),
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    repository = pathlib.Path(__file__).resolve().parents[1]
    parser.add_argument("--readme", type=pathlib.Path, default=repository / "README.md")
    parser.add_argument("--cargo-toml", type=pathlib.Path, default=repository / "Cargo.toml")
    args = parser.parse_args()
    report = verify(
        args.readme.read_text(encoding="utf-8"),
        args.cargo_toml.read_text(encoding="utf-8"),
    )
    print(json.dumps(report, sort_keys=True, separators=(",", ":")))


if __name__ == "__main__":
    main()
