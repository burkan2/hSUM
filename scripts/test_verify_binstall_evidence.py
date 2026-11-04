#!/usr/bin/env python3
"""Regression tests for native cargo-binstall evidence verification."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

import verify_binstall_evidence as contract


SOURCE_SHA = "a" * 40
RUN_ID = "123456"
TOOL_VERSION = "1.22.0"
PACKAGE_VERSION = "0.1.0-alpha.4"


def write_report(root: Path, target: str, **changes: object) -> None:
    report = {
        "schema_version": "hsum.binstall-smoke.v1",
        "passed": True,
        "target": target,
        "package_version": PACKAGE_VERSION,
        "binstall_version": TOOL_VERSION,
        "installed_sha256": "b" * 64,
        "source_commit_sha": SOURCE_SHA,
        "source_state": "github-clean-checkout",
        "github_run_id": RUN_ID,
        "strategy": "crate-meta-data",
        "compile_fallback_used": False,
        "quick_install_used": False,
        "telemetry_disabled": True,
        "installed_binary": "hsum",
        "internal_xtask_absent": True,
    }
    report.update(changes)
    report_dir = root / target
    report_dir.mkdir(parents=True)
    (report_dir / "report.json").write_text(
        json.dumps(report), encoding="utf-8"
    )


def verify(root: Path) -> dict[str, object]:
    return contract.verify(root, SOURCE_SHA, RUN_ID, TOOL_VERSION, PACKAGE_VERSION)


class BinstallEvidenceTests(unittest.TestCase):
    def test_accepts_two_complete_provenance_bound_reports(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for target in contract.TARGETS:
                write_report(root, target)

            result = verify(root)

        self.assertTrue(result["passed"])
        self.assertEqual(result["targets"], sorted(contract.TARGETS))

    def test_rejects_missing_or_duplicate_targets(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            write_report(root, "aarch64-apple-darwin")
            with self.assertRaisesRegex(contract.EvidenceError, "exactly 2"):
                verify(root)

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for target in contract.TARGETS:
                write_report(root, target)
            duplicate = root / "duplicate"
            duplicate.mkdir()
            (duplicate / "report.json").write_text(
                (root / "aarch64-apple-darwin" / "report.json").read_text(),
                encoding="utf-8",
            )
            with self.assertRaisesRegex(contract.EvidenceError, "exactly 2"):
                verify(root)

    def test_rejects_provenance_strategy_or_binary_drift(self) -> None:
        cases = {
            "source_commit_sha": "c" * 40,
            "source_state": "local-working-tree",
            "github_run_id": "654321",
            "strategy": "compile",
            "compile_fallback_used": True,
            "quick_install_used": True,
            "installed_binary": "xtask",
            "installed_sha256": "not-a-digest",
        }
        for key, value in cases.items():
            with self.subTest(key=key), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                for target in contract.TARGETS:
                    changes = {key: value} if target == "aarch64-apple-darwin" else {}
                    write_report(root, target, **changes)
                with self.assertRaises(contract.EvidenceError):
                    verify(root)


if __name__ == "__main__":
    unittest.main()
