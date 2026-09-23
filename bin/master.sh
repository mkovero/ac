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
# It stops on needs-discussion, on needs-clarification, on blocked, on
# site-visit (deferred site work), on qa
# approval, and when a role leaves its own label in place.
#
#   AC_ROUNDS=3          max dev→qa cycles before handing back (default 3)
#   AC_STEPS=8           max state transitions per issue, loop backstop (default 8)
#   AC_DESIGN_PASSES=2   max architect passes per issue per run (default 2)
#   AC_UX_PASSES=2       max ux passes per issue per run (default 2)
#   AC_NO_TRIAGE=1       never triage; an unrouted issue is nothing to do
#   AC_PROVIDER=codex    use Codex for ordinary roles except the model-bound
#                        QA gates (default: claude)
#   AC_DEVELOPER_PROVIDER=codex
#                        override one role; likewise TRIAGE, ARCHITECT, and UX.
#                        Per-role settings win over AC_PROVIDER. QA remains
#                        model-bound until its two approval labels are migrated.
#   AC_CODEX_RECHECK=0   after a Codex fail and a revision, run full Claude QA
#                        and then Codex again (old flow). Default 1: send the
#                        revision straight back to Codex for a recheck of the
#                        delta; Claude QA is not re-run, and its approval of the
#                        failed tip is carried forward only if Codex passes.
#   AC_RIG_AUTO=0        stop at rig-pending for a human instead of running
#                        bin/rig.sh. Default 1: when requires-rig is on the PR
#                        or its issue, run the rig session at the reviewed
#                        commit, then a full same-commit QA pass with the record.
#   AC_WAIT_MERGE=1      wait at an epic child until its PR's merge commit is
#                        on main, then continue with the next child; stop the
#                        epic if the child or its PR closes without that
#                        (default: stop and return)
#   AC_MERGE_POLL_SECONDS=60  polling interval for AC_WAIT_MERGE
#
# Verify label names first — a wrong one makes this do nothing while looking
# like it worked:  gh label list -R mkovero/ac

_caller_limit_file="${AC_LIMIT_FILE:-}"
source "$(dirname "$0")/common.sh"
# One limit file per run: a concurrent master.sh must not clear or trip ours.
export AC_LIMIT_FILE="${_caller_limit_file:-$AC_LOG_DIR/provider-limit.$$}"
# Our own per-run file goes when the run does; a caller's path is theirs.
[[ -n $_caller_limit_file ]] || trap 'rm -f "$AC_LIMIT_FILE"' EXIT
BIN="$(cd "$(dirname "$0")" && pwd)"
ROUNDS="${AC_ROUNDS:-3}"
STATE=""          # outcome of the last drive(), read by the epic runner
STATE_PR=""       # the PR qa_loop approved, set with the awaiting-merge state
STEPS="${AC_STEPS:-8}"
RECHECK="${AC_CODEX_RECHECK:-1}"
RIG_AUTO="${AC_RIG_AUTO:-1}"
DESIGN_PASSES="${AC_DESIGN_PASSES:-2}"
UX_PASSES="${AC_UX_PASSES:-2}"

# dev→qa rounds are reset at the start of each issue and after an authoritative
# design/UX handback. DESIGN_PASSES, UX_PASSES, and STEPS independently bound
# cross-role loops.
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
# A failed read fails the function: a failed branch lookup followed by an
# empty body lookup would otherwise read as "no PR", and design_pass_voids
# would skip clearing approvals on a PR it never saw (#560).
pr_for() {
  local n="$1" pr
  pr=$(gh_retry gh pr list -R "$AC_REPO" --state open --json number,headRefName --jq \
    "[.[] | select((.headRefName | startswith(\"issue-$n-\")) or (.headRefName == \"issue-$n\"))] | .[0].number // empty") \
    || return 1
  [[ -n $pr ]] && { printf '%s\n' "$pr"; return; }
  gh_retry gh pr list -R "$AC_REPO" --state open --json number,body --jq \
    "[.[] | select(.body // \"\" | test(\"[Cc]loses +#$n\\\\b\"))] | .[0].number // empty"
}

# Whether PR <pr>'s change is on main, by ancestry of its merge commit — not by
# issue state, and not by mergedAt alone: a stacked PR reads merged once it
# lands on its base branch, while main still lacks its commits. GitHub's
# compare API answers, not the local origin/main, which in the shared checkout
# can be stale and would read a fresh merge as missing. Prints one of:
#   landed <oid>            merge commit is main or an ancestor of it
#   elsewhere <base> <oid>  the PR has a merge commit, and main does not
#   unmerged                the PR has no merge
#   unknown                 a call failed or answered nothing usable
# A failure is never `landed`.
pr_landed() {
  local pr="$1" info merged_at oid base status
  info="$(gh_retry gh pr view "$pr" -R "$AC_REPO" --json mergedAt,mergeCommit,baseRefName \
    --jq '[.mergedAt // "-", .mergeCommit.oid // "-", .baseRefName // "-"] | join("|")')" \
    || { echo unknown; return 0; }
  IFS='|' read -r merged_at oid base <<< "$info"
  [[ -n $merged_at ]] || { echo unknown; return 0; }
  [[ $merged_at == - ]] && { echo unmerged; return 0; }
  [[ $oid =~ ^[0-9a-f]{40}$ ]] || { echo unknown; return 0; }
  # compare/main...<oid>: base main, head oid. `behind` = oid is behind main,
  # i.e. an ancestor of it. The order matters: reversed, `ahead` would be it.
  status="$(gh_retry gh api "repos/$AC_REPO/compare/main...$oid" --jq .status)" \
    || { echo unknown; return 0; }
  case "$status" in
    behind|identical) echo "landed $oid" ;;
    ahead|diverged)   echo "elsewhere $base $oid" ;;
    *)                echo unknown ;;
  esac
}

