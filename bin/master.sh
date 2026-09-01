#!/usr/bin/env bash
# master.sh <issue-id>... [--fg]
#
# Drives an issue through whatever its labels say it needs:
#
#   nothing routed it yet → triage
#   needs-design → architect    needs-ux → ux
#   ready-to-implement → developer → qa → (needs-work → developer → qa)...
#
# qa or developer may decide the DESIGN is what is wrong, not the code, and put
# needs-design or needs-ux back on the issue. That is not a failure state: this
# picks it up, re-drives architect or ux, and returns to the PR with a forced
# full re-review — the diff may already carry an approval of a design that no
# longer stands.
#
# It does NOT merge, close, or resolve a disagreement. Those are your gates.
# It stops on needs-discussion, on needs-clarification, on blocked, on qa
# approval, and when a role leaves its own label in place.
#
#   AC_ROUNDS=3          max dev→qa cycles before handing back (default 3)
#   AC_STEPS=8           max state transitions per issue, loop backstop (default 8)
#   AC_DESIGN_PASSES=2   max architect passes per issue per run (default 2)
#   AC_UX_PASSES=2       max ux passes per issue per run (default 2)
#   AC_NO_TRIAGE=1       never triage; an unrouted issue is nothing to do
#
# Verify label names first — a wrong one makes this do nothing while looking
# like it worked:  gh label list -R mkovero/ac

source "$(dirname "$0")/common.sh"
BIN="$(cd "$(dirname "$0")" && pwd)"
ROUNDS="${AC_ROUNDS:-3}"
STATE=""          # outcome of the last drive(), read by the epic runner
STEPS="${AC_STEPS:-8}"
DESIGN_PASSES="${AC_DESIGN_PASSES:-2}"
UX_PASSES="${AC_UX_PASSES:-2}"

# dev→qa rounds, counted per ISSUE rather than per qa_loop() call. A design
# handback re-enters qa_loop, and a counter local to it would reset there —
# turning ROUNDS from a bound into a suggestion.
qa_round=0

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

# Has anything routed this issue? Any one of these labels means triage,
# architect, ux or you already decided where it goes. blocked, needs-discussion
# and needs-clarification are not listed: drive() returns on them earlier, so
# listing them here would be a branch nothing can reach.
routed() {
  local ls="$1" l
  for l in ready-to-implement needs-design needs-ux epic; do
    has "$l" "$ls" && return 0
  done
  return 1
}

# Positive evidence that triage spoke, same shape and same reason as
# qa_evidence(): a run that crashed or hit its turn limit leaves an issue
# looking exactly like one triage never touched, and an API failure must not
# read as "no spec". Echoes a count or fails; never 0-on-error.
triage_evidence() {
  local c
  c=$(gh_retry gh issue view "$1" -R "$AC_REPO" --json comments \
      --jq '[.comments[] | select(.body | test("agent: *triage"; "i"))] | length') || return 1
  echo "${c:-0}"
}

