#!/usr/bin/env python3
"""Verify two-target cargo-binstall smoke evidence and provenance."""

from __future__ import annotations

import argparse
import json
import pathlib
import re
from typing import Any


TARGETS = {"aarch64-apple-darwin", "x86_64-unknown-linux-gnu"}
SHA256 = re.compile(r"^[0-9a-f]{64}$")
COMMIT_SHA = re.compile(r"^[0-9a-f]{40}$")


class EvidenceError(RuntimeError):
    """The cargo-binstall evidence is incomplete, ambiguous, or mismatched."""


def verify(
    evidence_root: pathlib.Path,
    source_sha: str,
    run_id: str,
    tool_version: str,
    package_version: str,
) -> dict[str, Any]:
    if COMMIT_SHA.fullmatch(source_sha) is None:
        raise EvidenceError("expected source SHA must be 40 lowercase hex characters")
    reports = sorted(evidence_root.glob("**/report.json"))
    if len(reports) != len(TARGETS):
        raise EvidenceError(
            f"expected exactly {len(TARGETS)} cargo-binstall reports, observed {len(reports)}"
        )

    observed: dict[str, dict[str, Any]] = {}
    for path in reports:
        try:
            report = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
            raise EvidenceError(f"could not read {path}: {error}") from error
        target = report.get("target")
        if target not in TARGETS or target in observed:
            raise EvidenceError(f"unexpected or duplicate cargo-binstall target: {target!r}")
        expected = {
            "schema_version": "hsum.binstall-smoke.v1",
            "passed": True,
            "target": target,
            "package_version": package_version,
            "binstall_version": tool_version,
            "source_commit_sha": source_sha,
            "source_state": "github-clean-checkout",
            "github_run_id": run_id,
            "strategy": "crate-meta-data",
            "compile_fallback_used": False,
            "quick_install_used": False,
            "telemetry_disabled": True,
            "installed_binary": "hsum",
            "internal_xtask_absent": True,
        }
        for key, value in expected.items():
            if report.get(key) != value:
                raise EvidenceError(
                    f"{path}: {key} must be {value!r}, observed {report.get(key)!r}"
                )
        digest = report.get("installed_sha256")
        if not isinstance(digest, str) or SHA256.fullmatch(digest) is None:
            raise EvidenceError(f"{path}: installed_sha256 is not canonical")
        observed[target] = report

    if set(observed) != TARGETS:
        raise EvidenceError(f"cargo-binstall targets do not match {sorted(TARGETS)}")
    return {
        "schema_version": "hsum.binstall-evidence-verification.v1",
        "passed": True,
        "targets": sorted(observed),
        "source_commit_sha": source_sha,
        "github_run_id": run_id,
        "binstall_version": tool_version,
        "package_version": package_version,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evidence-root", type=pathlib.Path, required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--tool-version", required=True)
    parser.add_argument("--package-version", required=True)
    args = parser.parse_args()
    report = verify(
        args.evidence_root,
        args.source_sha,
        args.run_id,
        args.tool_version,
        args.package_version,
    )
    print(json.dumps(report, sort_keys=True, separators=(",", ":")))


if __name__ == "__main__":
    main()
