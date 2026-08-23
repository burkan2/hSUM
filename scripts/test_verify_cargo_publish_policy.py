#!/usr/bin/env python3

import pathlib
import subprocess
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("verify-cargo-publish-policy.sh")


def verify(manifest_text: str) -> subprocess.CompletedProcess[str]:
    with tempfile.TemporaryDirectory() as directory:
        manifest = pathlib.Path(directory) / "Cargo.toml"
        manifest.write_text(manifest_text, encoding="utf-8")
        return subprocess.run(
            ["bash", str(SCRIPT), str(manifest)],
            text=True,
            capture_output=True,
            check=False,
        )


class VerifyCargoPublishPolicyTests(unittest.TestCase):
    def test_accepts_the_exact_crates_io_allowlist(self) -> None:
        result = verify('[package]\nname = "example"\npublish = ["crates-io"]\n')

        self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_disabled_or_multiple_registries(self) -> None:
        for policy in ('false', '["crates-io", "private"]'):
            with self.subTest(policy=policy):
                result = verify(f'[package]\nname = "example"\npublish = {policy}\n')
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("[package].publish must be exactly", result.stderr)

    def test_rejects_a_misleading_line_outside_package_metadata(self) -> None:
        result = verify(
            '[package]\nname = "example"\npublish = false\n'
            '\n[package.metadata.example]\npublish = ["crates-io"]\n'
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("[package].publish must be exactly", result.stderr)


if __name__ == "__main__":
    unittest.main()