# A branch with no open PR is a failed earlier run, not a fresh start.
stale_branch() {
  git show-ref -q "refs/heads/issue-$1"
}

# Absence of needs-work is NOT approval: a session that crashed, hit its turn
# limit, or ended without posting leaves the labels exactly as an approving one
# does. Require positive evidence that QA spoke. qa_evidence() is in common.sh.
qa_comments() { qa_evidence "$1"; }

codex_gate() {
  local pr="$1" pls
  echo "  PR #$pr: independent Codex QA"
  "$BIN/review.sh" --independent "$pr" || return 1
  pls="$(pr_labels "$pr")" || return 1
  if has needs-work "$pls"; then
    echo "  PR #$pr: Codex QA requested changes"
    return 2
  fi
  if ! has codex-approved "$pls"; then
    echo "  PR #$pr: Codex QA posted no approval — stopping"
    return 1
  fi
  echo "  PR #$pr: independent Codex QA approved"
}

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
# Only a comment carrying triage's `### spec` heading counts. Triage also posts
# agent-tagged one-liners (scope-label backfills on other issues); on
# 2026-09-17 those made #472 and #494 look triaged, and drive() stopped them at
# "spec present but no routing label".
triage_evidence() {
  local c
  c=$(gh_retry gh issue view "$1" -R "$AC_REPO" --json comments \
      --jq '[.comments[] | select((.body | test("agent: *triage"; "i")) and (.body | test("(^|\n)### spec")))] | length') || return 1
  echo "${c:-0}"
}

