#!/usr/bin/env python3
"""Regression tests for the canonical completion-ledger verifier."""

from __future__ import annotations

import unittest
from pathlib import Path

import verify_completion_ledger as contract


ROOT = Path(__file__).resolve().parents[1]
LEDGER = (ROOT / "outputs/STABLE_V0_1_COMPLETION_LEDGER.md").read_text(
    encoding="utf-8"
)


class CompletionLedgerTests(unittest.TestCase):
    def test_current_ledger_passes(self) -> None:
        report = contract.verify(LEDGER)

        self.assertTrue(report["passed"])
        self.assertEqual(report["canonical_rows"], 54)
        self.assertGreater(report["earned_evidence_cells"], 0)
        self.assertEqual(report["applicable_evidence_cells"], 316)

    def test_stale_percentage_is_rejected(self) -> None:
        report = contract.verify(LEDGER)
        current = contract.percentage(
            report["earned_evidence_cells"], report["applicable_evidence_cells"]
        )
        changed = LEDGER.replace(
            f"Full stable-program evidence: **{current}**",
            "Full stable-program evidence: **0.1%**",
        )

        self.assertNotEqual(changed, LEDGER)
        with self.assertRaisesRegex(contract.LedgerError, "metric is stale"):
            contract.verify(changed)

    def test_uncontrolled_evidence_value_is_rejected(self) -> None:
        changed = LEDGER.replace(
            "| A1-01 | Pinned Rust workspace and one contributor check | Native target evidence passing | yes |",
            "| A1-01 | Pinned Rust workspace and one contributor check | Native target evidence passing | complete |",
        )

        with self.assertRaisesRegex(contract.LedgerError, "invalid evidence value"):
            contract.verify(changed)


if __name__ == "__main__":
    unittest.main()
