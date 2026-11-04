#!/usr/bin/env python3
"""Verify the canonical completion ledger vocabulary, denominator, and metrics."""

from __future__ import annotations

import argparse
import json
import pathlib
from typing import Any


CANONICAL_ROW_COUNT = 54
CANONICAL_CORE_ROW_COUNT = 31
EVIDENCE_VALUES = {"yes", "no", "n/a"}
TABLE_START = "## Canonical in-scope rows"
TABLE_END = "## Explicit canonical exclusions"


class LedgerError(RuntimeError):
    """The stable completion ledger no longer matches its stated metrics."""


def percentage(numerator: int, denominator: int) -> str:
    return f"{100 * numerator / denominator:.1f}%"


def canonical_rows(ledger: str) -> list[tuple[str, list[str]]]:
    start = ledger.find(TABLE_START)
    end = ledger.find(TABLE_END)
    if start < 0 or end < 0 or end <= start:
        raise LedgerError("canonical ledger table boundaries are missing")

    rows: list[tuple[str, list[str]]] = []
    seen: set[str] = set()
    for line in ledger[start:end].splitlines():
        if not line.startswith("|") or line.startswith("|---"):
            continue
        fields = [field.strip() for field in line.strip().strip("|").split("|")]
        if fields[0] == "ID":
            continue
        if len(fields) != 10:
            raise LedgerError(f"malformed canonical ledger row: {line}")
        row_id = fields[0]
        if row_id in seen:
            raise LedgerError(f"duplicate canonical ledger row: {row_id}")
        seen.add(row_id)
        evidence = fields[3:9]
        for value in evidence:
            if value not in EVIDENCE_VALUES:
                raise LedgerError(
                    f"invalid evidence value in {row_id}: {value!r}; "
                    f"allowed={sorted(EVIDENCE_VALUES)}"
                )
        rows.append((row_id, evidence))
    return rows


def require_claim(ledger: str, label: str, claim: str) -> None:
    expected = f"- {label}: {claim}"
    if expected not in ledger:
        raise LedgerError(f"ledger metric is stale; expected {expected}")


def verify(ledger: str) -> dict[str, Any]:
    rows = canonical_rows(ledger)
    if len(rows) != CANONICAL_ROW_COUNT:
        raise LedgerError(
            f"canonical row count must be {CANONICAL_ROW_COUNT}, observed {len(rows)}"
        )

    implementation_count = sum(evidence[0] == "yes" for _, evidence in rows)
    earned = sum(value == "yes" for _, evidence in rows for value in evidence)
    applicable = sum(value != "n/a" for _, evidence in rows for value in evidence)
    release_count = sum(evidence[5] == "yes" for _, evidence in rows)

    core = [
        evidence
        for row_id, evidence in rows
        if row_id.startswith(("A1-", "A2-", "B1-"))
    ]
    if len(core) != CANONICAL_CORE_ROW_COUNT:
        raise LedgerError(
            f"stable core row count must be {CANONICAL_CORE_ROW_COUNT}, "
            f"observed {len(core)}"
        )
    core_implemented = sum(evidence[0] == "yes" for evidence in core)

    require_claim(
        ledger,
        "Stable core implementation coverage (A1/A2/B1 rows)",
        f"**{percentage(core_implemented, len(core))}** "
        f"({core_implemented}/{len(core)})",
    )
    require_claim(
        ledger,
        "All in-scope implementation coverage",
        f"**{percentage(implementation_count, len(rows))}** "
        f"({implementation_count}/{len(rows)})",
    )
    require_claim(
        ledger,
        "Full stable-program evidence",
        f"**{percentage(earned, applicable)}** "
        f"({earned}/{applicable} applicable evidence cells)",
    )
    require_claim(
        ledger,
        "Release-qualified coverage",
        f"**{percentage(release_count, len(rows))}** "
        f"({release_count}/{len(rows)})",
    )

    return {
        "schema_version": "hsum.completion-ledger-contract.v1",
        "passed": True,
        "canonical_rows": len(rows),
        "implementation_count": implementation_count,
        "earned_evidence_cells": earned,
        "applicable_evidence_cells": applicable,
        "release_qualified_count": release_count,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    repository = pathlib.Path(__file__).resolve().parents[1]
    parser.add_argument(
        "--ledger",
        type=pathlib.Path,
        default=repository / "outputs/STABLE_V0_1_COMPLETION_LEDGER.md",
    )
    args = parser.parse_args()
    report = verify(args.ledger.read_text(encoding="utf-8"))
    print(json.dumps(report, sort_keys=True, separators=(",", ":")))


if __name__ == "__main__":
    main()