# The newest architect **file manifest** field says `none` (ARCH_MANIFEST_JQ).
architect_declared_no_change() {
  local body
  body=$(gh_retry gh issue view "$1" -R "$AC_REPO" --json comments \
    --jq "$ARCH_MANIFEST_JQ") || return 1
  printf '%s\n' "$body" | awk '
    /^\*\*file manifest\*\*/ { m=1; next }
    m && NF { if (tolower($0) ~ /^[[:space:]`(]*none/) found=1; exit }
    END { exit found ? 0 : 1 }'
}

# Stop the whole run on a provider account limit: every later step would fail
# the same way (2026-09-17, 00:37Z: 13 issues "aborted" in two minutes).
limit_stop() {
  [[ -s $AC_LIMIT_FILE ]] || return 0
  echo
  echo "  provider limit — stopping this run: $(cat "$AC_LIMIT_FILE")"
  echo "  nothing after this issue was attempted. rerun once the limit resets."
  exit 75
}

# qa_loop <issue> <pr> [force]
# force=full → ignore the reviewed-SHA cache and review the whole PR again.
# drive() sets it when architect or ux has just changed the design under a diff
# that may already carry an approval of the design it replaced.
qa_loop() {
  local n="$1" pr="$2" force="${3:-}" ls ils before after head mark ev pre post dc rev
  # codex_base: the tip Claude QA approved and Codex then failed. While set,
  # a revision goes back to Codex alone (AC_CODEX_RECHECK). Kept on disk so a
  # rerun after a stop still knows which delta Claude has not seen.
  local cbfile="$AC_LOG_DIR/codex-base-pr-$pr.sha" codex_base=""
  [[ $RECHECK == 1 && -f $cbfile ]] && codex_base="$(cat "$cbfile")"
  # rig_step: requires-rig is on the PR or its issue and tree QA is done at
  # $head. Run the rig session once per commit. 0 → a record is posted, do a
  # full QA pass with it. 1 → stop here, STATE set.
  local rmark="$AC_LOG_DIR/rig-pr-$pr.sha"
  rig_step() {
    local rrc=0
    if [[ $RIG_AUTO != 1 ]]; then
      echo "  #$n PR #$pr: REQUIRES RIG (AC_RIG_AUTO=0) — run: bin/rig.sh $pr"
      STATE=needs-rig; return 1
    fi
    if [[ -f $rmark && "$(cat "$rmark")" == "$head" ]]; then
      echo "  #$n PR #$pr: rig already ran at ${head:0:8} and requires-rig is still set — yours"
      echo "     read the agent:rig record and the QA pass after it: a fail or"
      echo "     decline, or a check the record did not close."
      STATE=needs-rig; return 1
    fi
    echo "  #$n PR #$pr: rig session at ${head:0:8}"
    "$BIN/rig.sh" "$pr" $fg || rrc=$?
    case $rrc in
      0) mkdir -p "$AC_LOG_DIR"; printf '%s\n' "$head" > "$rmark"
         echo "  #$n PR #$pr: rig record posted — full QA at the same commit"
         return 0 ;;
      3) echo "  #$n PR #$pr: rig busy (lock held) — rerun later: bin/rig.sh --status" ;;
      4) echo "  #$n PR #$pr: rig not configured here (scripts or access missing)" ;;
      *) echo "  #$n PR #$pr: rig session posted no usable record — read its log" ;;
    esac
    STATE=needs-rig; return 1
  }
  codex_failed_at() {
    [[ $RECHECK == 1 ]] || return 0
    codex_base="$1"; mkdir -p "$AC_LOG_DIR"; printf '%s\n' "$1" > "$cbfile"
  }
  # decision_check (#560, R2): an approval label is a claim about a tip AND the
  # design it was judged against. For each approval label in $ls, the newest
  # record of the role that set it must name the current decision_rev. A label
  # whose record does not is removed, and $ls re-read. The architect may edit
  # while a review runs, and a revision made outside this runner leaves no
  # label behind, so this runs before any gate is skipped or reported passed.
  #   0 every present approval covers the current decision
  #   2 at least one label was removed — take the review path again
  #   1 could not read or could not clear — the caller stops
  #   3 a label was removed a second time under the same decision: the review
  #     that ran in between saw that decision and still recorded another (or
  #     none). Another review cannot converge, so stop, STATE=needs-human. A
  #     design that moved in between is a new decision and re-reviews again.
  decision_check() {
    local rev l role rec was rc=0 stuck=""
    rev="$(decision_of_pr "$pr")" \
      || { echo "  #$n PR #$pr: cannot read the design decision — stopping rather than guessing"; return 1; }
    for l in claude-approved codex-approved; do
      has "$l" "$ls" || continue
      [[ $l == claude-approved ]] && role=qa || role=codex-qa
      rec="$(newest_record "$pr" "$role")" \
        || { echo "  #$n PR #$pr: cannot read the $role record — stopping"; return 1; }
      record_names_decision "$rec" "$rev" && continue
      was="$(decision_of_record "$rec")"
      echo "  #$n PR #$pr: $l was given under decision $was; current is $rev — removing it"
      invalidate_approvals "$pr" "the newest \`$role\` record names decision \`$was\`; the design comments now digest to \`$rev\` (decision_rev). The approval covers a design that no longer stands, so that gate runs again." "$l" \
        || { echo "  #$n PR #$pr: cannot clear $l — do it by hand"; return 1; }
      rc=2
      if [[ " $dc_seen " == *" $l@$rev "* ]]; then
        stuck+="${stuck:+ }$l ($role record names $was)"
      fi
      dc_seen+=" $l@$rev"
    done
    if [[ -n $stuck ]]; then
      echo "  #$n PR #$pr: removed again under the same decision $rev: $stuck — stopping"
      echo "     a review ran under $rev and its record names another decision, so"
      echo "     reviewing again cannot converge. the record's decision: line must be"
      echo "     exactly $rev (qa.md step 4, codex-qa.md step 5). read that record."
      STATE=needs-human; return 3
    fi
    if (( rc == 2 )); then
      ls="$(pr_labels "$pr")" || { echo "  #$n PR #$pr: cannot read labels — stopping rather than guessing"; return 1; }
    fi
    return "$rc"
  }
  # Not every exit path sets STATE, and drive() re-enters this function after a
  # handback. A STATE left over from the previous entry would read as a second
  # handback and loop until the step limit — which looks like cycling labels
  # and is not.
  STATE=""
  # label@decision pairs decision_check has already removed in this entry.
  local dc_seen=""
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
      if has requires-rig "$ls" || has requires-rig "$ils"; then
        echo "  #$n PR #$pr: carries requires-rig — the rig session runs after"
        echo "     the next tree QA pass, at the revised commit."
      fi
      echo "  #$n PR #$pr: revising (round $qa_round)"
      pre="$(gh_retry gh pr view "$pr" -R "$AC_REPO" --json headRefOid --jq .headRefOid)" \
        || { echo "  #$n: cannot read the tip — not starting a revise"; return 1; }
      # A run that stopped between the Codex fail and the revise, or one from
      # before this flow existed, has no base on disk. claude-approved paired
      # with needs-work is the Codex-fail state (qa.md step 5); confirm it from
      # both records rather than from the labels alone.
      if [[ $RECHECK == 1 && -z $codex_base ]] && has claude-approved "$ls" \
          && [[ "$(newest_record "$pr" codex-qa)" == *"$pre"* \
             && "$(newest_record "$pr" qa)" == *"$pre"* ]]; then
        codex_failed_at "$pre"
      fi
      cpre=""$(gh_retry gh pr view "$pr" -R "$AC_REPO" --json comments --jq '.comments | length')" \
        || { echo "  #$n: cannot count PR comments — not starting a revise"; return 1; }
      local revise_rc=0 retry_head
      AC_REVISE_MODE="${codex_base:+codex}" "$BIN/revise.sh" "$pr" $fg || revise_rc=$?
      # 75 is a provider limit (run()): retrying would only hit it again.
      if (( revise_rc != 0 && revise_rc != 75 && revise_rc != 130 && revise_rc != 143 )); then
        retry_head="$(gh_retry gh pr view "$pr" -R "$AC_REPO" --json headRefOid --jq .headRefOid)" || return 1
        if [[ $retry_head == "$pre" ]]; then
          echo "  #$n: revision worker exited before pushing — retrying once in the preserved worktree"
          revise_rc=0
          AC_REVISE_MODE="${codex_base:+codex}" "$BIN/revise.sh" "$pr" $fg || revise_rc=$?
        fi
      fi
      (( revise_rc == 0 )) || { echo "  #$n: revise failed"; return 1; }
      post="$(gh_retry gh pr view "$pr" -R "$AC_REPO" --json headRefOid --jq .headRefOid)" \
        || { echo "  #$n: cannot read the tip — check the PR by hand"; return 1; }

      # A revise that pushed nothing is a design/ux handback (label on the
      # issue, handled first below) or one of two different facts:
      #  - the developer posted a PR comment: it is saying the block is not
      #    code-fixable (a rig measurement, an assumed criterion, a design call);
      #  - no push AND no comment: the session did not finish — a crash, a turn
      #    limit, or a headless session that backgrounded its gate and ended its
      #    turn waiting (AGENTS.md → headless sessions). revise.sh still exits 0
      #    then, so the retry above never fires. That is not a decline, and the
      #    fix may be sitting uncommitted in the worktree.
      # Either way, clearing needs-work here would be this script overruling
      # the verdict — and the reviewed-SHA cache below would then see a tip qa
      # has already reviewed with a comment on it and report "raised nothing",
      # which is how a request-changes verdict turns into "yours to merge".
      # Leave the label. Stop.
      if [[ $pre == "$post" ]]; then
        ils="$(labels "$n")" || { echo "  #$n: cannot read issue labels — stopping"; return 1; }
        if has needs-design "$ils"; then
          echo "  #$n PR #$pr: developer handed back to architect (needs-design)"
          echo "     the PR stays unchanged; architect must amend the design/manifest"
          STATE=needs-design; return 0
        fi
        if has needs-ux "$ils"; then
          echo "  #$n PR #$pr: developer handed back to ux (needs-ux)"
          echo "     the PR stays unchanged; ux must resolve the output decision"
          STATE=needs-ux; return 0
        fi
        local cpost branch dirty
        cpost="$(gh_retry gh pr view "$pr" -R "$AC_REPO" --json comments --jq '.comments | length')" || cpost=""
        echo "  #$n PR #$pr: revise pushed nothing — tip is still $post"
        if [[ -n $cpost ]] && (( cpost > cpre )); then
          echo "     the developer commented: needs-work stays. re-reviewing an"
          echo "     identical tip cannot change the verdict, so the block is one"
          echo "     only you can clear: a rig measurement, acceptance of an assumed"
          echo "     criterion, a design call. read the developer's PR comment for which."
        else
          branch="$(gh_retry gh pr view "$pr" -R "$AC_REPO" --json headRefName --jq .headRefName 2>/dev/null)" || branch=""
          dirty=""
          [[ -n $branch && -d $WT_BASE/$branch ]] \
            && dirty="$(git -C "$WT_BASE/$branch" status --porcelain --untracked-files=no 2>/dev/null | wc -l)"
          if [[ -z $cpost ]]; then
            echo "     cannot count PR comments — either the developer declined (read"
            echo "     its PR comment) or the session did not finish (below)."
          else
            echo "     and posted no PR comment: the developer session did not finish."
            echo "     this is NOT a decline — likely a crash, a turn limit, or a"
            echo "     headless session that backgrounded its gate and ended its turn."
          fi
          if [[ -n $dirty ]] && (( dirty > 0 )); then
            echo "     $WT_BASE/$branch has $dirty uncommitted tracked file(s) — the"
            echo "     fix may be written but not committed. inspect before re-running."
          fi
          echo "     needs-work stays. session log:"
          echo "       ls -t ${AC_LOG_DIR}/*developer-pr-$pr-rev*.jsonl | head -1"
        fi
        STATE=needs-human; return 0
      fi

      gh_retry gh pr edit "$pr" -R "$AC_REPO" \
        --remove-label needs-work --add-label in-review >/dev/null 2>&1 || true
      ls="$(pr_labels "$pr")"
    fi

    # Codex recheck: the revision answers a Codex finding on a tip Claude QA
    # approved. Codex reviews base..head and runs the workspace gate itself;
    # Claude QA is not re-run. Only a Codex pass that names both SHAs lets the
    # runner carry claude-approved forward — the label then means "Claude
    # approved base, and the only commits since answer Codex and passed Codex".
    if [[ -n $codex_base ]] && ! has needs-work "$ls" && ! has claude-approved "$ls"; then
      head="$(gh_retry gh pr view "$pr" -R "$AC_REPO" --json headRefOid --jq .headRefOid)" \
        || { echo "  #$n: cannot read the tip — stopping"; return 1; }
      # Every later check in this block compares against $head; an empty one
      # makes "the record names the head" always true.
      [[ $head =~ ^[0-9a-f]{40}$ ]] || { echo "  #$n: tip read as '$head' — stopping"; return 1; }
      if has requires-rig "$ls" || has requires-rig "$ils"; then
        # A rig gate under a Codex recheck: Codex cannot approve past it, and
        # the rig record needs a Claude pass to be accepted. Full QA instead.
        echo "  #$n PR #$pr: requires-rig under a Codex recheck — full Claude QA path"
        codex_base=""; rm -f "$cbfile"; force=full
      fi
      # The carry-forward restores an approval Claude gave at the base. If the
      # design changed since, that approval covers a superseded design (#560).
      rev=""
      if [[ -n $codex_base ]]; then
        rev="$(decision_of_pr "$pr")" \
          || { echo "  #$n PR #$pr: cannot read the design decision — stopping rather than guessing"; return 1; }
        if ! record_names_decision "$(newest_record "$pr" qa)" "$rev"; then
          echo "  #$n PR #$pr: Claude QA at ${codex_base:0:8} predates decision $rev — full Claude QA path"
          codex_base=""; rm -f "$cbfile"; force=full
        fi
      fi
      if [[ -n $codex_base && $head != "$codex_base" ]]; then
        echo "  #$n PR #$pr: Codex recheck of ${codex_base:0:8}..${head:0:8} (Claude QA not re-run)"
        local rrc=0 crec
        "$BIN/review.sh" --independent --recheck "$codex_base" "$pr" || rrc=$?
        if (( rrc == 3 )); then
          echo "  #$n PR #$pr: tip no longer descends from the approved base — full Claude QA"
          codex_base=""; rm -f "$cbfile"; force=full
        elif (( rrc != 0 )); then
          echo "  #$n PR #$pr: Codex recheck failed to run"; return 1
        else
          ls="$(pr_labels "$pr")" || return 1
          if has needs-work "$ls"; then
            echo "  #$n PR #$pr: Codex recheck requested changes"
            continue
          fi
          crec="$(newest_record "$pr" codex-qa)"
          if ! has codex-approved "$ls" || [[ $crec != *"$head"* || $crec != *"$codex_base"* ]]; then
            echo "  #$n PR #$pr: Codex recheck posted no approval naming both tips — stopping"
            STATE=needs-human; return 0
          fi
          [[ "$(gh_retry gh pr view "$pr" -R "$AC_REPO" --json headRefOid --jq .headRefOid)" == "$head" ]] \
            || { echo "  #$n PR #$pr: tip moved during the recheck — stopping"; return 1; }
          gh_retry gh pr edit "$pr" -R "$AC_REPO" --add-label claude-approved >/dev/null \
            || { echo "  #$n PR #$pr: could not restore claude-approved — do it by hand"; return 1; }
          gh_retry gh pr comment "$pr" -R "$AC_REPO" --body "<!-- agent: runner -->
claude-approved carried forward to \`$head\`: Claude QA approved \`$codex_base\`; the commits since answer a Codex finding and passed a Codex recheck (AC_CODEX_RECHECK). Claude QA has not reviewed \`$codex_base..$head\`.
decision: $rev" >/dev/null || true
          rm -f "$cbfile"
          echo "  #$n PR #$pr: Codex recheck approved — claude-approved carried forward from ${codex_base:0:8}"
          ls="$(pr_labels "$pr")" || { echo "  #$n PR #$pr: cannot read labels — stopping rather than guessing"; return 1; }
          dc=0; decision_check || dc=$?
          (( dc == 1 )) && return 1
          (( dc == 3 )) && return 0
          (( dc == 2 )) && { codex_base=""; continue; }
          echo "  #$n PR #$pr: both QA gates passed — yours to merge"
          STATE=awaiting-merge; STATE_PR="$pr"; return 0
        fi
      fi
    fi

    head="$(gh_retry gh pr view "$pr" -R "$AC_REPO" --json headRefOid --jq .headRefOid)"
    mark="$AC_LOG_DIR/reviewed-pr-$pr.sha"

    # Already reviewed at this exact tip. requires-rig is a pre-approval stop;
    # after a human clears it, absence of claude-approved forces a full same-tip
    # QA pass so the measurement record becomes part of the approval evidence.
    # Unless force is set — then the tip is unchanged but the design under it
    # is not, and the cached approval is an approval of a superseded spec.
    ev="$(qa_evidence "$pr")" || { echo "  #$n: cannot count qa output — stopping"; return 1; }
    # Same for a design revised under an unchanged tip: an approval whose
    # record names another decision is dropped before the cache can reuse it.
    # A removed claude-approved sends the branch below to a full pass; a
    # removed codex-approved to codex_gate.
    dc=0; decision_check || dc=$?
    (( dc == 1 )) && return 1
    (( dc == 3 )) && return 0
    if [[ -z $force && -f $mark && "$(cat "$mark")" == "$head" ]] && (( ev > 0 )); then
      if has requires-rig "$ls" || has requires-rig "$ils"; then
        echo "  #$n PR #$pr: tree QA complete — requires-rig"
        rig_step || return 0
        force=full; continue
      fi
      if has claude-approved "$ls"; then
        if ! has codex-approved "$ls"; then
          codex_gate "$pr" || { rc=$?; (( rc == 2 )) && { codex_failed_at "$head"; continue; }; return "$rc"; }
          ls="$(pr_labels "$pr")"
        fi
        if has codex-approved "$ls" && ! has needs-work "$ls"; then
          dc=0; decision_check || dc=$?
          (( dc == 1 )) && return 1
          (( dc == 3 )) && return 0
          (( dc == 2 )) && continue
          echo "  #$n PR #$pr: both QA gates passed — yours to merge"
          STATE=awaiting-merge; STATE_PR="$pr"; return 0
        fi
      fi
      echo "  #$n PR #$pr: no approval at a reviewed commit — full QA pass"
      force=full
    fi

    echo "  #$n PR #$pr: qa review${force:+ (full pass)}"
    before="$(qa_evidence "$pr")" || { echo "  #$n: cannot count qa output — stopping"; return 1; }
    codex_base=""; rm -f "$cbfile"   # Claude sees the whole delta again
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
      if has requires-rig "$ls" || has requires-rig "$ils"; then
        echo "  #$n PR #$pr: tree QA complete — requires-rig"
        rig_step || return 0
        force=full; continue
      elif has claude-approved "$ls"; then
        # A codex-approved from before this pass is not skipped past on trust,
        # and the design may have moved while Claude QA ran.
        dc=0; decision_check || dc=$?
        (( dc == 1 )) && return 1
        (( dc == 3 )) && return 0
        (( dc == 2 )) && continue
        if ! has codex-approved "$ls"; then
          codex_gate "$pr" || { rc=$?; (( rc == 2 )) && { codex_failed_at "$head"; continue; }; return "$rc"; }
          ls="$(pr_labels "$pr")"
        fi
        if has codex-approved "$ls" && ! has needs-work "$ls"; then
          dc=0; decision_check || dc=$?
          (( dc == 1 )) && return 1
          (( dc == 3 )) && return 0
          (( dc == 2 )) && continue
          echo "  #$n PR #$pr: both QA gates passed — yours to merge"
          STATE=awaiting-merge; STATE_PR="$pr"
        else
          echo "  #$n PR #$pr: independent QA did not approve — stopping"
          STATE=needs-human
        fi
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

# design_pass_voids <issue> <role> (#560, R1) — called by drive() after an
# architect or ux pass, whether it was a handback in the review loop or already
# pending when the run started. Any approval on the open PR was judged against
# the decision that pass may have revised, and the tip has not moved, so
# nothing else would clear it: remove both labels and force the next qa_loop
# into a full pass. Runs even when the pass edited nothing — the check is
# cheap, a stale approval is not. Sets drive()'s force. No PR, nothing to do.
design_pass_voids() {
  local n="$1" role="$2" p
  p="$(pr_for "$n")" || { echo "  #$n: cannot look up the PR after the $role pass — stopping"; return 1; }
  [[ -n $p ]] || return 0
  invalidate_approvals "$p" "a $role pass ran on #$n, and approvals given before it cover a decision that may no longer stand. Both gates run again." \
    || { echo "  #$n PR #$p: cannot clear approvals — do it by hand"; return 1; }
  echo "  #$n PR #$p: approvals cleared after the $role pass — full review next"
  force=full
}

drive() {
  local n="$1" step=0 ls pr tc st force="" continue_arg="" mf=""
  local ran_design=0 ran_ux=0 ran_triage=0 preflight_ran=0
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
    # Deferred site work: a check for someone at the rig, nothing to implement.
    has site-visit "$ls"       && { echo "  #$n: site-visit — needs someone at the rig (see Deferred site work)"; STATE=needs-rig; return 0; }
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
      # If a real implementation branch already contains work, however, an
      # architect handback may have removed needs-design without restoring the
      # ready label. Preserve the work and treat the existing branch as an
      # implicit continuation; do not invent a fresh implementation.
      if stale_branch "$n"; then
        local unlabeled_wt="$WT_BASE/issue-$n" unlabeled_dirty=0 unlabeled_ahead=0
        [[ -d $unlabeled_wt ]] && unlabeled_dirty="$(git -C "$unlabeled_wt" status --porcelain | wc -l)"
        unlabeled_ahead="$(git rev-list --count "origin/main..issue-$n" 2>/dev/null || echo 0)"
        if (( unlabeled_dirty > 0 || unlabeled_ahead > 0 )); then
          echo "  #$n: routing label missing but existing implementation work is present — continuing"
          ls+=$'\nready-to-implement'
          continue_arg=--continue
        else
          echo "  #$n: triage spec present but no routing label — yours to set"
          STATE=needs-human; return 0
        fi
      else
        echo "  #$n: triage spec present but no routing label — yours to set"
        STATE=needs-human; return 0
      fi
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
      design_pass_voids "$n" ux || return 1
      qa_round=0
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
      design_pass_voids "$n" architect || return 1
      qa_round=0
      continue
    fi

    pr="$(pr_for "$n")" || { echo "  #$n: cannot look up the PR — stopping rather than guessing"; return 1; }
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
        echo "       continuing in the same worktree with the currently selected provider"
        continue_arg=--continue
      else
        echo "     it is empty — reusing it for this implementation attempt."
      fi
    fi

    has ready-to-implement "$ls" \
      || { echo "  #$n: no PR, not ready-to-implement — nothing to do"; return 0; }

    # Every developer invocation needs a hard file boundary. Issues triaged
    # straight to ready-to-implement have no architect comment, so create that
    # boundary before opening a worktree instead of asking dev to implement
    # from triage's non-exhaustive "files likely affected" list.
    mf="$(manifest_of "$n")" \
      || { echo "  #$n: cannot read architect manifest — stopping"; return 1; }
    if [[ -z $mf ]]; then
      # One preflight. An architect can finish with an empty manifest on
      # purpose (#400: "none", verification only); re-running design on that
      # looped until the step limit.
      if (( preflight_ran )); then
        echo "  #$n: still no file manifest after a design pass — yours"
        if architect_declared_no_change "$n"; then
          echo "     the architect's manifest is 'none': no code change is planned."
          echo "     close the issue, or re-route it if code is still wanted."
        fi
        STATE=needs-human; return 0
      fi
      preflight_ran=1
      echo "  #$n: no architect manifest — running design preflight"
      "$BIN/design.sh" "$n" $fg || { echo "  #$n: design preflight failed"; return 1; }
      continue
    fi

    echo "  #$n: implementing${continue_arg:+ (continuation)}"
    "$BIN/implement.sh" "$n" $continue_arg $fg || { echo "  #$n: implement failed"; return 1; }
    pr="$(pr_for "$n")" || { echo "  #$n: cannot look up the PR implement opened — check GitHub"; return 1; }
    if [[ -z $pr ]]; then
      # A developer may discover an out-of-manifest dependency and correctly
      # hand the issue back to design without committing or opening a PR. Read
      # the issue labels before calling that a failed implementation; the next
      # loop must drive the design gate against the preserved worktree.
      ls="$(labels "$n")" || { echo "  #$n: cannot read post-implementation labels"; return 1; }
      if has needs-design "$ls"; then
        echo "  #$n: implementation handed back to architect — routing design"
        continue
      fi
      if has needs-ux "$ls"; then
        echo "  #$n: implementation handed back to UX — routing design"
        continue
      fi
      if has blocked "$ls" || has needs-discussion "$ls"; then
        echo "  #$n: implementation stopped on a human/design gate"
        STATE=needs-human
        return 0
      fi
      echo "  #$n: no PR opened — check the session log"
      return 1
    fi
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

# Returns 0 only once pr_landed verifies the PR's merge commit is on main.
# 5 = the child will not land through this PR (stop the epic); 4 = integration
# pushed; 1-3 = could not wait. The issue's state is never evidence of a merge:
# it closes for duplicates, supersession and by hand too. It is read only to
# report why the child closed. The word "merged" is printed on the verified
# path only.
wait_for_merge() {
  local child="$1" pr="$2" pr_state state merged_at mergeable merge_status
  local verdict issue state_reason base oid
  local poll="${AC_MERGE_POLL_SECONDS:-60}"
  [[ $poll =~ ^[1-9][0-9]*$ ]] || { echo "  invalid AC_MERGE_POLL_SECONDS: $poll" >&2; return 2; }
  [[ -n $pr ]] || { echo "  cannot identify the open PR for #$child" >&2; return 1; }
  while true; do
    pr_state="$(gh_retry gh pr view "$pr" -R "$AC_REPO" \
      --json state,mergedAt,mergeable,mergeStateStatus \
      --jq '[.state // "UNKNOWN", .mergedAt // "-", .mergeable // "UNKNOWN", .mergeStateStatus // "UNKNOWN"] | join("|")' \
      2>/dev/null || true)"
    IFS='|' read -r state merged_at mergeable merge_status <<< "$pr_state"
    if [[ -z $state ]]; then
      echo "  #$child: could not read PR #$pr — checking again in ${poll}s"
      sleep "$poll"; continue
    fi
    verdict=""
    if [[ -n $merged_at && $merged_at != - ]]; then
      verdict="$(pr_landed "$pr")"
    else
      if [[ $state == CLOSED ]]; then
        echo "  #$child: PR #$pr closed without landing on main"
        return 5
      fi
      issue="$(gh_retry gh issue view "$child" -R "$AC_REPO" --json state,stateReason \
        --jq '[.state // "UNKNOWN", .stateReason // "-"] | join("|")' 2>/dev/null || echo 'UNKNOWN|-')"
      IFS='|' read -r state state_reason <<< "$issue"
      if [[ $state == CLOSED ]]; then
        # The PR read above can predate the merge that closed the issue, so
        # ask the PR again before reading the closure as a non-landing.
        verdict="$(pr_landed "$pr")"
        if [[ $verdict == unmerged ]]; then
          [[ -n $state_reason && $state_reason != - ]] || state_reason="none recorded"
          echo "  #$child: issue closed (reason: $state_reason) while PR #$pr shows no merge — no landed change found on main"
          return 5
        fi
      fi
    fi
    case "$verdict" in
      "") ;;
      landed\ *)
        oid="${verdict#landed }"
        echo "  #$child: PR #$pr landed on main (${oid:0:7}); continuing epic"
        return 0 ;;
      elsewhere\ *)
        read -r _ base oid <<< "$verdict"
        echo "  #$child: PR #$pr's base is $base; merge commit ${oid:0:7} is not on main"
        return 5 ;;
      *)
        echo "  #$child: whether PR #$pr's change is on main could not be checked — checking again in ${poll}s"
        sleep "$poll"; continue ;;
    esac
    if [[ $mergeable == CONFLICTING || $merge_status == DIRTY ]]; then
      echo "  #$child PR #$pr has merge conflicts with main — integrating"
      "$BIN/integrate.sh" "$pr" $fg || return 3
      return 4
    fi
    echo "  #$child awaiting your merge — checking again in ${poll}s"
    sleep "$poll"
  done
}

drive_epic() {
  local e="$1" kids c st wait_rc wait_pr
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
    STATE=""; STATE_PR=""
    drive "$c" || { limit_stop; echo "  #$c: aborted — stopping epic"; return 1; }

    case "$STATE" in
      awaiting-merge)
        echo
        echo "  #$c is ready for your merge."
        echo "  Later children branch from main and would not see #$c's work."
        if [[ -n ${AC_WAIT_MERGE:-} ]]; then
          while true; do
            wait_rc=0
            # The PR qa_loop approved, not a fresh lookup: pr_for lists open
            # PRs only, so a merge before this line would lose the PR and
            # never reach the landing check.
            wait_pr="$STATE_PR"
            wait_for_merge "$c" "$wait_pr" || wait_rc=$?
            if (( wait_rc == 4 )); then
              echo "  #$c: integration pushed — rerunning both QA gates"
              STATE=""; STATE_PR=""
              drive "$c" || { echo "  #$c: post-integration QA aborted"; return 1; }
              [[ $STATE == awaiting-merge ]] || {
                echo "  #$c stopped after integration in state: ${STATE:-unknown}"
                return 0
              }
              continue
            fi
            if (( wait_rc == 5 )); then
              # Not KEEP_GOING-able: every later child would build on a main
              # without #$c's change.
              echo "  #$c: no change landed on main — stopping epic #$e."
              echo "  A rerun of master.sh $e skips closed children and counts a closed blocker"
              echo "  as done: check #$c's state and PR #$wait_pr before rerunning,"
              echo "  and reopen #$c or remove it from #$e's child list."
              return 0
            fi
            (( wait_rc == 0 )) || return 0
            break
          done
        else
          echo "  Merge it, then rerun: master.sh $e"
          [[ -n ${KEEP_GOING:-} ]] || return 0
        fi ;;
      needs-rig)
        echo "  #$c needs a rig measurement — stopping epic."
        [[ -n ${KEEP_GOING:-} ]] || return 0 ;;
      needs-human|blocked)
        echo "  #$c needs you — stopping epic."
        [[ -n ${KEEP_GOING:-} ]] || return 0 ;;
      *)
        echo "  #$c did not reach a terminal workflow state — stopping epic."
        echo "  Inspect its labels and session log before continuing."
        [[ -n ${KEEP_GOING:-} ]] || return 0 ;;
    esac
  done
  echo "  #$e: all children processed"
}

gh_up || exit 1
rm -f "$AC_LIMIT_FILE"

for id in "${ids[@]}"; do
  echo "== issue #$id"
  if is_epic "$id"; then
    drive_epic "$id" || true
    limit_stop
  else
    STATE=""
    drive "$id" || echo "  #$id: aborted"
    limit_stop
    # is_epic() ran before triage did. An issue triage has just broken into
    # sub-issues is an epic now, and drive() returns STATE=epic saying so.
    [[ $STATE == epic ]] && drive_epic "$id" || true
  fi
done

echo
echo "nothing above was merged or closed. board.sh for current state."
