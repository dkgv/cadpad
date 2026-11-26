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


def metrics(results):
    compiled = [row["compile_success"] for row in results if isinstance(row.get("compile_success"), bool)]
    reviewed = [row for row in results if row.get("manual_review") in {"correct", "incorrect"}]
    wrong = sum(row["manual_review"] == "incorrect" for row in reviewed)
    qcases = [row for row in results if isinstance(row.get("questions"), list)]
    cases_with_questions = sum(bool(row["questions"]) for row in qcases)
    question_count = sum(len(row["questions"]) for row in qcases)
    return {
        "compile": (sum(compiled), len(compiled)),
        "wrong": (wrong, len(reviewed)),
        "questions": (question_count, len(qcases)),
        # Preserve the established display denominator: the entire evaluated
        # corpus. No question rate is reported when questions were not recorded.
        "question_cases": (cases_with_questions, len(results) if qcases else 0),
    }


def rate(pair):
    return pair[0] / pair[1] if pair[1] else None


def validate_results(results, expected_ids, label):
    result_ids = [row.get("id") for row in results]
    if len(set(result_ids)) != len(result_ids) or set(result_ids) - set(expected_ids):
        raise SystemExit(f"{label}: results contain duplicate or unknown case IDs")
    if set(result_ids) != set(expected_ids):
        missing = sorted(set(expected_ids) - set(result_ids))
        raise SystemExit(f"{label}: results must include all 20 cases; missing: {', '.join(missing)}")
    for row in results:
        if row.get("manual_review", "unreviewed") not in VALID_REVIEW:
            raise SystemExit(f"{label} {row['id']}: manual_review must be correct, incorrect, or unreviewed")
        if row.get("questions") is not None and not isinstance(row["questions"], list):
            raise SystemExit(f"{label} {row['id']}: questions must be a list")


def one_backend(results, label):
    backends = {row.get("backend") for row in results}
    if len(backends) != 1 or not next(iter(backends), None):
        raise SystemExit(f"{label}: comparison requires one nonempty backend label across all rows; found {backends}")
    return next(iter(backends))


def show_comparison(label, current, previous):
    print(f"{label} release comparison (rate change in percentage points):")
    names = (("compile", "Compile success"), ("wrong", "Wrong translation"),
             ("question_cases", "Cases with clarification"))
    for key, title in names:
        now, before = current[key], previous[key]
        now_rate, before_rate = rate(now), rate(before)
        if now_rate is None or before_rate is None:
            delta = "n/a (no scored denominator)"
        else:
            delta = f"{(now_rate - before_rate) * 100:+.1f} pp"
        print(f"{title}: {now[0]}/{now[1]} vs {before[0]}/{before[1]} ({delta})")
    now, before = current["questions"], previous["questions"]
    print(f"Actionable questions: {now[0]} across {current['question_cases'][0]} clarification cases "
          f"vs {before[0]} across {previous['question_cases'][0]} (question counts, not a rate)")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("results", type=Path, help="JSONL results copied from results.template.jsonl")
    parser.add_argument("--compare", type=Path, help="compare against a manually reviewed prior-release JSONL run")
    args = parser.parse_args()
    cases = read_jsonl(CASES)
    results = read_jsonl(args.results)
    expected_ids = [case["id"] for case in cases]
    if len(expected_ids) != 20 or len(set(expected_ids)) != 20:
        raise SystemExit(f"Corpus must have exactly 20 unique cases; found {len(expected_ids)}")
    validate_results(results, expected_ids, "Current run")
    current = metrics(results)
    print(f"P1.91 agent evaluation: {len(cases)} prose cases")
    print(f"Compile success: {current['compile'][0]}/{current['compile'][1]} scored" if current['compile'][1] else "Compile success: unscored (no run data)")
    print(f"Actionable clarification questions: {current['questions'][0]} across {current['question_cases'][0]}/{current['question_cases'][1]} cases" if current['questions'][1] else "Actionable clarification questions: unscored (no run data)")
    print(f"Wrong translation: {current['wrong'][0]}/{current['wrong'][1]} manually reviewed" if current['wrong'][1] else "Wrong translation: unscored (manual review required)")
    print(f"Unreviewed cases: {sum(row.get('manual_review', 'unreviewed') == 'unreviewed' for row in results)}")
    if args.compare:
        previous = read_jsonl(args.compare)
        validate_results(previous, expected_ids, "Comparison run")
        current_backend = one_backend(results, "Current run")
        previous_backend = one_backend(previous, "Comparison run")
        if current_backend != previous_backend:
            raise SystemExit(f"Provider/backend mismatch: current={current_backend}, previous={previous_backend}")
        show_comparison(current_backend, current, metrics(previous))


if __name__ == "__main__":
    main()
