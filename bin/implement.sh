#!/usr/bin/env bash
# implement.sh <issue> [--fg]
#
# Developer role against one issue, in its own worktree so several can run at
# once. For a PR already sent back by QA use revise.sh — this script creates
# branches and would reset an existing one.

source "$(dirname "$0")/common.sh"
n="${1:?usage: implement.sh <issue> [--fg]}"; shift || true
wt="$WT_BASE/issue-$n"

# -B force-resets, so an existing branch would lose its commits and with them
# the PR's history. Refuse instead.
if git show-ref -q "refs/heads/issue-$n"; then
  echo "branch issue-$n exists — use revise.sh for QA feedback, or delete it first" >&2
  exit 1
fi

require_space "$wt" || exit 1
git fetch -q origin main
git worktree add -B "issue-$n" "$wt" origin/main >/dev/null
cd "$wt"
link_support "$wt"

AC_TAG="issue-$n" run developer "Implement issue #$n in $AC_REPO." "$@"
echo "worktree: $wt   branch: issue-$n" >&2