# qa_loop <issue> <pr> [force]
# force=full → ignore the reviewed-SHA cache and review the whole PR again.
# drive() sets it when architect or ux has just changed the design under a diff
# that may already carry an approval of the design it replaced.
qa_loop() {
  local n="$1" pr="$2" force="${3:-}" ls ils before after head mark ev pre post
  # Not every exit path sets STATE, and drive() re-enters this function after a
  # handback. A STATE left over from the previous entry would read as a second
  # handback and loop until the step limit — which looks like cycling labels
  # and is not.
  STATE=""
  while (( qa_round <= ROUNDS )); do
    ls="$(pr_labels "$pr")" || { echo "  #$n PR #$pr: cannot read labels — stopping rather than guessing"; return 1; }
    has needs-discussion "$ls" && { echo "  #$n PR #$pr: qa escalated — yours"; STATE=needs-human; return 0; }

    # qa or developer can conclude that the design is wrong rather than the
    # code, and send the issue back by re-applying needs-design or needs-ux.
    # Read the ISSUE: those are issue labels (AGENTS.md label schema) and
    # architect and ux act on the issue, not on the PR. Checked before the
    # needs-work branch below, so a PR carrying both goes to the design
    # question first instead of spending a revise round on the old one.
    ils="$(labels "$n")" || { echo "  #$n: cannot read issue labels — stopping rather than guessing"; return 1; }
    if has needs-design "$ils"; then
      echo "  #$n PR #$pr: sent back to architect (needs-design on the issue)"
      STATE=needs-design; return 0
    fi
    if has needs-ux "$ils"; then
      echo "  #$n PR #$pr: sent back to ux (needs-ux on the issue)"
      STATE=needs-ux; return 0
    fi

    # Already labelled needs-work: the verdict is in, revise before reviewing
    # again. Reviewing first would re-review a tip qa has already judged.
    if has needs-work "$ls"; then
      (( ++qa_round ))
      if (( qa_round > ROUNDS )); then
        echo "  #$n PR #$pr: still needs-work after $ROUNDS rounds — stopping"
        echo "     two agents failing to converge is signal. read the reviews."
        return 0
      fi
      if has requires-rig "$ls"; then
        echo "  #$n PR #$pr: carries requires-rig — revising the code does not"
        echo "     retire the measurement; the label stays for you to clear."
      fi
      echo "  #$n PR #$pr: revising (round $qa_round)"
      pre="$(gh_retry gh pr view "$pr" -R "$AC_REPO" --json headRefOid --jq .headRefOid)" \
        || { echo "  #$n: cannot read the tip — not starting a revise"; return 1; }
      "$BIN/revise.sh" "$pr" $fg || { echo "  #$n: revise failed"; return 1; }
      post="$(gh_retry gh pr view "$pr" -R "$AC_REPO" --json headRefOid --jq .headRefOid)" \
        || { echo "  #$n: cannot read the tip — check the PR by hand"; return 1; }

      # A revise that pushed nothing is the developer saying the block is not
      # code-fixable. Clearing needs-work here would be this script overruling
      # that on the developer's behalf — and worse, the reviewed-SHA cache below
      # would then see a tip qa has already reviewed with a comment on it and
      # report "raised nothing", which is how a request-changes verdict turns
      # into "yours to merge". Leave the label. Stop.
      if [[ $pre == "$post" ]]; then
        echo "  #$n PR #$pr: revise pushed nothing — tip is still $post"
        echo "     needs-work stays. re-reviewing an identical tip cannot change"
        echo "     the verdict, so the block is one only you can clear: a rig"
        echo "     measurement, acceptance of an assumed criterion, a design call."
        echo "     read the developer's PR comment for which."
        STATE=needs-human; return 0
      fi

      gh_retry gh pr edit "$pr" -R "$AC_REPO" \
        --remove-label needs-work --add-label in-review >/dev/null 2>&1 || true
      ls="$(pr_labels "$pr")"
    fi

    head="$(gh_retry gh pr view "$pr" -R "$AC_REPO" --json headRefOid --jq .headRefOid)"
    mark="$AC_LOG_DIR/reviewed-pr-$pr.sha"

    # Already reviewed at this exact tip. requires-rig is a pre-approval stop;
    # after a human clears it, absence of claude-approved forces a full same-tip
    # QA pass so the measurement record becomes part of the approval evidence.
    # Unless force is set — then the tip is unchanged but the design under it
    # is not, and the cached approval is an approval of a superseded spec.
    ev="$(qa_evidence "$pr")" || { echo "  #$n: cannot count qa output — stopping"; return 1; }
    if [[ -z $force && -f $mark && "$(cat "$mark")" == "$head" ]] && (( ev > 0 )); then
      if has requires-rig "$ls"; then
        echo "  #$n PR #$pr: tree QA complete — REQUIRES RIG before approval"
        echo "     a measurement is outstanding. the label is human-clear only:"
        echo "     read the review's 'rig verification required' field, run the"
        echo "     session, then: gh pr edit $pr -R $AC_REPO --remove-label requires-rig"
        STATE=needs-rig; return 0
      fi
      if has claude-approved "$ls"; then
        echo "  #$n PR #$pr: qa approved $head — yours to merge"
        STATE=awaiting-merge; return 0
      fi
      echo "  #$n PR #$pr: rig gate cleared — full QA must incorporate its evidence"
      force=full
    fi

    echo "  #$n PR #$pr: qa review${force:+ (full — design changed since the last pass)}"
    before="$(qa_evidence "$pr")" || { echo "  #$n: cannot count qa output — stopping"; return 1; }
    "$BIN/review.sh" "$pr" ${force:+--full} $fg || { echo "  #$n: review failed"; return 1; }
    force=""   # one forced pass; later rounds go back to reviewing the delta
    after="$(qa_evidence "$pr")" || { echo "  #$n: cannot count qa output — review may have succeeded, check the PR"; return 1; }

    if (( after <= before )); then
      echo "  #$n PR #$pr: qa posted nothing — inconclusive, NOT approved"
      echo "     read the session log before believing this PR passed."
      return 1
    fi

    ls="$(pr_labels "$pr")" || { echo "  #$n PR #$pr: cannot read labels — stopping rather than guessing"; return 1; }
    has needs-discussion "$ls" && { echo "  #$n PR #$pr: qa escalated — yours"; STATE=needs-human; return 0; }

    # The review that just ran may itself be the handback. Catch it here rather
    # than a lap later: the needs-work branch above would otherwise spend a
    # revise round answering a review whose own verdict was "wrong design".
    ils="$(labels "$n")" || { echo "  #$n: cannot read issue labels — stopping rather than guessing"; return 1; }
    if has needs-design "$ils"; then
      echo "  #$n PR #$pr: qa sent it back to architect (needs-design on the issue)"
      STATE=needs-design; return 0
    fi
    if has needs-ux "$ils"; then
      echo "  #$n PR #$pr: qa sent it back to ux (needs-ux on the issue)"
      STATE=needs-ux; return 0
    fi

    if ! has needs-work "$ls"; then
      if has requires-rig "$ls"; then
        echo "  #$n PR #$pr: tree QA complete — REQUIRES RIG before approval"
        echo "     see the review's 'rig verification required' field."
        STATE=needs-rig
      elif has claude-approved "$ls"; then
        echo "  #$n PR #$pr: qa reviewed and raised nothing — yours to merge"
        STATE=awaiting-merge
      else
        echo "  #$n PR #$pr: qa posted no approval or routed finding — stopping"
        STATE=needs-human
      fi
      return 0
    fi
  done

  # Reachable only on re-entry after a handback, when the earlier passes
  # already spent the budget. Silence here would read as a clean finish.
  echo "  #$n PR #$pr: dev→qa rounds already spent ($ROUNDS) — stopping"
  return 0
}

