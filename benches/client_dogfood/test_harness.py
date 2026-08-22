#!/usr/bin/env python3
"""Unit tests for client event-stream acceptance without calling a model."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

import harness


TOKEN = "HSUM_CLIENT_DOGFOOD_7F3A91"


class ClientDogfoodHarnessTests(unittest.TestCase):
    def test_codex_requires_real_ordered_tool_items_and_get_content(self) -> None:
        events = [
            {"type": "item.completed", "item": {
                "type": "mcp_tool_call", "server": "hsum", "tool": "evidence_search"
            }},
            {"type": "item.completed", "item": {
                "type": "mcp_tool_call",
                "server": "hsum",
                "tool": "evidence_get",
                "result": {"content": TOKEN},
            }},
        ]
        tools = harness.codex_tool_events(events)
        self.assertEqual(
            harness.require_tool_round_trip("Codex", tools, TOKEN),
            ["evidence_search", "evidence_get"],
        )

    def test_claim_without_tool_events_is_rejected(self) -> None:
        with self.assertRaisesRegex(harness.QualificationError, "did not emit ordered"):
            harness.require_tool_round_trip("Codex", [], TOKEN)

    def test_get_event_without_fixture_token_is_rejected(self) -> None:
        tools = [
            {"tool": "evidence_search", "event": {}},
            {"tool": "evidence_get", "event": {"result": "different bytes"}},
        ]
        with self.assertRaisesRegex(harness.QualificationError, "fixture token"):
            harness.require_tool_round_trip("Codex", tools, TOKEN)

    def test_claude_extracts_only_hsum_mcp_tool_use_blocks(self) -> None:
        events = [
            {
                "type": "assistant",
                "message": {"content": [
                    {"type": "tool_use", "id": "native", "name": "Read", "input": {}},
                    {
                        "type": "tool_use",
                        "id": "search",
                        "name": "mcp__hsum__evidence_search",
                        "input": {},
                    },
                ]},
            },
            {
                "type": "user",
                "message": {"content": [{
                    "type": "tool_result",
                    "tool_use_id": "search",
                    "content": "search result",
                }]},
            },
        ]
        self.assertEqual(
            [tool["tool"] for tool in harness.claude_tool_events(events)],
            ["evidence_search"],
        )

    def test_structured_answer_requires_token_and_citation(self) -> None:
        answer = {"status": "ok", "token": TOKEN, "citation_uri": "hsum://citation"}
        self.assertEqual(harness.validate_answer(json.dumps(answer), TOKEN, "client"), answer)
        with self.assertRaisesRegex(harness.QualificationError, "fixture token"):
            harness.validate_answer({**answer, "token": "wrong"}, TOKEN, "client")

    def test_manifest_checksum_is_enforced(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "manifest.json"
            checksum = root / "manifest.sha256"
            manifest.write_text("{}\n", encoding="utf-8")
            checksum.write_text(f"{'0' * 64}  manifest.json\n", encoding="utf-8")
            with self.assertRaisesRegex(harness.QualificationError, "checksum mismatch"):
                harness.load_manifest(manifest, checksum)

    def test_live_evidence_names_are_unique(self) -> None:
        self.assertEqual(len(harness.RAW_EVIDENCE_NAMES), len(set(harness.RAW_EVIDENCE_NAMES)))
        self.assertIn("codex.jsonl", harness.RAW_EVIDENCE_NAMES)
        self.assertIn("claude.jsonl", harness.RAW_EVIDENCE_NAMES)


if __name__ == "__main__":
    unittest.main()
