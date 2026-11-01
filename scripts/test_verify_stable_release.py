#!/usr/bin/env python3
"""Real Minisign tests; keys are disposable and never production authority."""

import hashlib
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location(
    "stable_release", Path(__file__).with_name("verify_stable_release.py")
)
contract = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(contract)
MINISIGN = os.environ.get("HSUM_TEST_MINISIGN", "minisign")


class StableReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="hsum-test-signatures-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.dist = self.root / "dist"
        self.dist.mkdir()
        self.tag = "v0.1.0"
        subprocess.run(
            [MINISIGN, "-G", "-W", "-p", str(self.root / "test.pub"),
             "-s", str(self.root / "test.key")],
            check=True, capture_output=True,
        )
        self.key = (self.root / "test.pub").read_text().splitlines()[1]
        self.payloads = [
            "hsum-v0.1.0-aarch64-apple-darwin.zip",
            "hsum-v0.1.0-aarch64-apple-darwin.spdx.json",
            "hsum-v0.1.0-x86_64-unknown-linux-gnu.tar.gz",
            "hsum-v0.1.0-x86_64-unknown-linux-gnu.spdx.json",
            "hsum-v0.1.0-cargo-licenses.json", "install-hsum.sh",
        ]
        for name in self.payloads:
            (self.dist / name).write_text(f"test payload {name}\n")
        for name in self.payloads:
            if name.endswith((".zip", ".tar.gz")):
                (self.dist / (name + ".sha256")).write_text(self.line(name))
        (self.dist / "SHA256SUMS").write_text("".join(self.line(name) for name in sorted(self.payloads)))
        for name in ["SHA256SUMS", "install-hsum.sh", *self.payloads[:1], self.payloads[2]]:
            self.sign(name)

    def line(self, name):
        digest = hashlib.sha256((self.dist / name).read_bytes()).hexdigest()
        return f"{digest}  {name}\n"

    def sign(self, name):
        subprocess.run([MINISIGN, "-S", "-s", str(self.root / "test.key"),
                        "-m", str(self.dist / name), "-t", f"hSUM {self.tag}: {name}"],
                       check=True, capture_output=True)

    def verify(self, **options):
        return contract.verify(self.tag, self.dist, self.key, minisign=MINISIGN, **options)

    def test_valid_signed_bundle(self):
        result = self.verify()
        self.assertTrue(result["signatures_verified"])
        self.assertEqual(result["signed_files"], 4)

    def test_rejects_missing_and_wrong_authority(self):
        with self.assertRaises(contract.ReleaseError):
            contract.verify(self.tag, self.dist, "", minisign=MINISIGN)
        subprocess.run([MINISIGN, "-G", "-W", "-p", str(self.root / "wrong.pub"),
                        "-s", str(self.root / "wrong.key")],check=True,capture_output=True)
        wrong = (self.root / "wrong.pub").read_text().splitlines()[1]
        with self.assertRaises(contract.ReleaseError):
            contract.verify(self.tag, self.dist, wrong, minisign=MINISIGN)

    def test_rejects_tampered_manifest_even_when_checksums_are_consistent(self):
        name = "install-hsum.sh"
        (self.dist/name).write_text("substituted installer\n")
        (self.dist/"SHA256SUMS").write_text("".join(self.line(name) for name in sorted(self.payloads)))
        with self.assertRaises(contract.ReleaseError):
            self.verify()

    def test_rejects_swapped_signature_and_missing_signature(self):
        shutil.copyfile(self.dist/"install-hsum.sh.minisig",self.dist/"SHA256SUMS.minisig")
        with self.assertRaises(contract.ReleaseError):self.verify()

    def test_rejects_wrong_trusted_comment_and_unsigned_control_injection(self):
        target=self.dist/"SHA256SUMS"
        subprocess.run([MINISIGN,"-S","-s",str(self.root/"test.key"),
                        "-m",str(target),"-t","wrong tag and filename"],
                       check=True,capture_output=True)
        with self.assertRaises(contract.ReleaseError):self.verify()
        signature=self.dist/"SHA256SUMS.minisig"
        original=signature.read_bytes()
        for separator in [b"\v",b"\f",b"\r",b"\x1c"]:
            forged=(b"untrusted comment: ignored"+separator+b"ignored"+separator+
                    b"trusted comment: hSUM v0.1.0: SHA256SUMS\n"+original.split(b"\n",1)[1])
            signature.write_bytes(forged)
            with self.assertRaises(contract.ReleaseError):self.verify()

    def test_rejects_legacy_nonprehashed_signature(self):
        subprocess.run([MINISIGN,"-S","-l","-s",str(self.root/"test.key"),
                        "-m",str(self.dist/"SHA256SUMS"),"-t","hSUM v0.1.0: SHA256SUMS"],
                       check=True,capture_output=True)
        with self.assertRaises(contract.ReleaseError):self.verify()
        (self.dist/"SHA256SUMS.minisig").unlink()
        with self.assertRaises(contract.ReleaseError):self.verify()

    def test_rejects_unsigned_payload_changes(self):
        (self.dist/self.payloads[1]).write_text("changed sbom\n")
        with self.assertRaises(contract.ReleaseError):self.verify()

    def test_rejects_extras_symlinks_and_nested_entries(self):
        extra=self.dist/"extra"
        for create in [lambda: extra.write_text("extra"), lambda: extra.mkdir(),
                       lambda: extra.symlink_to(self.root/"test.pub")]:
            create()
            with self.assertRaises(contract.ReleaseError):self.verify()
            if extra.is_dir():extra.rmdir()
            else:extra.unlink()
        payload=self.dist/self.payloads[0]
        saved=self.root/"payload"
        payload.rename(saved);payload.symlink_to(saved)
        with self.assertRaises(contract.ReleaseError):self.verify()

    def test_rejects_duplicate_or_traversing_manifest_entries(self):
        manifest=self.dist/"SHA256SUMS"
        original=manifest.read_text()
        for modified in [original+original.splitlines()[0]+"\n",
                         original.replace(self.payloads[0],"../test.key")]:
            manifest.write_text(modified);self.sign("SHA256SUMS")
            with self.assertRaises(contract.ReleaseError):self.verify()

    def test_draft_validation_cannot_claim_signatures(self):
        for path in self.dist.glob("*.minisig"):path.unlink()
        result=self.verify(unsigned_draft=True)
        self.assertFalse(result["signatures_verified"])
        with self.assertRaises(contract.ReleaseError):self.verify()

    def test_rejects_noncanonical_tags(self):
        for tag in ["v01.0.0","v0.1.0-alpha.4","v0.1.0/../x","0.1.0"]:
            with self.assertRaises(contract.ReleaseError):
                contract.verify(tag,self.dist,self.key,minisign=MINISIGN)


if __name__ == "__main__":
    unittest.main()
