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
STATE=""          # outcome of the last drive(), read by the epic runner
STEPS="${AC_STEPS:-8}"

fg=""; ids=()
for a in "$@"; do
  case "$a" in --fg) fg="--fg" ;; *) ids+=("$a") ;; esac
done
[[ ${#ids[@]} -gt 0 ]] || { echo "usage: master.sh <issue-id>... [--fg]" >&2; exit 1; }

# No 2>/dev/null: gh_retry already separates real errors from transient ones,
# and swallowing the message here turns an API outage into an empty label set —
# which reads as "no needs-work", which reads as "approved".
labels()    { gh_retry gh issue view "$1" -R "$AC_REPO" --json labels --jq '.labels[].name'; }
pr_labels() { gh_retry gh pr view "$1" -R "$AC_REPO" --json labels --jq '.labels[].name'; }
has()       { printf '%s\n' "$2" | grep -qx "$1"; }

# Match on head branch first — developer.md step 2 specifies issue-{N}-{slug}.
# Fall back to the PR body's closing reference, because not every branch in
# this repo follows that convention. Never match on title: titles get edited.
pr_for() {
  local n="$1" pr
  pr=$(gh_retry gh pr list -R "$AC_REPO" --state open --json number,headRefName --jq \
    "[.[] | select((.headRefName | startswith(\"issue-$n-\")) or (.headRefName == \"issue-$n\"))] | .[0].number // empty")
  [[ -n $pr ]] && { printf '%s\n' "$pr"; return; }
  gh_retry gh pr list -R "$AC_REPO" --state open --json number,body --jq \
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
  local n="$1" pr="$2" round=0 ls before after head mark ev
  while (( round <= ROUNDS )); do
    ls="$(pr_labels "$pr")" || { echo "  #$n PR #$pr: cannot read labels — stopping rather than guessing"; return 1; }
    has needs-discussion "$ls" && { echo "  #$n PR #$pr: qa escalated — yours"; STATE=needs-human; return 0; }

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
      gh_retry gh pr edit "$pr" -R "$AC_REPO" \
        --remove-label needs-work --add-label in-review >/dev/null 2>&1 || true
      ls="$(pr_labels "$pr")"
    fi

    head="$(gh_retry gh pr view "$pr" -R "$AC_REPO" --json headRefOid --jq .headRefOid)"
    mark="$AC_LOG_DIR/reviewed-pr-$pr.sha"

    # Already reviewed at this exact tip and qa raised nothing: that is a pass.
    ev="$(qa_evidence "$pr")" || { echo "  #$n: cannot count qa output — stopping"; return 1; }
    if [[ -f $mark && "$(cat "$mark")" == "$head" ]] && (( ev > 0 )); then
      echo "  #$n PR #$pr: qa reviewed $head and raised nothing — yours to merge"
      STATE=awaiting-merge; return 0
    fi

    echo "  #$n PR #$pr: qa review"
    before="$(qa_evidence "$pr")" || { echo "  #$n: cannot count qa output — stopping"; return 1; }
    "$BIN/review.sh" "$pr" $fg || { echo "  #$n: review failed"; return 1; }
    after="$(qa_evidence "$pr")" || { echo "  #$n: cannot count qa output — review may have succeeded, check the PR"; return 1; }

    if (( after <= before )); then
      echo "  #$n PR #$pr: qa posted nothing — inconclusive, NOT approved"
      echo "     read the session log before believing this PR passed."
      return 1
    fi

    ls="$(pr_labels "$pr")" || { echo "  #$n PR #$pr: cannot read labels — stopping rather than guessing"; return 1; }
    has needs-discussion "$ls" && { echo "  #$n PR #$pr: qa escalated — yours"; STATE=needs-human; return 0; }
    has needs-work "$ls"       || { echo "  #$n PR #$pr: qa reviewed and raised nothing — yours to merge"; STATE=awaiting-merge; return 0; }
  done
}

drive() {
  local n="$1" step=0 ls pr
  local ran_design=0 ran_ux=0

  while (( step < STEPS )); do
    (( ++step ))
    ls="$(labels "$n")" || { echo "  #$n: cannot read issue"; return 1; }

    has blocked "$ls"          && { echo "  #$n: blocked — lift condition is in the comment that applied it"; STATE=blocked; return 0; }
    has needs-discussion "$ls" && { echo "  #$n: needs-discussion — yours to decide"; STATE=needs-human; return 0; }

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
      local wt="$WT_BASE/issue-$n" dirty="" ahead=""
      [[ -d $wt ]] && dirty="$(git -C "$wt" status --porcelain 2>/dev/null | wc -l)"
      ahead="$(git rev-list --count "origin/main..issue-$n" 2>/dev/null || echo 0)"

      echo "  #$n: branch issue-$n exists but no open PR."
      if (( ${dirty:-0} > 0 )) || (( ahead > 0 )); then
        # Work is present. Deleting here throws away a whole run.
        echo "     it has work: ${dirty:-0} uncommitted file(s), $ahead commit(s) ahead of main."
        echo "     an earlier run was probably cut off. do NOT delete it. resume:"
        echo "       jq -r 'select(.type==\"system\") | .session_id // empty' \\"
        echo "         ${AC_LOG_DIR}/*developer-issue-$n.jsonl | head -1"
        echo "       cd $wt && claude --resume <id>"
      else
        echo "     it is empty — no commits, nothing uncommitted. safe to clear:"
        echo "       git worktree remove --force $wt; git branch -D issue-$n"
      fi
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

# Children of an epic, in the order the epic lists them. Three shapes, in
# order of preference: the sub-issues API; a markdown table whose first column
# is `| #NNN |` and whose last column is blocked-by; task-list checkboxes.
# Order is the sequencing — never sort or dedupe it into a different order.
epic_body() { gh_retry gh issue view "$1" -R "$AC_REPO" --json body --jq .body 2>/dev/null; }

# emits: "<issue> <blocker> <blocker> ..."  (blockers may be empty)
epic_rows() {
  epic_body "$1" | awk -F'|' '
    /^[[:space:]]*\|[[:space:]]*#[0-9]+[[:space:]]*\|/ {
      n = $2; gsub(/[^0-9]/, "", n)
      b = $(NF-1); gsub(/[^0-9 ]/, " ", b)
      print n, b
    }'
}

children() {
  local out
  out=$(gh_retry gh api "repos/$AC_REPO/issues/$1/sub_issues" --jq '.[].number' 2>/dev/null || true)
  [[ -n $out ]] && { printf '%s\n' "$out"; return; }
  out=$(epic_rows "$1" | awk '{print $1}')
  [[ -n $out ]] && { printf '%s\n' "$out"; return; }
  epic_body "$1" \
    | grep -oE '^[[:space:]]*-[[:space:]]*\[[ xX]\][[:space:]]*#[0-9]+' \
    | grep -oE '[0-9]+$'
}

# Blockers for one child, from the epic's table. Empty when the table gives
# none, or when the epic does not use the table shape at all.
blockers_of() {
  epic_rows "$1" | awk -v c="$2" '$1 == c { $1 = ""; print }'
}

is_epic() {
  printf '%s\n' "$(labels "$1")" | grep -qx epic && return 0
  [[ -n "$(children "$1")" ]]
}

drive_epic() {
  local e="$1" kids c st
  mapfile -t kids < <(children "$e")
  (( ${#kids[@]} )) || { echo "  #$e: no sub-issues or task-list refs found"; return 0; }
  echo "  #$e: epic with ${#kids[@]} children — $(printf '#%s ' "${kids[@]}")"

  for c in "${kids[@]}"; do
    st="$(gh_retry gh issue view "$c" -R "$AC_REPO" --json state --jq .state 2>/dev/null || echo UNKNOWN)"
    if [[ $st == CLOSED ]]; then echo "  #$c: closed, skipping"; continue; fi

    # The epic's blocked-by column is a real dependency, not a hint. A child
    # whose blocker is still open would branch from a main without it.
    local blk open_blk=""
    for blk in $(blockers_of "$e" "$c"); do
      [[ "$(gh_retry gh issue view "$blk" -R "$AC_REPO" --json state --jq .state 2>/dev/null)" == CLOSED ]] \
        || open_blk+="#$blk "
    done
    if [[ -n $open_blk ]]; then
      echo "  #$c: blocked by $open_blk— skipping"
      continue
    fi

    echo "-- #$c (child of #$e)"
    STATE=""
    drive "$c" || { echo "  #$c: aborted — stopping epic"; return 1; }

    case "$STATE" in
      awaiting-merge)
        echo
        echo "  #$c is ready for your merge. Stopping here."
        echo "  Later children branch from main and would not see #$c's work."
        echo "  Merge it, then rerun: master.sh $e"
        [[ -n ${KEEP_GOING:-} ]] || return 0 ;;
      needs-human|blocked)
        echo "  #$c needs you — stopping epic."
        [[ -n ${KEEP_GOING:-} ]] || return 0 ;;
    esac
  done
  echo "  #$e: all children processed"
}

gh_up || exit 1

for id in "${ids[@]}"; do
  echo "== issue #$id"
  if is_epic "$id"; then
    drive_epic "$id" || true
  else
    STATE=""
    drive "$id" || echo "  #$id: aborted"
  fi
done

echo
echo "nothing above was merged or closed. board.sh for current state."
