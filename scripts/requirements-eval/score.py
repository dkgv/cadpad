#!/usr/bin/env python3
"""Score manually reviewed agent runs for the P1.91 requirements corpus."""
import argparse
import json
from pathlib import Path

CASES = Path(__file__).resolve().with_name("cases.jsonl")
VALID_REVIEW = {"correct", "incorrect", "unreviewed"}


def read_jsonl(path):
    with path.open(encoding="utf-8") as stream:
        return [json.loads(line) for line in stream if line.strip() and not line.lstrip().startswith("#")]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("results", type=Path, help="JSONL results copied from results.template.jsonl")
    args = parser.parse_args()
    cases = read_jsonl(CASES)
    results = read_jsonl(args.results)
    expected_ids = [case["id"] for case in cases]
    result_ids = [row.get("id") for row in results]
    if len(expected_ids) != 20 or len(set(expected_ids)) != 20:
        raise SystemExit(f"Corpus must have exactly 20 unique cases; found {len(expected_ids)}")
    if len(set(result_ids)) != len(result_ids) or set(result_ids) - set(expected_ids):
        raise SystemExit("Results contain duplicate or unknown case IDs")
    if set(result_ids) != set(expected_ids):
        missing = sorted(set(expected_ids) - set(result_ids))
        raise SystemExit(f"Results must include all 20 cases; missing: {', '.join(missing)}")
    for row in results:
        if row.get("manual_review", "unreviewed") not in VALID_REVIEW:
            raise SystemExit(f"{row['id']}: manual_review must be correct, incorrect, or unreviewed")
        if row.get("questions") is not None and not isinstance(row["questions"], list):
            raise SystemExit(f"{row['id']}: questions must be a list")
    compiled = [row["compile_success"] for row in results if isinstance(row.get("compile_success"), bool)]
    reviewed = [row for row in results if row.get("manual_review") in {"correct", "incorrect"}]
    wrong = sum(row["manual_review"] == "incorrect" for row in reviewed)
    qcases = [row for row in results if isinstance(row.get("questions"), list)]
    cases_with_questions = [row for row in qcases if row["questions"]]
    question_count = sum(len(row["questions"]) for row in qcases)
    print(f"P1.91 agent evaluation: {len(cases)} prose cases")
    print(f"Compile success: {sum(compiled)}/{len(compiled)} scored" if compiled else "Compile success: unscored (no run data)")
    print(f"Actionable clarification questions: {question_count} across {len(cases_with_questions)}/{len(results)} cases" if qcases else "Actionable clarification questions: unscored (no run data)")
    print(f"Wrong translation: {wrong}/{len(reviewed)} manually reviewed" if reviewed else "Wrong translation: unscored (manual review required)")
    print(f"Unreviewed cases: {sum(row.get('manual_review', 'unreviewed') == 'unreviewed' for row in results)}")


if __name__ == "__main__":
    main()
