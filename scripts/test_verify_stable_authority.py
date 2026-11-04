"""The stable draft must use one public key pinned in three signed-source files."""

import base64
import hashlib
from pathlib import Path
import tempfile
import unittest

import verify_stable_authority as authority


class AuthorityTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        (self.root / "release").mkdir()
        # Structural public-key fixture only; no signing identity is generated.
        self.raw = b"Ed" + bytes(range(40))
        self.key = base64.b64encode(self.raw).decode()
        self.fingerprint = hashlib.sha256(self.raw).hexdigest()
        (self.root / "release/minisign.pub").write_text(f"untrusted comment: test key\n{self.key}\n")
        (self.root / "Cargo.toml").write_text(
            '[package.metadata.hsum-release]\n'
            f'minisign-public-key = "{self.key}"\n'
            f'minisign-public-key-sha256 = "{self.fingerprint}"\n')
        (self.root / "SECURITY.md").write_text(f"Minisign public key SHA-256: `{self.fingerprint}`\n")

    def test_matching_pins_are_returned(self):
        self.assertEqual(authority.verify(self.root), {"public_key": self.key, "public_key_sha256": self.fingerprint})

    def test_each_missing_authority_file_fails(self):
        for name in ["release/minisign.pub", "Cargo.toml", "SECURITY.md"]:
            path = self.root / name
            original = path.read_bytes()
            path.unlink()
            with self.assertRaises(authority.AuthorityError):
                authority.verify(self.root)
            path.write_bytes(original)

    def test_wrong_key_and_hash_in_cargo_fail(self):
        path = self.root / "Cargo.toml"
        original = path.read_text()
        for changed in [original.replace(self.key, "unconfigured"), original.replace(self.fingerprint, "0" * 64)]:
            path.write_text(changed)
            with self.assertRaises(authority.AuthorityError):
                authority.verify(self.root)

    def test_missing_wrong_and_ambiguous_security_pin_fail(self):
        path = self.root / "SECURITY.md"
        original = path.read_text()
        for changed in ["pending\n", original.replace(self.fingerprint, "0" * 64), original + original]:
            path.write_text(changed)
            with self.assertRaises(authority.AuthorityError):
                authority.verify(self.root)

    def test_malformed_and_oversized_key_files_fail(self):
        path = self.root / "release/minisign.pub"
        for text in [self.key, "untrusted comment: test\ninvalid\n", "x" * 4097,
                     f"untrusted comment: test\vforged\n{self.key}\n"]:
            path.write_text(text)
            with self.assertRaises(authority.AuthorityError):
                authority.verify(self.root)

    def test_symlinked_authority_files_fail(self):
        path = self.root / "release/minisign.pub"
        target = self.root / "outside.pub"
        path.rename(target)
        path.symlink_to(target)
        with self.assertRaises(authority.AuthorityError):
            authority.verify(self.root)


if __name__ == "__main__":
    unittest.main()
