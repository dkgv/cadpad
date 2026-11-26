# P1.91 requirements agent evaluation

`cases.jsonl` contains 20 prose inputs over the four existing P1 fixtures:
turntable (R1), sliding clamp and retracting arms (R2/R4), cabinet door (E3),
and printable bracket (E5). `expected` and `question_expected` are manual-review
references, not an automatic semantic judge.

## Run

Build the MCP bridge binary, then select a pre-authenticated CLI explicitly and
provide a new output path:

```sh
cargo build -p cadpad-mcp
CADPAD_REQUIREMENTS_EVAL_BACKEND=claude_cli \
CADPAD_REQUIREMENTS_EVAL_RESULTS="$PWD/docs/requirements-eval/run.jsonl" \
cargo test -p cadpad-agent --test requirements_eval -- --ignored --exact run_requirements_agent_eval --nocapture
```

Use `codex_cli` to select Codex. Both CLIs use their configured default
model. The test refuses to run without an explicit supported CLI backend and
output path. It copies each fixture source into a
separate temporary directory, initializes it through Cadpad's project browser,
opens the declared model through the normal project/model APIs, and calls the
real `AgentService` with the local MCP bridge. Drafts are saved only into the
temporary Core catalog so the driver can read the canonical print and
compiler diagnostics. It never edits fixture source or approves a draft. Raw
JSONL output is created mode 0600 on Unix and contains no credential data.

For a one- or two-case backend preflight, set
`CADPAD_REQUIREMENTS_EVAL_CASE_LIMIT=2` and use a separate output path; omit it
for the full 20-case run. The scorer requires all 20 case IDs.

After the run, manually normalize each `questions` array to one entry per
distinct decision the user must answer; do not count question-mark characters.
The scorer reports both the actionable question count and the number of cases
that asked for clarification.

The driver records compile success when a persisted draft has a canonical
print, hashes, and no error-severity diagnostics; warnings are allowed. A
completed non-clarification turn without a valid draft counts as a compile
failure. Clarification-only turns and tool-approval/provider blockers remain
unscored for compilation. Clarification questions must be manually recorded from the full final answer. Review each saved print and response against `expected`, then set
`manual_review` to `correct` or `incorrect` and add a concise rationale.

Score the results with:

```sh
python3 scripts/requirements-eval/score.py "$PWD/docs/requirements-eval/run.jsonl"
```

Wrong-translation rate uses saved translations that were manually reviewed;
clarification-only cases are reported separately and excluded from that
denominator. Keep raw responses and evaluation notes local under
`docs/requirements-eval/`; compare the rate with the previous release.
For a reproducible like-for-like release comparison, record provider/model and
corpus metadata and use the `--compare` workflow in
`docs/requirements-eval/release-comparison.md`. The JSONL currently records
backend but not the resolved model ID, so include that in the release report.
