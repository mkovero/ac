#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "--daemon" ]]; then
    if (($# != 1)); then
        echo "Usage: $0 [--daemon]" >&2
        exit 2
    fi

    readonly POLL_SECONDS="${CODEX_QA_POLL_SECONDS:-300}"
    if [[ ! "$POLL_SECONDS" =~ ^[1-9][0-9]*$ ]]; then
        echo "ERROR: CODEX_QA_POLL_SECONDS must be a positive integer." >&2
        exit 2
    fi

    SCRIPT_PATH="$(realpath "$0")"
    readonly SCRIPT_PATH
    echo "Codex QA daemon started; polling every ${POLL_SECONDS}s."

    while true; do
        if ! "$SCRIPT_PATH"; then
            echo "Codex QA pass failed; retrying in ${POLL_SECONDS}s." >&2
        fi
        sleep "$POLL_SECONDS"
    done
elif (($# != 0)); then
    echo "Usage: $0 [--daemon]" >&2
    exit 2
fi

source "$(dirname "$0")/common.sh"

readonly CLAUDE_LABEL="claude-approved"
readonly CODEX_LABEL="codex-approved"
readonly NEEDS_WORK_LABEL="needs-work"
readonly REQUIRES_RIG_LABEL="requires-rig"

# The active review worktree is removed on both success and failure. Refuse to
# reuse an existing path: it may contain evidence or edits from an interrupted
# run, and silently replacing those would be destructive.
active_worktree=""
cleanup_worktree() {
    if [[ -n "$active_worktree" ]]; then
        git -C "$ROOT" worktree remove --force "$active_worktree" || {
            echo "WARNING: could not remove review worktree: $active_worktree" >&2
            return 1
        }
        active_worktree=""
    fi
}
trap cleanup_worktree EXIT

# Run from the repository containing this script.
cd "$HERE"

# Verify required commands before doing anything.
for command in gh codex git jq; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "ERROR: required command not found: $command" >&2
        exit 1
    fi
done

# Verify GitHub authentication up front.
if ! gh_retry gh auth status >/dev/null 2>&1; then
    echo "ERROR: gh is not authenticated." >&2
    exit 1
fi

echo "Checking for open PRs with '$CLAUDE_LABEL', without '$CODEX_LABEL', '$NEEDS_WORK_LABEL', or '$REQUIRES_RIG_LABEL'..."

# IMPORTANT:
# This is exactly:
#
#   open
#   AND claude-approved
#   AND NOT codex-approved
#   AND NOT needs-work
#   AND NOT requires-rig
#
# A Codex failure leaves claude-approved in place because that label belongs
# to Claude QA. Excluding needs-work prevents the daemon from reviewing the
# same rejected tip again on every poll. A revised tip re-enters only after
# Claude QA has reviewed it and restored claude-approved.
prs_output="$(
    gh_retry gh pr list \
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
                    and .name != "needs-work"
                    and .name != "requires-rig"
                )
            )
            | .number
        '
)"

prs=()
if [[ -n "$prs_output" ]]; then
    mapfile -t prs <<<"$prs_output"
fi

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
    labels_output="$(
        gh_retry gh pr view "$pr" \
            --json labels \
            --jq '.labels[].name'
    )"

    labels=()
    if [[ -n "$labels_output" ]]; then
        mapfile -t labels <<<"$labels_output"
    fi

    has_claude_approved=false
    has_codex_approved=false
    has_needs_work=false
    has_requires_rig=false

    for label in "${labels[@]}"; do
        case "$label" in
            "$CLAUDE_LABEL")
                has_claude_approved=true
                ;;
            "$CODEX_LABEL")
                has_codex_approved=true
                ;;
            "$NEEDS_WORK_LABEL")
                has_needs_work=true
                ;;
            "$REQUIRES_RIG_LABEL")
                has_requires_rig=true
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

    if [[ "$has_needs_work" == true ]]; then
        echo "Skipping PR #$pr: '$NEEDS_WORK_LABEL' is present."
        continue
    fi

    if [[ "$has_requires_rig" == true ]]; then
        echo "Skipping PR #$pr: '$REQUIRES_RIG_LABEL' is present."
        continue
    fi

    pr_url="$(
        gh_retry gh pr view "$pr" \
            --json url \
            --jq '.url'
    )"

    pr_title="$(
        gh_retry gh pr view "$pr" \
            --json title \
            --jq '.title'
    )"

    echo "Title: $pr_title"
    echo "URL:   $pr_url"

    # Review the exact GitHub tip in an isolated, disposable worktree. Using
    # refs/pull/N/head also works when the PR branch originates from a fork.
    pr_head="$(
        gh_retry gh pr view "$pr" \
            --json headRefOid \
            --jq '.headRefOid'
    )"
    [[ -n "$pr_head" ]] || {
        echo "ERROR: PR #$pr has no head commit." >&2
        exit 1
    }

    review_worktree="$WT_BASE/codex-pr-$pr"
    if [[ -e "$review_worktree" ]]; then
        echo "ERROR: review worktree path already exists: $review_worktree" >&2
        echo "Inspect it, then remove it with: git worktree remove '$review_worktree'" >&2
        exit 1
    fi

    require_space "$review_worktree"
    mkdir -p "$WT_BASE" "$AC_TARGET"
    git fetch -q origin "pull/$pr/head"
    fetched_head="$(git rev-parse FETCH_HEAD)"
    if [[ "$fetched_head" != "$pr_head" ]]; then
        echo "ERROR: PR #$pr moved while preparing the review." >&2
        echo "Expected $pr_head, fetched $fetched_head; retry the run." >&2
        exit 1
    fi
    git worktree add --detach "$review_worktree" "$pr_head" >/dev/null
    active_worktree="$review_worktree"
    link_support "$active_worktree"

    echo "Review worktree: $active_worktree [$pr_head]"
    echo "Starting Codex..."

    # Codex owns the actual review and GitHub state transition.
    # The wrapper intentionally does not parse a PASS/FAIL file and does
    # not maintain local workflow state.
    CARGO_TARGET_DIR="$AC_TARGET" codex exec \
        -c 'approval_policy="never"' \
        -s workspace-write \
        -c 'sandbox_workspace_write.network_access=true' \
        -C "$active_worktree" \
        --add-dir "$AC_TARGET" "
You are the independent Codex QA worker for GitHub PR #$pr.

Read .agents/codex-qa.md before doing anything else.

Also follow the repository's normal AGENTS.md instructions.

Source bin/common.sh and run every GitHub CLI operation through gh_retry.
Inspect the PR, linked issue, PR discussion,
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
    cleanup_worktree
done

echo
echo "Codex QA run complete."
