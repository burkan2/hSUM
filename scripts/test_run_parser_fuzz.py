import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("run-parser-fuzz.sh")


class FuzzRunnerTests(unittest.TestCase):
    def run_fixture(self, seconds, fail=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            cargo = root / "cargo"
            cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$*"\nexit ' + ('7' if fail else '0') + '\n')
            cargo.chmod(0o700)
            env = {**os.environ, "PATH": str(root) + os.pathsep + os.environ["PATH"], "FUZZ_SECONDS": seconds}
            result = subprocess.run(["bash", str(SCRIPT), str(root / "logs")], env=env, capture_output=True, text=True)
            logs = {p.stem: p.read_text() for p in (root / "logs").glob("*.log")}
            return result, logs

    def test_all_targets_keep_the_requested_duration_and_safety_limits(self):
        result, logs = self.run_fixture("600")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(set(logs), {"citation_uri", "query_parser", "search_cursor", "jsonl_snapshot", "mcp_frame", "chunk_bytes"})
        for target, log in logs.items():
            self.assertIn(f"fuzz run {target} --", log)
            self.assertIn("-max_total_time=600", log)
            self.assertIn("-rss_limit_mb=2048", log)
            self.assertIn("-timeout=5", log)

    def test_bad_durations_are_rejected_before_execution(self):
        for seconds in ("0", "1801", "999999999999999999999", "-1", "1.5", "1; exit 0", "01"):
            with self.subTest(seconds=seconds):
                result, logs = self.run_fixture(seconds)
                self.assertEqual(result.returncode, 2)
                self.assertFalse(logs)

    def test_crashes_preserve_the_log_and_failure_exit(self):
        result, logs = self.run_fixture("15", fail=True)
        self.assertEqual(result.returncode, 7)
        self.assertEqual(set(logs), {"citation_uri"})


if __name__ == "__main__":
    unittest.main()