drive() {
  local n="$1" step=0 ls pr tc st force=""
  local ran_design=0 ran_ux=0 ran_triage=0
  local design_passes=0 ux_passes=0
  qa_round=0

  while (( step < STEPS )); do
    (( ++step ))
    ls="$(labels "$n")" || { echo "  #$n: cannot read issue"; return 1; }

    # A role that ran and left its OWN label is a spec gap, and retrying is how
    # a loop turns one into an infinite one — the guards below stop on it. But
    # a label that was cleared and later re-applied is a different fact: qa or
    # developer sending the issue back, which is exactly what deserves another
    # pass. Observing the label absent is what separates the two cases, so
    # observe it every lap, before anything branches on it.
    has needs-design "$ls" || ran_design=0
    has needs-ux     "$ls" || ran_ux=0

    has blocked "$ls"          && { echo "  #$n: blocked — lift condition is in the comment that applied it"; STATE=blocked; return 0; }
    has needs-discussion "$ls" && { echo "  #$n: needs-discussion — yours to decide"; STATE=needs-human; return 0; }
    has needs-clarification "$ls" && { echo "  #$n: needs-clarification — triage is waiting on the reporter"; STATE=needs-human; return 0; }
    has epic "$ls"             && { echo "  #$n: epic — children drive separately"; STATE=epic; return 0; }

    # Nothing has routed this issue and triage has never spoken on it. This is
    # the case that used to fall out of the bottom as "nothing to do".
    if ! routed "$ls"; then
      tc="$(triage_evidence "$n")" || { echo "  #$n: cannot read issue comments — stopping rather than guessing"; return 1; }
      if (( tc == 0 )); then
        [[ -n ${AC_NO_TRIAGE:-} ]] && { echo "  #$n: unrouted, AC_NO_TRIAGE set — nothing to do"; return 0; }
        (( ran_triage )) && { echo "  #$n: triage ran and applied no routing label — read its comment"; return 0; }
        ran_triage=1; echo "  #$n: triage"
        "$BIN/triage.sh" "$n" $fg || { echo "  #$n: triage failed"; return 1; }
        continue
      fi
      # Spec comment but no routing label: triage stopped mid-way, or a label
      # was removed by hand. Either way the next step is a decision, not a run.
      echo "  #$n: triage spec present but no routing label — yours to set"
      STATE=needs-human; return 0
    fi

    # ux step 6 runs first: it clears needs-ux but defers ready-to-implement to
    # architect when both labels are set, so design must be the later gate.
    if has needs-ux "$ls"; then
      if (( ran_ux )); then
        echo "  #$n: ux ran, needs-ux still set — read its comment"; return 0
      fi
      if (( ux_passes >= UX_PASSES )); then
        echo "  #$n: needs-ux applied $ux_passes times this run — stopping"
        echo "     the issue is bouncing between ux and implementation. read the comments."
        STATE=needs-human; return 0
      fi
      ran_ux=1; (( ++ux_passes )); echo "  #$n: ux (pass $ux_passes)"
      "$BIN/ux.sh" "$n" $fg || { echo "  #$n: ux failed"; return 1; }
      continue
    fi

    if has needs-design "$ls"; then
      if (( ran_design )); then
        echo "  #$n: architect ran, needs-design still set — read its comment"; return 0
      fi
      if (( design_passes >= DESIGN_PASSES )); then
        echo "  #$n: needs-design applied $design_passes times this run — stopping"
        echo "     the issue is bouncing between design and implementation. read the comments."
        STATE=needs-human; return 0
      fi
      ran_design=1; (( ++design_passes )); echo "  #$n: architect (pass $design_passes)"
      "$BIN/design.sh" "$n" $fg || { echo "  #$n: design failed"; return 1; }
      continue
    fi

    pr="$(pr_for "$n")"
    if [[ -n $pr ]]; then
      echo "  #$n: PR #$pr"
      st=0; qa_loop "$n" "$pr" "$force" || st=$?
      force=""
      (( st == 0 )) || return "$st"
      case "$STATE" in
        needs-design|needs-ux)
          # Round the loop: the label is on the issue, and the ux and design
          # gates above route on it. Come back to this PR with a full pass —
          # the tip may be unchanged, but what it is measured against is not.
          force=full; continue ;;
      esac
      return 0
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
    st=0; qa_loop "$n" "$pr" || st=$?
    (( st == 0 )) || return "$st"
    case "$STATE" in
      needs-design|needs-ux) force=full; continue ;;
    esac
    return 0
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
      needs-rig)
        echo "  #$c needs a rig measurement — stopping epic."
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
    # is_epic() ran before triage did. An issue triage has just broken into
    # sub-issues is an epic now, and drive() returns STATE=epic saying so.
    [[ $STATE == epic ]] && drive_epic "$id" || true
  fi
done

echo
echo "nothing above was merged or closed. board.sh for current state."
