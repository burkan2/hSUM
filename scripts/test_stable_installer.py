#!/usr/bin/env python3
"""Network-free stable installer tests with real detached signatures."""

import os
from pathlib import Path
import subprocess
import unittest
import zipfile

import test_verify_stable_release as signatures

MINISIGN = signatures.MINISIGN


class StableInstallerTests(unittest.TestCase):
    def setUp(self):
        fixture = signatures.StableReleaseTests()
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        self.fixture = fixture
        self.root = fixture.root
        self.dist = fixture.dist
        self.bin = self.root / "tools"
        self.bin.mkdir()
        self.template = Path(os.environ.get("HSUM_INSTALLER_TEMPLATE", Path(__file__).with_name("install.sh")))
        self.installer = self.root / "installer.sh"
        self.render(fixture.key)
        self.write_tool("uname", '#!/bin/bash\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; esac\n')
        self.write_tool("spctl", '#!/bin/bash\nexit "${HSUM_TEST_GATEKEEPER_REJECT:-0}"\n')
        self.write_tool("ditto", '#!/bin/bash\ncp "$1" "$2"\n')
        self.write_tool("xattr", '#!/bin/bash\necho forbidden >> "$HSUM_TEST_EXECUTIONS"\nexit 0\n')
        self.write_tool("curl", '''#!/usr/bin/env python3
import os,pathlib,shutil,sys
args=sys.argv[1:];target=args[args.index('--output')+1]
name=args[-1].rsplit('/',1)[1]
shutil.copyfile(pathlib.Path(os.environ['HSUM_TEST_FIXTURES'])/name,target)
''')
        (self.bin / "minisign").symlink_to(Path(MINISIGN).resolve() if "/" in MINISIGN else __import__("shutil").which(MINISIGN))
        self.env = {**os.environ, "PATH": str(self.bin)+os.pathsep+os.environ["PATH"],
                    "XDG_BIN_HOME": str(self.root/"installed"), "HSUM_TEST_FIXTURES": str(self.dist),
                    "HSUM_TEST_EXECUTIONS": str(self.root/"executions"), "PYTHONDONTWRITEBYTECODE": "1"}
        asset=self.dist/fixture.payloads[0]
        with zipfile.ZipFile(asset,"w") as archive:
            archive.writestr("hsum", '#!/bin/bash\nprintf "%s\\n" "$*" >> "$HSUM_TEST_EXECUTIONS"\nif [ "$1" = --version ]; then echo "hsum 0.1.0"; fi\n')
        (self.dist/(asset.name+".sha256")).write_text(fixture.line(asset.name))
        (self.dist/"install-hsum.sh").write_bytes(self.installer.read_bytes())
        (self.dist/"SHA256SUMS").write_text("".join(fixture.line(name) for name in sorted(fixture.payloads)))
        for name in ["SHA256SUMS","install-hsum.sh",asset.name]:fixture.sign(name)

    def write_tool(self,name,contents):
        path=self.bin/name;path.write_text(contents);path.chmod(0o700)

    def render(self,key):
        self.installer.write_text(self.template.read_text().replace("@HSUM_VERSION@","0.1.0").replace("@HSUM_MINISIGN_PUBLIC_KEY@",key))

    def run_installer(self,*args):
        return subprocess.run(["bash",str(self.installer),"--confirm","--activate",str(self.root),*args],
                              env=self.env,cwd=self.root,text=True,capture_output=True)

    def assert_refused_without_execution(self,result):
        self.assertNotEqual(result.returncode,0,result.stdout+result.stderr)
        self.assertFalse((self.root/"executions").exists())
        self.assertFalse((self.root/"installed/hsum").exists())

    def test_valid_signed_bundle_installs_and_activates(self):
        result=self.run_installer()
        self.assertEqual(result.returncode,0,result.stdout+result.stderr)
        self.assertTrue((self.root/"installed/hsum").is_file())
        self.assertIn("integration install codex",(self.root/"executions").read_text())

    def test_manifest_tampering_is_refused_before_binary_execution(self):
        with (self.dist/"SHA256SUMS").open("a") as stream:stream.write("changed\n")
        self.assert_refused_without_execution(self.run_installer())

    def test_archive_tampering_is_refused_before_extraction_or_execution(self):
        with (self.dist/self.fixture.payloads[0]).open("ab") as stream:stream.write(b"changed")
        self.assert_refused_without_execution(self.run_installer())

    def test_unsigned_nul_suffix_cannot_forge_verified_comment_context(self):
        for name in ["SHA256SUMS", self.fixture.payloads[0]]:
            signature=self.dist/(name+".minisig")
            original=signature.read_bytes()
            subprocess.run([MINISIGN,"-S","-s",str(self.root/"test.key"),
                            "-m",str(self.dist/name),"-t","hSUM"],
                           check=True,capture_output=True)
            lines=signature.read_bytes().split(b"\n")
            lines[2]=b"trusted comment: hSUM\0 v0.1.0: "+name.encode()
            signature.write_bytes(b"\n".join(lines))
            with self.subTest(name=name):
                self.assert_refused_without_execution(self.run_installer())
            signature.write_bytes(original)

    def test_missing_key_and_missing_verifier_are_refused(self):
        self.render("")
        self.assert_refused_without_execution(self.run_installer())
        self.render(self.fixture.key)
        (self.bin/"minisign").unlink()
        self.write_tool("minisign", "#!/bin/bash\nexit 127\n")
        self.assert_refused_without_execution(self.run_installer())

    def test_stable_rejects_alpha_notarization_bypass(self):
        self.assert_refused_without_execution(self.run_installer("--allow-unnotarized-alpha"))

    def test_gatekeeper_failure_cannot_execute_or_clear_quarantine(self):
        self.env["HSUM_TEST_GATEKEEPER_REJECT"]="1"
        self.assert_refused_without_execution(self.run_installer())


if __name__ == "__main__":
    unittest.main()
