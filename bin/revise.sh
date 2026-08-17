#!/usr/bin/env bash
# revise.sh <pr> [--fg]
#
# Developer role against QA feedback on an existing PR. Branch comes from the
# PR itself, never from origin/main — resetting it would drop the commits the
# PR is made of.

source "$(dirname "$0")/common.sh"
n="${1:?usage: revise.sh <pr> [--fg]}"; shift || true

branch="$(gh_retry gh pr view "$n" -R "$AC_REPO" --json headRefName --jq .headRefName)"
[[ -n $branch ]] || { echo "no branch for PR #$n" >&2; exit 1; }
require_space "$WT_BASE/$branch" || exit 1

# A revise usually runs against the branch the implement session left checked
# out. Reuse that worktree — a branch cannot be checked out twice, and both
# fallbacks in the old version failed on exactly that case.
wt="$(ensure_worktree "$branch" "$WT_BASE/$branch")" \
  || { echo "cannot get a worktree for $branch" >&2; exit 1; }
cd "$wt"
git pull -q --ff-only 2>/dev/null || true
link_support "$wt"

AC_TAG="pr-$n-rev" run developer "PR #$n in $AC_REPO is labelled needs-work. \
Read the agent:qa review comment on it and address every point raised. This is \
a revision: the branch and the PR already exist — commit and push to this \
branch, do not open a new PR and do not change labels. Reply to the review \
points in a PR comment so the next QA pass can see what you did and why. Any \
point you disagree with, say so there rather than silently leaving it." "$@"

echo "worktree: $wt   branch: $branch" >&2
echo "when pushed: gh_retry gh pr edit $n -R $AC_REPO --remove-label needs-work --add-label in-review" >&2
