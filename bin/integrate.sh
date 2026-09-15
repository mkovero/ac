#!/usr/bin/env bash
# integrate.sh <pr> [--fg]
#
# Bring an open PR up to current main after an earlier epic child merges.
# Conflict resolution is a developer task; the runner owns the disposable
# worktree, push, and invalidation of commit-bound approvals.

source "$(dirname "$0")/common.sh"
n="${1:?usage: integrate.sh <pr> [--fg]}"; shift || true

branch="$(gh_retry gh pr view "$n" -R "$AC_REPO" --json headRefName --jq .headRefName)"
old_head="$(gh_retry gh pr view "$n" -R "$AC_REPO" --json headRefOid --jq .headRefOid)"
[[ -n $branch && -n $old_head ]] || { echo "cannot resolve PR #$n branch" >&2; exit 1; }

wt="$WT_BASE/integrate-pr-$n"
[[ ! -e $wt ]] || {
  echo "integration worktree already exists: $wt" >&2
  echo "inspect or resume it; it may contain unresolved conflict work" >&2
  exit 1
}

require_space "$wt"
git fetch -q origin main "$branch"
[[ $(git rev-parse "origin/$branch") == "$old_head" ]] || {
  echo "PR #$n moved while preparing integration; retry" >&2
  exit 1
}
git worktree add -B "integrate-pr-$n" "$wt" "origin/$branch" >/dev/null
cd "$wt"
link_support "$wt"

if git merge --no-edit origin/main; then
  echo "<runner/integration> PR #$n merged current main without conflicts"
  ( cd ac-rs && cargo test --workspace )
  ( cd ac-rs && cargo clippy -- -D warnings )
  ( cd ac-rs && cargo fmt --check )
else
  echo "<runner/integration> PR #$n requires developer conflict resolution"
  AC_TAG="pr-$n-integrate" run developer "Integrate current origin/main into PR #$n in $AC_REPO.

This is integration mode. A merge is already in progress in this disposable
worktree. Inspect every unmerged path and both sides of each conflict. Preserve
the reviewed intent of the PR and main, resolve only integration conflicts,
run the full workspace verification gate, and commit the merge. Do not push;
the runner verifies and pushes the result. If the two designs cannot coexist,
abort the merge, apply needs-design on the linked issue, and stop." "$@"
fi

[[ -z $(git diff --name-only --diff-filter=U) ]] || {
  echo "PR #$n still has unresolved paths; preserving $wt" >&2
  exit 1
}
[[ -z $(git status --porcelain) ]] || {
  echo "PR #$n integration left uncommitted work; preserving $wt" >&2
  exit 1
}
new_head="$(git rev-parse HEAD)"
[[ $new_head != "$old_head" ]] || {
  echo "PR #$n integration produced no commit; preserving $wt" >&2
  exit 1
}

git push origin "HEAD:$branch"
remote_head=""
for attempt in {1..10}; do
  remote_head="$(gh_retry gh pr view "$n" -R "$AC_REPO" --json headRefOid --jq .headRefOid)"
  [[ $remote_head == "$new_head" ]] && break
  (( attempt == 10 )) || sleep 2
done
[[ $remote_head == "$new_head" ]] || {
  echo "PR #$n remote head did not reach integration commit $new_head" >&2
  exit 1
}

gh_retry gh pr edit "$n" -R "$AC_REPO" --remove-label claude-approved \
  --remove-label codex-approved --remove-label needs-work --add-label in-review >/dev/null
git worktree remove --force "$wt"
echo "<runner/integration> PR #$n integrated main at $new_head; QA approvals invalidated"
