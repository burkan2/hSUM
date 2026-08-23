#!/usr/bin/env python3

import copy
import importlib.util
import pathlib
import unittest


MODULE_PATH = pathlib.Path(__file__).with_name("verify_clean_machine_evidence.py")
SPEC = importlib.util.spec_from_file_location("verify_clean_machine_evidence", MODULE_PATH)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


def report(machine: str) -> dict:
    trial = {
        "trial": 1,
        "passed": True,
        "first_init_seconds": 0,
        "first_cli_citation_seconds": 0,
        "mcp_round_trip_seconds": 1,
        "total_seconds": 1,
    }
    return {
        "schema_version": "hsum.clean-machine-trials.v1",
        "passed": True,
        "protocol": {
            "trial_count": 5,
            "network": "disabled",
            "fresh_repository_per_trial": True,
            "fresh_hsum_home_per_trial": True,
            "cli_citation_round_trip": True,
            "mcp_search_get_status_project_round_trip": True,
        },
        "candidate": {
            "version": "hsum 0.1.0-alpha.4",
            "sha256": "a" * 64,
            "commit_sha": "1" * 40,
            "checkout_commit_sha": "1" * 40,
            "source_commit_sha": "2" * 40,
            "base_commit_sha": "3" * 40,
            "tree_sha": "4" * 40,
        },
        "platform": {
            "machine": machine,
            "system": "Linux" if machine == "x86_64" else "Darwin",
        },
        "github": {"run_id": "123", "run_attempt": "1"},
        "trials": [{**trial, "trial": number} for number in range(1, 6)],
        "summary": {
            "failure_reasons": [],
            "first_cli_citation_seconds": {"target_passed": True},
            "mcp_round_trip_seconds": {"target_passed": True},
        },
    }


class VerifyCleanMachineEvidenceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.reports = [report("x86_64"), report("arm64")]
        self.expected = MODULE.ExpectedProvenance(
            checkout_sha="1" * 40,
            source_sha="2" * 40,
            base_sha="3" * 40,
            tree_sha="4" * 40,
            run_id="123",
        )

    def test_accepts_two_complete_native_reports_bound_to_all_identities(self) -> None:
        MODULE.verify_reports(self.reports, self.expected)

    def test_rejects_each_ambiguous_or_mismatched_identity(self) -> None:
        for field in (
            "commit_sha",
            "checkout_commit_sha",
            "source_commit_sha",
            "base_commit_sha",
            "tree_sha",
        ):
            with self.subTest(field=field):
                reports = copy.deepcopy(self.reports)
                reports[1]["candidate"][field] = "f" * 40
                with self.assertRaisesRegex(ValueError, field):
                    MODULE.verify_reports(reports, self.expected)

    def test_rejects_wrong_run_or_incomplete_trial_protocol(self) -> None:
        wrong_run = copy.deepcopy(self.reports)
        wrong_run[0]["github"]["run_id"] = "999"
        with self.assertRaisesRegex(ValueError, "run_id"):
            MODULE.verify_reports(wrong_run, self.expected)

        incomplete = copy.deepcopy(self.reports)
        incomplete[0]["trials"].pop()
        with self.assertRaisesRegex(ValueError, "five trials"):
            MODULE.verify_reports(incomplete, self.expected)


if __name__ == "__main__":
    unittest.main()
