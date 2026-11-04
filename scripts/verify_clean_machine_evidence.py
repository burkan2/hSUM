#!/usr/bin/env python3
"""Verify that both native clean-machine reports bind to one exact candidate."""

from __future__ import annotations

import argparse
import json
import pathlib
import re
from typing import NamedTuple


SHA1 = re.compile(r"[0-9a-f]{40}\Z")
SHA256 = re.compile(r"[0-9a-f]{64}\Z")


class ExpectedProvenance(NamedTuple):
    checkout_sha: str
    source_sha: str
    base_sha: str
    tree_sha: str
    run_id: str


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def verify_reports(reports: list[dict], expected: ExpectedProvenance) -> None:
    require(len(reports) == 2, "expected exactly two native reports")
    require(
        {
            (report.get("platform", {}).get("system"), report.get("platform", {}).get("machine"))
            for report in reports
        }
        == {("Linux", "x86_64"), ("Darwin", "arm64")},
        "reports must cover native Linux x86_64 and Darwin arm64",
    )

    expected_fields = {
        "commit_sha": expected.checkout_sha,
        "checkout_commit_sha": expected.checkout_sha,
        "source_commit_sha": expected.source_sha,
        "base_commit_sha": expected.base_sha,
        "tree_sha": expected.tree_sha,
    }
    for field, value in expected_fields.items():
        require(SHA1.fullmatch(value) is not None, f"expected {field} is not a SHA-1")

    versions: set[str] = set()
    for report in reports:
        machine = report["platform"]["machine"]
        require(
            report.get("schema_version") == "hsum.clean-machine-trials.v1",
            f"{machine}: unexpected schema_version",
        )
        require(report.get("passed") is True, f"{machine}: report did not pass")

        candidate = report.get("candidate", {})
        for field, value in expected_fields.items():
            require(
                candidate.get(field) == value,
                f"{machine}: candidate {field} does not match expected provenance",
            )
        require(
            SHA256.fullmatch(candidate.get("sha256", "")) is not None,
            f"{machine}: candidate sha256 is invalid",
        )
        version = candidate.get("version")
        require(isinstance(version, str) and version.startswith("hsum "), f"{machine}: bad version")
        versions.add(version)

        github = report.get("github", {})
        require(github.get("run_id") == expected.run_id, f"{machine}: run_id mismatch")
        require(github.get("run_attempt") not in (None, ""), f"{machine}: missing run_attempt")

        protocol = report.get("protocol", {})
        require(protocol.get("trial_count") == 5, f"{machine}: trial_count is not five")
        require(protocol.get("network") == "disabled", f"{machine}: network was not disabled")
        for field in (
            "fresh_repository_per_trial",
            "fresh_hsum_home_per_trial",
            "cli_citation_round_trip",
            "mcp_search_get_status_project_round_trip",
        ):
            require(protocol.get(field) is True, f"{machine}: protocol {field} did not pass")

        trials = report.get("trials", [])
        require(len(trials) == 5, f"{machine}: expected five trials")
        require(
            [trial.get("trial") for trial in trials] == [1, 2, 3, 4, 5],
            f"{machine}: trial sequence is incomplete",
        )
        require(all(trial.get("passed") is True for trial in trials), f"{machine}: a trial failed")

        summary = report.get("summary", {})
        require(summary.get("failure_reasons") == [], f"{machine}: failure reasons are present")
        require(
            summary.get("first_cli_citation_seconds", {}).get("target_passed") is True,
            f"{machine}: first CLI citation target failed",
        )
        require(
            summary.get("mcp_round_trip_seconds", {}).get("target_passed") is True,
            f"{machine}: MCP round-trip target failed",
        )

    require(len(versions) == 1, "native reports identify different hSUM versions")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--evidence-root", type=pathlib.Path, required=True)
    parser.add_argument("--checkout-sha", required=True)
    parser.add_argument("--source-sha", required=True)
    parser.add_argument("--base-sha", required=True)
    parser.add_argument("--tree-sha", required=True)
    parser.add_argument("--run-id", required=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    report_paths = sorted(args.evidence_root.glob("*/report.json"))
    reports = [json.loads(path.read_text(encoding="utf-8")) for path in report_paths]
    verify_reports(
        reports,
        ExpectedProvenance(
            checkout_sha=args.checkout_sha,
            source_sha=args.source_sha,
            base_sha=args.base_sha,
            tree_sha=args.tree_sha,
            run_id=args.run_id,
        ),
    )
    print("clean-machine provenance and five-trial evidence verified")


if __name__ == "__main__":
    main()
