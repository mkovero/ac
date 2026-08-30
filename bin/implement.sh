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
sparse_trim "$wt"
cd "$wt"
link_support "$wt"

mapfile -t files < <(manifest_of "$n")

if (( ${#files[@]} == 0 )); then
  echo "no file manifest on #$n — run bin/design.sh $n first" >&2
  echo "  (AC_NO_MANIFEST=1 to implement blind, at roughly 3x the cost)" >&2
  [[ -n ${AC_NO_MANIFEST:-} ]] || exit 1
fi

for f in "${files[@]}"; do
  [[ -e $f ]] || echo "note: manifest names $f — not in tree, assuming new file" >&2
done

AC_TAG="issue-$n" run developer "Implement issue #$n in $AC_REPO.

The architect's design comment names the files this change touches. That list is your scope:

$(printf '%s\n' "${files[@]}")

Read those files first, in that order, before anything else. Do not sweep the tree. Do not Glob or Grep to find what to work on — the search has already been done and its result is above. Grep is for locating a symbol inside a file already on this list.

A file you need that is not on the list is a finding about the design, not a gap for you to fill. Stop, comment on the issue with the path and why it is needed, apply needs-design, and end the run. Adding it silently is the exact failure this list exists to prevent." "$@"

echo "worktree: $wt   branch: issue-$n" >&2
