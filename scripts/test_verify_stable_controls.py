"""Fail-closed source and hosted release controls; no production authority."""

import copy
from pathlib import Path
import subprocess
import tempfile
import unittest

import verify_stable_controls as controls


class TagTests(unittest.TestCase):
    def test_tag_object_cannot_be_relabelled(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            def git(*args):
                return subprocess.check_output(["git", "-C", str(root), *args], text=True).strip()
            git("init", "--quiet")
            git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                "-c", "commit.gpgsign=false", "commit", "--quiet", "--allow-empty", "-m", "fixture")
            commit = git("rev-parse", "HEAD")
            git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                "-c", "tag.gpgsign=false", "tag", "-a", "v0.1.0-rc.1", "-m", "fixture")
            git("update-ref", "refs/tags/v0.1.0", "refs/tags/v0.1.0-rc.1")
            with self.assertRaisesRegex(controls.ControlError, "header"):
                controls.verify_tag(root, "v0.1.0", commit)
            git("tag", "-d", "v0.1.0")
            git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                "-c", "tag.gpgsign=false", "tag", "-a", "v0.1.0", "-m", "fixture")
            controls.verify_tag(root, "v0.1.0", commit)
            with self.assertRaises(controls.ControlError):
                controls.verify_tag(root, "v0.1.0", "0" * 40)


class EnvironmentTests(unittest.TestCase):
    def setUp(self):
        self.environment = {
            "name": "stable-release-publish",
            "protection_rules": [{"type": "required_reviewers", "prevent_self_review": True,
                                  "reviewers": [{"type": "User", "reviewer": {"id": 1}}]}],
            "deployment_branch_policy": {"protected_branches": False, "custom_branch_policies": True},
        }
        self.branches = {"total_count": 1, "branch_policies": [{"type": "tag", "name": "v0.1.0"}]}

    def verify(self):
        controls.verify_environment(self.environment, self.branches, "stable-release-publish", "v0.1.0")

    def test_protected_exact_tag_is_accepted(self):
        self.verify()

    def test_missing_reviewers_or_self_review_is_rejected(self):
        original = copy.deepcopy(self.environment)
        for rules in [[], [{"type": "wait_timer"}],
                      [{"type": "required_reviewers", "prevent_self_review": False, "reviewers": [1]}],
                      [{"type": "required_reviewers", "prevent_self_review": True, "reviewers": []}]]:
            self.environment = {**original, "protection_rules": rules}
            with self.assertRaises(controls.ControlError): self.verify()

    def test_unrestricted_branch_or_wrong_environment_is_rejected(self):
        for field, value in [("name", "other"), ("deployment_branch_policy", None),
                             ("deployment_branch_policy", {"protected_branches": True, "custom_branch_policies": False})]:
            original = copy.deepcopy(self.environment)
            self.environment[field] = value
            with self.assertRaises(controls.ControlError): self.verify()
            self.environment = original

    def test_other_tags_wildcards_branches_and_incomplete_pages_are_rejected(self):
        for branch in [{"type": "tag", "name": "v*"}, {"type": "tag", "name": "v0.1.1"},
                       {"type": "branch", "name": "v0.1.0"}]:
            self.branches["branch_policies"] = [branch]
            with self.assertRaises(controls.ControlError): self.verify()
        self.branches = {"total_count": 2, "branch_policies": [{"type": "tag", "name": "v0.1.0"}]}
        with self.assertRaises(controls.ControlError): self.verify()


if __name__ == "__main__":
    unittest.main()
