#!/usr/bin/env python3
"""Regression tests for the README release-contract verifier."""

from __future__ import annotations

import unittest
from pathlib import Path

import verify_readme_contract as contract


ROOT = Path(__file__).resolve().parents[1]
README = (ROOT / "README.md").read_text(encoding="utf-8")
CARGO_TOML = (ROOT / "Cargo.toml").read_text(encoding="utf-8")


class ReadmeContractTests(unittest.TestCase):
    def test_current_readme_passes(self) -> None:
        report = contract.verify(README, CARGO_TOML)
        self.assertTrue(report["passed"])
        self.assertGreaterEqual(report["capability_count"], 20)

    def test_uncontrolled_status_is_rejected(self) -> None:
        changed = README.replace(
            "| Local filesystem ingest | available |",
            "| Local filesystem ingest | generally available |",
        )
        with self.assertRaisesRegex(contract.ContractError, "invalid status"):
            contract.verify(changed, CARGO_TOML)

    def test_hybrid_cannot_be_promoted(self) -> None:
        changed = README.replace(
            "| Semantic and hybrid vector retrieval | beta |",
            "| Semantic and hybrid vector retrieval | available |",
        )
        with self.assertRaisesRegex(contract.ContractError, "must remain 'beta'"):
            contract.verify(changed, CARGO_TOML)

    def test_omitted_mode_must_remain_stable_lexical(self) -> None:
        changed = README.replace(
            "Omitted mode defaults to stable `lexical`",
            "Omitted mode defaults to `auto`",
        )
        with self.assertRaisesRegex(contract.ContractError, "stable lexical default"):
            contract.verify(changed, CARGO_TOML)

    def test_privacy_boundary_must_precede_client_command(self) -> None:
        changed = README.replace("Privacy boundary:", "Privacy note:", 1)
        with self.assertRaisesRegex(contract.ContractError, "privacy boundary"):
            contract.verify(changed, CARGO_TOML)

    def test_quickstart_version_must_match_cargo(self) -> None:
        changed = README.replace(
            "Install and smoke-test hSUM 0.1.0-alpha.4",
            "Install and smoke-test hSUM 0.1.0-alpha.3",
        )
        with self.assertRaisesRegex(contract.ContractError, "package version"):
            contract.verify(changed, CARGO_TOML)


if __name__ == "__main__":
    unittest.main()
