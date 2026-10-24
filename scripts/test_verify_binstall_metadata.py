#!/usr/bin/env python3
"""Regression tests for cargo-binstall release metadata."""

from __future__ import annotations

import unittest
from pathlib import Path

import verify_binstall_metadata as contract


ROOT = Path(__file__).resolve().parents[1]
CARGO_TOML = (ROOT / "Cargo.toml").read_bytes()


class BinstallMetadataTests(unittest.TestCase):
    def test_current_metadata_passes(self) -> None:
        report = contract.verify(CARGO_TOML)

        self.assertTrue(report["passed"])
        self.assertEqual(
            report["targets"],
            ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"],
        )
        self.assertTrue(report["compile_fallback_preserved"])

    def test_compile_fallback_cannot_be_disabled(self) -> None:
        changed = CARGO_TOML.replace(
            b'disabled-strategies = ["quick-install"]',
            b'disabled-strategies = ["quick-install", "compile"]',
        )

        with self.assertRaisesRegex(contract.MetadataError, "preserve the compile"):
            contract.verify(changed)

    def test_release_filename_or_target_drift_is_rejected(self) -> None:
        changed_name = CARGO_TOML.replace(
            b'{ name }-v{ version }-{ target }.zip',
            b'{ name }-{ target }-v{ version }.zip',
        )
        with self.assertRaisesRegex(contract.MetadataError, "pkg-url"):
            contract.verify(changed_name)

        changed_target = CARGO_TOML.replace(
            b"[package.metadata.binstall.overrides.aarch64-apple-darwin]\n",
            b"",
        )
        with self.assertRaisesRegex(contract.MetadataError, "cover exactly"):
            contract.verify(changed_target)


if __name__ == "__main__":
    unittest.main()
