#!/usr/bin/env python3

import hashlib
import pathlib
import subprocess
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("verify-release-assets.sh")
TAG = "v0.1.0-alpha.99"
CHECKSUMMED_ASSETS = (
    f"hsum-{TAG}-aarch64-apple-darwin.spdx.json",
    f"hsum-{TAG}-aarch64-apple-darwin.zip",
    f"hsum-{TAG}-cargo-licenses.json",
    f"hsum-{TAG}-x86_64-unknown-linux-gnu.spdx.json",
    f"hsum-{TAG}-x86_64-unknown-linux-gnu.tar.gz",
    "install-hsum.sh",
)


def digest(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def create_valid_draft(root: pathlib.Path) -> None:
    for index, name in enumerate(CHECKSUMMED_ASSETS, start=1):
        (root / name).write_bytes(f"asset-{index}\n".encode())

    for archive in (
        f"hsum-{TAG}-aarch64-apple-darwin.zip",
        f"hsum-{TAG}-x86_64-unknown-linux-gnu.tar.gz",
    ):
        (root / f"{archive}.sha256").write_text(
            f"{digest(root / archive)}  {archive}\n", encoding="utf-8"
        )

    (root / "SHA256SUMS").write_text(
        "".join(
            f"{digest(root / name)}  {name}\n" for name in CHECKSUMMED_ASSETS
        ),
        encoding="utf-8",
    )


def verify(tag: str, root: pathlib.Path) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", str(SCRIPT), tag, str(root)],
        text=True,
        capture_output=True,
        check=False,
    )


class VerifyReleaseAssetsTests(unittest.TestCase):
    def test_accepts_the_complete_immutable_draft(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            create_valid_draft(root)

            result = verify(TAG, root)

        self.assertEqual(result.returncode, 0, result.stderr)

    def test_rejects_missing_license_or_either_sbom_and_extra_assets(self) -> None:
        required = (
            f"hsum-{TAG}-cargo-licenses.json",
            f"hsum-{TAG}-aarch64-apple-darwin.spdx.json",
            f"hsum-{TAG}-x86_64-unknown-linux-gnu.spdx.json",
        )
        for removed in required:
            with self.subTest(removed=removed), tempfile.TemporaryDirectory() as directory:
                root = pathlib.Path(directory)
                create_valid_draft(root)
                (root / removed).unlink()
                result = verify(TAG, root)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("immutable asset contract", result.stderr)

        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            create_valid_draft(root)
            (root / "unexpected.txt").write_text("extra\n", encoding="utf-8")
            result = verify(TAG, root)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("immutable asset contract", result.stderr)

    def test_rejects_main_payload_checksum_corruption(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            create_valid_draft(root)
            (root / "install-hsum.sh").write_text("changed\n", encoding="utf-8")
            result = verify(TAG, root)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("FAILED", result.stdout + result.stderr)

    def test_rejects_archive_sidecar_checksum_corruption(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            create_valid_draft(root)
            sidecar = root / f"hsum-{TAG}-aarch64-apple-darwin.zip.sha256"
            sidecar.write_text(
                f"{'0' * 64}  hsum-{TAG}-aarch64-apple-darwin.zip\n",
                encoding="utf-8",
            )
            result = verify(TAG, root)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("FAILED", result.stdout + result.stderr)

    def test_rejects_archive_sidecar_bound_to_the_wrong_asset(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            create_valid_draft(root)
            sidecar = root / f"hsum-{TAG}-aarch64-apple-darwin.zip.sha256"
            sidecar.write_text(
                f"{digest(root / 'install-hsum.sh')}  install-hsum.sh\n",
                encoding="utf-8",
            )
            result = verify(TAG, root)

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must contain exactly one SHA-256", result.stderr)

    def test_rejects_noncanonical_release_tags(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            result = verify("v0.1.0", pathlib.Path(directory))

        self.assertEqual(result.returncode, 2)
        self.assertIn("exact vX.Y.Z-alpha.N", result.stderr)


if __name__ == "__main__":
    unittest.main()
