#!/usr/bin/env bash
# revise.sh <pr> [--fg]
#
# Developer role against QA feedback on an existing PR. Branch comes from the
# PR itself, never from origin/main — resetting it would drop the commits the
# PR is made of.

source "$(dirname "$0")/common.sh"
n="${1:?usage: revise.sh <pr> [--fg]}"; shift || true

branch="$(gh pr view "$n" -R "$AC_REPO" --json headRefName --jq .headRefName)"
[[ -n $branch ]] || { echo "no branch for PR #$n" >&2; exit 1; }
wt="$WT_BASE/$branch"

if [[ -d $wt ]]; then
  cd "$wt" && git pull -q --ff-only 2>/dev/null || true
else
  git fetch -q origin "$branch"
  git worktree add "$wt" "$branch" >/dev/null 2>&1 \
    || git worktree add --track -b "$branch" "$wt" "origin/$branch" >/dev/null
  cd "$wt"
fi
link_support "$wt"

AC_TAG="pr-$n-rev" run developer "PR #$n in $AC_REPO is labelled needs-work. \
Read the agent:qa review comment on it and address every point raised. This is \
a revision: the branch and the PR already exist — commit and push to this \
branch, do not open a new PR and do not change labels. Reply to the review \
points in a PR comment so the next QA pass can see what you did and why. Any \
point you disagree with, say so there rather than silently leaving it." "$@"

echo "worktree: $wt   branch: $branch" >&2
echo "when pushed: gh pr edit $n -R $AC_REPO --remove-label needs-work --add-label in-review" >&2
