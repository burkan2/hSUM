#!/usr/bin/env python3
"""Exercise stable smoke orchestration with real signatures and stub OS tools.

This validates routing and failure propagation, not Gatekeeper or isolation.
Native release workflows exercise the actual OS checks and product binary.
"""

import base64
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import unittest
import zipfile

import test_verify_stable_release as signatures


class SmokeRoutingTests(unittest.TestCase):
    def setUp(self):
        self.fixture = signatures.StableReleaseTests()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.root = self.fixture.root
        self.repo = self.root / "repo"
        scripts = self.repo / "scripts"
        scripts.mkdir(parents=True)
        for name in ["install.sh", "installer-smoke.sh", "no-network-smoke.sh",
                     "verify_stable_release.py", "verify_stable_authority.py"]:
            shutil.copyfile(Path(__file__).with_name(name), scripts / name)
        key = self.fixture.key
        fingerprint = hashlib.sha256(base64.b64decode(key)).hexdigest()
        (self.repo / "Cargo.toml").write_text('[package]\nversion = "0.1.0"\n'
            f'[package.metadata.hsum-release]\nminisign-public-key = "{key}"\n'
            f'minisign-public-key-sha256 = "{fingerprint}"\n')
        (self.repo / "release").mkdir()
        shutil.copyfile(self.root / "test.pub", self.repo / "release/minisign.pub")
        (self.repo / "SECURITY.md").write_text(f"Minisign public key SHA-256: `{fingerprint}`\n")
        self.tools = self.root / "tools"
        self.tools.mkdir()
        minisign = shutil.which(signatures.MINISIGN)
        if not minisign:
            raise RuntimeError("real Minisign is required")
        (self.tools / "minisign").symlink_to(minisign)
        self.tool("uname", '#!/bin/bash\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; esac\n')
        self.tool("spctl", '#!/bin/bash\nexit "${HSUM_TEST_GATEKEEPER_EXIT:-0}"\n')
        self.tool("ditto", '#!/bin/bash\ncp "$1" "$2"\n')
        self.tool("sandbox-exec", '#!/bin/bash\nshift 2\nexec "$@"\n')
        self.env = {**os.environ, "PATH": str(self.tools) + os.pathsep + os.environ["PATH"],
                    "PYTHONDONTWRITEBYTECODE": "1"}
        # Stand-in for the integration output consumed by installer-smoke.sh.
        self.binary = self.root / "hsum"
        self.binary.write_text('''#!/usr/bin/env python3
import json,os,pathlib,sys
args=sys.argv[1:]
if args[0]=='--version': print('hsum 0.1.0')
elif args[:2]==['integration','install']:
 registration=pathlib.Path(os.environ['HSUM_FAKE_CODEX_REGISTRATION'])
 changed=not registration.exists()
 registration.write_text(str(pathlib.Path(sys.argv[0]).resolve()))
 print('Citation round trip: verified')
 print('Registration changed: '+('yes' if changed else 'no'))
elif args[:2]==['integration','status']:
 print(json.dumps({'registration':'current','registered_executable':str(pathlib.Path(sys.argv[0]).resolve()),'registered_arguments':['mcp'],'agent_policy':'current','next_actions':[]}))
else: sys.exit(2)
''')
        self.binary.chmod(0o700)
        dist = self.fixture.dist
        with zipfile.ZipFile(dist / self.fixture.payloads[0], "w") as archive:
            archive.write(self.binary, "hsum")
        with tarfile.open(dist / self.fixture.payloads[2], "w:gz") as archive:
            entry = tarfile.TarInfo("hsum")
            contents = self.binary.read_bytes()
            entry.size = len(contents)
            entry.mode = 0o700
            archive.addfile(entry, io.BytesIO(contents))
        (dist / "install-hsum.sh").write_text((scripts / "install.sh").read_text()
            .replace("@HSUM_VERSION@", "0.1.0").replace("@HSUM_MINISIGN_PUBLIC_KEY@", key))
        for name in [self.fixture.payloads[0], self.fixture.payloads[2]]:
            (dist / (name + ".sha256")).write_text(self.fixture.line(name))
        (dist / "SHA256SUMS").write_text("".join(self.fixture.line(name) for name in sorted(self.fixture.payloads)))
        for name in ["SHA256SUMS", "install-hsum.sh", self.fixture.payloads[0], self.fixture.payloads[2]]:
            self.fixture.sign(name)
        (scripts / "release-smoke.sh").write_text('#!/bin/bash\nexit "${HSUM_TEST_RELEASE_EXIT:-0}"\n')

    def tool(self, name, contents):
        path = self.tools / name
        path.write_text(contents)
        path.chmod(0o700)

    def run_smoke(self, name, *args, **environment):
        return subprocess.run(["bash", str(self.repo / "scripts" / name), str(self.binary), *map(str, args)],
                              env={**self.env, **environment}, capture_output=True, text=True, timeout=60)

    def test_stable_bundle_installs_and_tampering_is_refused_on_both_platform_routes(self):
        for system, machine in [("Darwin", "arm64"), ("Linux", "x86_64")]:
            self.tool("uname", f'#!/bin/bash\ncase "$1" in -s) echo {system};; -m) echo {machine};; esac\n')
            result = self.run_smoke("installer-smoke.sh", self.fixture.dist)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("installer smoke passed", result.stdout)

    def test_stable_requires_bundle_and_real_gatekeeper_success(self):
        result = self.run_smoke("installer-smoke.sh")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("requires the signed release bundle", result.stderr)
        result = self.run_smoke("installer-smoke.sh", self.fixture.dist, HSUM_TEST_GATEKEEPER_EXIT="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("rejected the stable Developer ID/notarization assessment", result.stderr)

    def test_offline_routing_preserves_bundle_and_propagates_failures(self):
        result = self.run_smoke("no-network-smoke.sh", self.fixture.dist)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("installer smoke passed", result.stdout)
        result = self.run_smoke("no-network-smoke.sh", self.fixture.dist, HSUM_TEST_RELEASE_EXIT="23")
        self.assertEqual(result.returncode, 23)
        self.assertNotIn("installer smoke passed", result.stdout)
        result = self.run_smoke("no-network-smoke.sh", self.fixture.dist, HSUM_TEST_GATEKEEPER_EXIT="1")
        self.assertNotEqual(result.returncode, 0)
        result = self.run_smoke("no-network-smoke.sh", "--binary-only")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("installer smoke passed", result.stdout)


if __name__ == "__main__":
    unittest.main()
