#!/usr/bin/env python3
"""Bind signed tag headers and require live protected release environments.

Tag header checks supplement git verify-tag; they do not verify signatures.
Environment responses must come from GitHub's authenticated REST API.
"""

import argparse
import json
from pathlib import Path
import re
import subprocess
import sys


class ControlError(ValueError):
    pass


def stable_tag(tag):
    if not re.fullmatch(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", tag):
        raise ControlError("expected a canonical stable tag")


def verify_tag(root, tag, commit):
    stable_tag(tag)
    if not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", commit):
        raise ControlError("invalid expected commit")
    result = subprocess.run(["git", "-C", str(root), "cat-file", "tag", "refs/tags/" + tag],
                            capture_output=True, check=True, timeout=30)
    headers, separator, _ = result.stdout.partition(b"\n\n")
    lines = headers.split(b"\n")
    expected = {b"tag": tag.encode(), b"type": b"commit", b"object": commit.encode()}
    for key, value in expected.items():
        if not separator or [line for line in lines if line.startswith(key + b" ")] != [key + b" " + value]:
            raise ControlError("signed tag header does not bind the requested name and commit")


def verify_environment(environment, branches, name, tag):
    stable_tag(tag)
    if not isinstance(environment, dict) or environment.get("name") != name:
        raise ControlError("missing or wrong release environment")
    rules = environment.get("protection_rules", [])
    reviewers = [rule for rule in rules if isinstance(rule, dict) and rule.get("type") == "required_reviewers"]
    if (len(reviewers) != 1 or reviewers[0].get("prevent_self_review") is not True
            or not reviewers[0].get("reviewers")):
        raise ControlError("release environment requires reviewers and self-review prevention")
    if environment.get("deployment_branch_policy") != {"protected_branches": False, "custom_branch_policies": True}:
        raise ControlError("release environment requires an explicit tag deployment policy")
    policies = branches.get("branch_policies", [])
    if (branches.get("total_count") != 1 or len(policies) != 1
            or policies[0].get("type") != "tag" or policies[0].get("name") != tag):
        raise ControlError("release environment must allow only this exact stable tag")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    tag_parser = commands.add_parser("tag")
    tag_parser.add_argument("tag")
    tag_parser.add_argument("commit")
    environment_parser = commands.add_parser("environment")
    for argument in ["environment_json", "branches_json", "name", "tag"]:
        environment_parser.add_argument(argument)
    args = parser.parse_args()
    try:
        if args.command == "tag":
            verify_tag(Path.cwd(), args.tag, args.commit)
        else:
            verify_environment(json.loads(Path(args.environment_json).read_text()),
                               json.loads(Path(args.branches_json).read_text()), args.name, args.tag)
    except (ControlError, OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"stable control verification failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
