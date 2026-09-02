#!/usr/bin/env bash
# implement.sh <issue> [--continue] [--fg]
#
# Developer role against one issue, in its own worktree so several can run at
# once. For a PR already sent back by QA use revise.sh — this script creates
# branches and would reset an existing one.

source "$(dirname "$0")/common.sh"
n="${1:?usage: implement.sh <issue> [--continue] [--fg]}"; shift || true
wt="$WT_BASE/issue-$n"
continue_mode="${AC_CONTINUE:-}"
declare -a run_args=()
for arg in "$@"; do
  case "$arg" in
    --continue) continue_mode=1 ;;
    *) run_args+=("$arg") ;;
  esac
done

# Reuse only the exact empty branch/worktree shape left by a failed preflight.
# Anything dirty or ahead may contain a cut-off implementation and is refused.
if git show-ref -q "refs/heads/issue-$n"; then
  existing="$(worktree_of_branch "issue-$n")"
  dirty=""; ahead="$(git rev-list --count "origin/main..issue-$n" 2>/dev/null || echo 0)"
  [[ -n $existing && -d $existing ]] && dirty="$(git -C "$existing" status --porcelain)"
  if [[ -n $dirty || $ahead != 0 ]] && [[ -z $continue_mode ]]; then
    echo "branch issue-$n contains or may contain work — use revise.sh or inspect it" >&2
    exit 1
  fi
  if [[ -n $existing ]]; then
    wt="$existing"
  else
    require_space "$wt" || exit 1
    git worktree add "$wt" "issue-$n" >/dev/null
  fi
  [[ -n $dirty || $ahead != 0 ]] || git -C "$wt" merge -q --ff-only origin/main
else
  require_space "$wt" || exit 1
  git fetch -q origin main
  git worktree add -B "issue-$n" "$wt" origin/main >/dev/null
fi

sparse_trim "$wt"
cd "$wt"
link_support "$wt"

manifest="$(manifest_of "$n")" \
  || { echo "cannot read architect manifest for #$n" >&2; exit 1; }
files=()
[[ -n $manifest ]] && mapfile -t files <<< "$manifest"

if (( ${#files[@]} == 0 )); then
  echo "no file manifest on #$n — run bin/design.sh $n first" >&2
  echo "  (AC_NO_MANIFEST=1 to implement blind, at roughly 3x the cost)" >&2
  [[ -n ${AC_NO_MANIFEST:-} ]] || exit 1
fi

for f in "${files[@]}"; do
  [[ -e $f ]] || echo "note: manifest names $f — not in tree, assuming new file" >&2
done

if [[ -n $continue_mode ]]; then
  task="Continue the interrupted implementation of issue #$n in $AC_REPO.

This branch already contains uncommitted or committed work from an earlier
developer session that ended before opening a PR. Inspect git status and the
existing diff first. Preserve correct work, finish the remaining implementation
and verification, then commit, push, and open the PR. Do not restart from a
clean tree and do not discard work merely because another provider produced it."
else
  task="Implement issue #$n in $AC_REPO."
fi

AC_TAG="issue-$n${continue_mode:+-continue}" run developer "$task

The architect's design comment names the files this change touches. That list is your scope:

$(printf '%s\n' "${files[@]}")

Read those files first, in that order, before anything else. Do not sweep the tree. Do not Glob or Grep to find what to work on — the search has already been done and its result is above. Grep is for locating a symbol inside a file already on this list.

A file you need that is not on the list is a finding about the design, not a gap for you to fill. Stop, comment on the issue with the path and why it is needed, apply needs-design, and end the run. Adding it silently is the exact failure this list exists to prevent." "${run_args[@]}"

echo "worktree: $wt   branch: issue-$n" >&2
