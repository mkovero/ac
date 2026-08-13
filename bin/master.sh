#!/usr/bin/env bash
# master.sh <issue-id>... [--fg]
#
# Drives an issue through whatever its labels say it needs:
#
#   needs-design → architect    needs-ux → ux
#   ready-to-implement → developer → qa → (needs-work → developer → qa)...
#
# It does NOT merge, close, or resolve a disagreement. Those are your gates.
# It stops on needs-discussion, on blocked, on qa approval, and when a role
# leaves its own label in place.
#
#   AC_ROUNDS=3   max dev→qa cycles before handing back (default 3)
#   AC_STEPS=8    max state transitions per issue, loop backstop (default 8)
#
# Verify label names first — a wrong one makes this do nothing while looking
# like it worked:  gh label list -R mkovero/ac

source "$(dirname "$0")/common.sh"
BIN="$(cd "$(dirname "$0")" && pwd)"
ROUNDS="${AC_ROUNDS:-3}"
STEPS="${AC_STEPS:-8}"

fg=""; ids=()
for a in "$@"; do
  case "$a" in --fg) fg="--fg" ;; *) ids+=("$a") ;; esac
done
[[ ${#ids[@]} -gt 0 ]] || { echo "usage: master.sh <issue-id>... [--fg]" >&2; exit 1; }

labels()    { gh issue view "$1" -R "$AC_REPO" --json labels --jq '.labels[].name' 2>/dev/null; }
pr_labels() { gh pr view "$1" -R "$AC_REPO" --json labels --jq '.labels[].name' 2>/dev/null; }
has()       { printf '%s\n' "$2" | grep -qx "$1"; }

# Match on head branch first — developer.md step 2 specifies issue-{N}-{slug}.
# Fall back to the PR body's closing reference, because not every branch in
# this repo follows that convention. Never match on title: titles get edited.
pr_for() {
  local n="$1" pr
  pr=$(gh pr list -R "$AC_REPO" --state open --json number,headRefName --jq \
    "[.[] | select((.headRefName | startswith(\"issue-$n-\")) or (.headRefName == \"issue-$n\"))] | .[0].number // empty")
  [[ -n $pr ]] && { printf '%s\n' "$pr"; return; }
  gh pr list -R "$AC_REPO" --state open --json number,body --jq \
    "[.[] | select(.body // \"\" | test(\"[Cc]loses +#$n\\\\b\"))] | .[0].number // empty"
}

# A branch with no open PR is a failed earlier run, not a fresh start.
stale_branch() {
  git show-ref -q "refs/heads/issue-$1"
}

# Absence of needs-work is NOT approval: a session that crashed, hit its turn
# limit, or ended without posting leaves the labels exactly as an approving one
# does. Require positive evidence that QA spoke. qa_evidence() is in common.sh.
qa_comments() { qa_evidence "$1"; }

qa_loop() {
  local n="$1" pr="$2" round=0 ls before after head mark
  while (( round <= ROUNDS )); do
    ls="$(pr_labels "$pr")"
    has needs-discussion "$ls" && { echo "  #$n PR #$pr: qa escalated — yours"; return 0; }

    # Already labelled needs-work: the verdict is in, revise before reviewing
    # again. Reviewing first would re-review a tip qa has already judged.
    if has needs-work "$ls"; then
      (( ++round ))
      if (( round > ROUNDS )); then
        echo "  #$n PR #$pr: still needs-work after $ROUNDS rounds — stopping"
        echo "     two agents failing to converge is signal. read the reviews."
        return 0
      fi
      echo "  #$n PR #$pr: revising (round $round)"
      "$BIN/revise.sh" "$pr" $fg || { echo "  #$n: revise failed"; return 1; }
      gh pr edit "$pr" -R "$AC_REPO" \
        --remove-label needs-work --add-label in-review >/dev/null 2>&1 || true
      ls="$(pr_labels "$pr")"
    fi

    head="$(gh pr view "$pr" -R "$AC_REPO" --json headRefOid --jq .headRefOid)"
    mark="$AC_LOG_DIR/reviewed-pr-$pr.sha"

    # Already reviewed at this exact tip and qa raised nothing: that is a pass.
    if [[ -f $mark && "$(cat "$mark")" == "$head" ]] && (( $(qa_evidence "$pr") > 0 )); then
      echo "  #$n PR #$pr: qa reviewed $head and raised nothing — yours to merge"
      return 0
    fi

    echo "  #$n PR #$pr: qa review"
    before="$(qa_evidence "$pr")"
    "$BIN/review.sh" "$pr" $fg || { echo "  #$n: review failed"; return 1; }
    after="$(qa_evidence "$pr")"

    if (( after <= before )); then
      echo "  #$n PR #$pr: qa posted nothing — inconclusive, NOT approved"
      echo "     read the session log before believing this PR passed."
      return 1
    fi

    ls="$(pr_labels "$pr")"
    has needs-discussion "$ls" && { echo "  #$n PR #$pr: qa escalated — yours"; return 0; }
    has needs-work "$ls"       || { echo "  #$n PR #$pr: qa reviewed and raised nothing — yours to merge"; return 0; }
  done
}

drive() {
  local n="$1" step=0 ls pr
  local ran_design=0 ran_ux=0

  while (( step < STEPS )); do
    (( ++step ))
    ls="$(labels "$n")" || { echo "  #$n: cannot read issue"; return 1; }

    has blocked "$ls"          && { echo "  #$n: blocked — lift condition is in the comment that applied it"; return 0; }
    has needs-discussion "$ls" && { echo "  #$n: needs-discussion — yours to decide"; return 0; }

    # ux step 6 runs first: it clears needs-ux but defers ready-to-implement to
    # architect when both labels are set, so design must be the later gate.
    if has needs-ux "$ls"; then
      (( ran_ux )) && { echo "  #$n: ux ran, needs-ux still set — read its comment"; return 0; }
      ran_ux=1; echo "  #$n: ux"
      "$BIN/ux.sh" "$n" $fg || { echo "  #$n: ux failed"; return 1; }
      continue
    fi

    # A role that ran and left its label is not a state to retry. Retrying is
    # how a loop turns a spec gap into an infinite one.
    if has needs-design "$ls"; then
      (( ran_design )) && { echo "  #$n: architect ran, needs-design still set — read its comment"; return 0; }
      ran_design=1; echo "  #$n: architect"
      "$BIN/design.sh" "$n" $fg || { echo "  #$n: design failed"; return 1; }
      continue
    fi

    pr="$(pr_for "$n")"
    if [[ -n $pr ]]; then
      echo "  #$n: PR #$pr"
      qa_loop "$n" "$pr"; return
    fi

    if stale_branch "$n"; then
      echo "  #$n: branch issue-$n exists but no open PR."
      echo "     earlier run did not open one, or the PR was closed. inspect, then:"
      echo "     git worktree remove --force $WT_BASE/issue-$n; git branch -D issue-$n"
      return 0
    fi

    has ready-to-implement "$ls" \
      || { echo "  #$n: no PR, not ready-to-implement — nothing to do"; return 0; }

    echo "  #$n: implementing"
    "$BIN/implement.sh" "$n" $fg || { echo "  #$n: implement failed"; return 1; }
    pr="$(pr_for "$n")"
    [[ -n $pr ]] || { echo "  #$n: no PR opened — check the session log"; return 1; }
    echo "  #$n: opened PR #$pr"
    qa_loop "$n" "$pr"; return
  done

  echo "  #$n: hit step limit ($STEPS) — labels are cycling, look at it"
}

for id in "${ids[@]}"; do
  echo "== issue #$id"
  drive "$id" || echo "  #$id: aborted"
done

echo
echo "nothing above was merged or closed. board.sh for current state."
