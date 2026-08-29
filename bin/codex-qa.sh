#!/usr/bin/env bash
set -euo pipefail

readonly CLAUDE_LABEL="claude-approved"
readonly CODEX_LABEL="codex-approved"

# Run from the repository containing this script.
cd "$(git rev-parse --show-toplevel)"

# Verify required commands before doing anything.
for command in gh codex git jq; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "ERROR: required command not found: $command" >&2
        exit 1
    fi
done

# Verify GitHub authentication up front.
if ! gh auth status >/dev/null 2>&1; then
    echo "ERROR: gh is not authenticated." >&2
    exit 1
fi

echo "Checking for open PRs with '$CLAUDE_LABEL' and without '$CODEX_LABEL'..."

# IMPORTANT:
# This is exactly:
#
#   open
#   AND claude-approved
#   AND NOT codex-approved
#
# 'needs-work' intentionally does not participate in eligibility.
mapfile -t prs < <(
    gh pr list \
        --state open \
        --label "$CLAUDE_LABEL" \
        --limit 100 \
        --json number,labels \
        --jq '
            .[]
            | select(
                all(
                    .labels[]?;
                    .name != "codex-approved"
                )
            )
            | .number
        '
)

if ((${#prs[@]} == 0)); then
    echo "No PRs require Codex QA."
    exit 0
fi

echo "Found ${#prs[@]} PR(s): ${prs[*]}"

for pr in "${prs[@]}"; do
    echo
    echo "============================================================"
    echo "Codex QA: PR #$pr"
    echo "============================================================"

    # Re-check state immediately before handing the PR to Codex.
    # Another process may have changed labels since gh pr list ran.
    mapfile -t labels < <(
        gh pr view "$pr" \
            --json labels \
            --jq '.labels[].name'
    )

    has_claude_approved=false
    has_codex_approved=false

    for label in "${labels[@]}"; do
        case "$label" in
            "$CLAUDE_LABEL")
                has_claude_approved=true
                ;;
            "$CODEX_LABEL")
                has_codex_approved=true
                ;;
        esac
    done

    if [[ "$has_claude_approved" != true ]]; then
        echo "Skipping PR #$pr: '$CLAUDE_LABEL' is no longer present."
        continue
    fi

    if [[ "$has_codex_approved" == true ]]; then
        echo "Skipping PR #$pr: '$CODEX_LABEL' is already present."
        continue
    fi

    pr_url="$(
        gh pr view "$pr" \
            --json url \
            --jq '.url'
    )"

    pr_title="$(
        gh pr view "$pr" \
            --json title \
            --jq '.title'
    )"

    echo "Title: $pr_title"
    echo "URL:   $pr_url"
    echo "Starting Codex..."

    # Codex owns the actual review and GitHub state transition.
    # The wrapper intentionally does not parse a PASS/FAIL file and does
    # not maintain local workflow state.
    codex exec -c 'approval_policy="never"' -c 'sandbox_mode="read-only"' -c 'sandbox_read_only.network_access=true' "
You are the independent Codex QA worker for GitHub PR #$pr.

Read .codex/qa-instructions.md before doing anything else.

Also follow the repository's normal AGENTS.md instructions.

Use GitHub CLI freely. Inspect the PR, linked issue, PR discussion,
commits, complete diff, checks, relevant source, tests, configuration,
documentation, and git history as needed.

You are responsible for the Codex QA labels and the PR comment.

If the reviewed PR passes:
    add '$CODEX_LABEL'
    remove 'needs-work' if present
    post the QA result as a PR comment

If the reviewed PR has blocking defects:
    add 'needs-work'
    remove '$CODEX_LABEL' if present
    post detailed findings as a PR comment

Do not modify source code, tests, configuration, or git history.

Do not create local workflow state files.

Do not touch '$CLAUDE_LABEL' unless the repository's explicit workflow
instructions require it; normally that label belongs to Claude.

Before applying the final label decision, re-check the PR's current HEAD
on GitHub. If the PR changed during your review, treat the review as stale
and do not add '$CODEX_LABEL'.

The GitHub PR is the persistent record of your review.
"

    echo "Codex finished PR #$pr."
done

echo
echo "Codex QA run complete."
