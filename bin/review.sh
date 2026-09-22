#!/usr/bin/env bash
# review.sh <pr> [--full] [--since <sha>] [--fg]
# review.sh --independent [--daemon|<pr>...]
# review.sh --independent --recheck <base-sha> <pr>
#
# --recheck: Codex re-reviews a codex-finding revision without a Claude QA pass
# in between. <base-sha> is the tip Claude QA approved and Codex failed. The
# runner (master.sh) restores claude-approved only if this pass approves.
# Exit 3: head does not descend from <base-sha> — the delta is not a delta,
# the caller must fall back to full Claude QA.
#
# QA role. No Edit/Write against the tree — a reviewer that can fix what it
# finds will fix it, and the finding never reaches you as a finding.
#
# Second and later passes review the DELTA, not the whole PR again. This is
# what qa.md already asks for: re-run the full mechanical gate against the new
# tip, re-review the delta. The commands stay full; the reading narrows.
#
# The reviewed SHA is cached locally per PR. Missing cache → full review, which
# is the right way to fail: toward more scrutiny, not less.

source "$(dirname "$0")/common.sh"

# Independent review slot.  This used to live in codex-qa.sh; keeping it here
# makes the review interface one command while preserving the separate label
# and model gate.
independent_review() {
  local -a prs=()
  local recheck_base="" provider
  # Refuse a misconfigured reviewer before a worktree and a gate run are paid for.
  provider="$(provider_for codex-qa)" || return
  review_provider_guard codex-qa "$provider" || return
  if [[ ${1:-} == --recheck ]]; then
    recheck_base="${2:-}"; shift 2 || true
    [[ $recheck_base =~ ^[0-9a-f]{40}$ && $# -eq 1 ]] \
      || { echo "usage: review.sh --independent --recheck <full-base-sha> <pr>" >&2; return 2; }
  fi
  if (($#)); then
    local p
    for p in "$@"; do
      [[ $p =~ ^[0-9]+$ ]] || { echo "usage: review.sh --independent [<pr>...]" >&2; return 2; }
      prs+=("$p")
    done
  else
    mapfile -t prs < <(gh_retry gh pr list -R "$AC_REPO" --state open --label claude-approved \
      --limit 100 --json number,labels --jq '.[] | select(all(.labels[]?; .name != "codex-approved" and .name != "needs-work" and .name != "requires-rig")) | .number')
  fi
  ((${#prs[@]})) || { echo "<codex/qa> No PRs require independent QA."; return 0; }

  local pr head wt labels qa_record issue decision
  for pr in "${prs[@]}"; do
    labels="$(gh_retry gh pr view "$pr" -R "$AC_REPO" --json labels --jq '.labels[].name')"
    has_label() { printf '%s\n' "$labels" | grep -qx "$1"; }
    head="$(gh_retry gh pr view "$pr" -R "$AC_REPO" --json headRefOid --jq .headRefOid)"
    qa_record="$(newest_record "$pr" qa)"
    if [[ -n $recheck_base ]]; then
      # A recheck is asked for by name, so a label that rules it out is an
      # error, not a queue skip: the caller must not read silence as a verdict.
      local l
      for l in codex-approved needs-work requires-rig claude-approved; do
        has_label "$l" && { echo "<codex/qa> PR #$pr: recheck refused, $l is set." >&2; return 1; }
      done
      [[ $head != "$recheck_base" ]] \
        || { echo "<codex/qa> PR #$pr: head is still $recheck_base — nothing to recheck." >&2; return 1; }
      if [[ $qa_record != *"$recheck_base"* ]]; then
        echo "<codex/qa> PR #$pr: newest Claude QA record does not name base $recheck_base." >&2
        return 1
      fi
      if [[ "$(newest_record "$pr" codex-qa)" != *"$recheck_base"* ]]; then
        echo "<codex/qa> PR #$pr: newest Codex QA record does not name base $recheck_base." >&2
        return 1
      fi
    else
      has_label claude-approved || { echo "<codex/qa> Skipping PR #$pr: claude-approved absent."; continue; }
      has_label codex-approved && continue
      has_label needs-work && continue
      has_label requires-rig && continue
      if [[ $qa_record != *"$head"* ]]; then
        echo "<codex/qa> PR #$pr: newest Claude QA record does not name current tip $head." >&2
        echo "<codex/qa> A fresh Claude QA pass with explicit SHA evidence is required." >&2
        return 1
      fi
    fi
    # The design revision this pass is judged against (#560): decision_of_pr,
    # the digest master.sh compares against. issue_of_pr feeds names_inputs.
    issue="$(issue_of_pr "$pr")" \
      || { echo "<codex/qa> PR #$pr: cannot read the issue it closes" >&2; return 1; }
    decision="$(decision_of_pr "$pr")" \
      || { echo "<codex/qa> PR #$pr: cannot read the design decision" >&2; return 1; }
    # Claude QA's record must cover the design as it stands now, in both modes:
    # a recheck carries that approval forward, a first pass pairs with it.
    if ! record_names_decision "$qa_record" "$decision"; then
      echo "<codex/qa> PR #$pr: newest Claude QA record names decision $(decision_of_record "$qa_record"), not the current $decision." >&2
      echo "<codex/qa> The design changed after that review; a fresh Claude QA pass is required." >&2
      return 1
    fi
    # The names step's inputs (gate.sh → stale_names.sh), per PR.
    names_inputs "$issue" \
      || { echo "<codex/qa> PR #$pr: cannot read its issue's superseded names" >&2; return 1; }
    wt="$WT_BASE/codex-pr-$pr"
    [[ ! -e $wt ]] || { echo "review worktree already exists: $wt" >&2; return 1; }
    require_space "$wt"; mkdir -p "$WT_BASE"
    # A private ref, not FETCH_HEAD: FETCH_HEAD belongs to the whole shared
    # checkout, and any concurrent fetch rewrites it (2026-09-17: PR #522
    # "changed while preparing review" while its head had not moved).
    git_retry git fetch -q origin "+pull/$pr/head:refs/ac/review/pr-$pr"
    [[ $(git rev-parse "refs/ac/review/pr-$pr") == "$head" ]] || { echo "PR #$pr changed while preparing review" >&2; return 1; }
    if [[ -n $recheck_base ]] && ! git merge-base --is-ancestor "$recheck_base" "$head" 2>/dev/null; then
      echo "<codex/qa> PR #$pr: $head does not descend from $recheck_base — full Claude QA needed." >&2
      return 3
    fi
    git worktree add --detach "$wt" "$head" >/dev/null
    git update-ref -d "refs/ac/review/pr-$pr"   # the worktree now holds $head
    link_support "$wt"
    local rc=0
    local task
    if [[ -n $recheck_base ]]; then
      task="Recheck PR #$pr in $AC_REPO as the independent Codex QA worker, in
recheck mode (codex-qa.md → recheck mode).

You failed this PR at $recheck_base. The developer revised it; the tip is now
$head. Claude QA approved $recheck_base and has NOT reviewed $recheck_base..$head.
The runner verified that the newest <!-- agent: qa --> record names
$recheck_base, that your newest <!-- agent: codex-qa --> record names
$recheck_base, and that $head descends from it.

Read your own review at $recheck_base first: every finding in it is resolved
at $head, or it is not. Then review the delta $recheck_base..$head in full, as
your role spec's steps 1-4 describe, including anything the delta breaks
outside the lines it touches. The runner ran the workspace gate at $head
(record below); do not re-run it.

Your comment names both $recheck_base and $head in full. On pass add
codex-approved and remove needs-work; on blocking defects add needs-work and
remove codex-approved. Never touch claude-approved — the runner restores it
only after your pass — nor in-review, requires-rig, or agent labels. Re-check
the PR HEAD before applying the final decision; if it changed, do not approve."
    else
      task="Review PR #$pr in $AC_REPO as the independent Codex QA worker.

The runner combined GitHub PR comments and reviews and verified that the newest
<!-- agent: qa --> record explicitly covers current tip $head. Treat that SHA
identity as the fresh-Claude-approval pre-check. Do not compare commit and
comment timestamps; commit timestamps are author-controlled and are not
workflow chronology.

Inspect the linked issue, decisions, complete diff, checks, tests, and relevant
history. On pass add codex-approved and remove needs-work; on blocking defects
add needs-work and remove codex-approved. Never touch claude-approved,
in-review, requires-rig, or agent labels. Re-check the PR HEAD before applying
the final decision; if it changed, do not approve."
    fi
    task="$task

The architect/ux design comments digest to decision $decision (decision_rev),
and the runner verified that the newest <!-- agent: qa --> record names it. On
the line directly under your \`## codex qa — PR #N at <sha>\` header, write
exactly \`decision: $decision\` (codex-qa.md → step 5); copy it, do not
recompute it. The runner removes an approval whose record names any other
decision."
    local gate_rc=0 gate_out
    gate_out="$(cd "$wt" && "$AC_GATE" 2>&1)" || gate_rc=$?
    printf '%s\n' "$gate_out" >&2
    task="$task

Workspace gate for $head (bin/gate.sh, exit $gate_rc — already run by the
runner; codex-qa.md → gate):

$gate_out"
    if (( gate_rc < 2 )); then
      ( cd "$wt" && AC_TAG="pr-$pr" run codex-qa "$task" --read ) || rc=$?
    else
      echo "<codex/qa> PR #$pr: gate refused in $wt" >&2; rc=1
    fi
    # The worktree is per-pass; so is its target (remove_worktree takes both).
    # Seeding makes the next one cheap.
    remove_worktree "$wt"
    ((rc == 0)) || return "$rc"
    echo "<codex/qa> Done. Review posted for PR #$pr."
  done
}

if [[ ${1:-} == --independent ]]; then
  shift
  if [[ ${1:-} == --daemon ]]; then
    shift; (($# == 0)) || { echo "usage: review.sh --independent --daemon" >&2; exit 2; }
    poll="${CODEX_QA_POLL_SECONDS:-300}"
    [[ $poll =~ ^[1-9][0-9]*$ ]] || { echo "CODEX_QA_POLL_SECONDS must be positive" >&2; exit 2; }
    while true; do independent_review || echo "<codex/qa> pass failed; retrying in ${poll}s" >&2; sleep "$poll"; done
  fi
  independent_review "$@"
  exit $?
fi

n="${1:?usage: review.sh <pr> [--full] [--since <sha>] [--fg]}"; shift || true

mode=auto since=""
declare -a rest=()
while (( $# )); do
  case "$1" in
    --full)  mode=full ;;
    --since) since="$2"; mode=delta; shift ;;
    *)       rest+=("$1") ;;
  esac
  shift
done

mark="$AC_LOG_DIR/reviewed-pr-$n.sha"
mkdir -p "$AC_LOG_DIR"
head_sha="$(gh_retry gh pr view "$n" -R "$AC_REPO" --json headRefOid --jq .headRefOid)"
# The design revision this pass is judged against (#560), captured as the
# review starts: an edit made while it runs must not read as covered.
# decision_of_pr, not issue_of_pr: master.sh compares against the same digest.
# issue_of_pr feeds names_inputs below.
issue="$(issue_of_pr "$n")" || { echo "cannot read the issue PR #$n closes" >&2; exit 1; }
decision="$(decision_of_pr "$n")" || { echo "cannot read the design decision for PR #$n" >&2; exit 1; }
decision_ask="On the line directly under that head-SHA header line, write exactly
\`decision: $decision\` (qa.md → step 4). The runner computed it from the
architect/ux design comments as this pass started; copy it verbatim, do not
recompute it. The runner removes an approval whose record names any other
decision."

# qa_evidence() lives in common.sh — counts both comments and reviews.
qa_comments() { qa_evidence "$n"; }

if [[ $mode == auto ]]; then
  if [[ -f $mark && $(qa_comments) -gt 0 ]]; then
    since="$(cat "$mark")"
    mode=delta
  else
    [[ -f $mark ]] && echo "note: cached SHA but no qa comment on the PR — full review" >&2
    mode=full
  fi
fi

# An empty range is not a delta. Refuse rather than hand QA a task whose
# premise is false — it will either say so and waste the run, or invent one.
if [[ $mode == delta && $since == "$head_sha" ]]; then
  echo "no new commits since last review of #$n ($since)." >&2
  echo "use --full to review the same tip again." >&2
  exit 0
fi

if [[ $mode == delta && -n $since ]]; then
  prompt="Review PR #$n in $AC_REPO. This is a re-review, not a first pass.

You already reviewed this PR at commit $since. New commits since then: $since..$head_sha.
Read your own earlier review comment on the PR first — the one marked 'agent: qa'.

Scope of this pass:
- The full workspace gate against the new tip is already run — see the gate
  record below. Not the delta's crate — the workspace: two changes that each
  pass alone can break in combination. Do not re-run it.
- Read the delta $since..$head_sha, not the whole diff. Do not re-litigate
  parts you already accepted.
- For each point you raised in your earlier review: is it addressed? Say so
  explicitly, one line each. A point silently dropped is a point not fixed.
- The delta touches measurement values, output formatting, or display units →
  the standards check applies to it on the same terms as a first pass.
- The delta may break something outside itself. Where it plausibly does, say
  where you looked.

State at the top of your comment which commit range you reviewed, including
the full $head_sha SHA as the range endpoint.

$decision_ask

Standards PDFs are NOT in this checkout — they are licence-restricted and gitignored. They are at $AC_STDDOCS. Each PDF has a .txt sibling extracted with pdftotext -layout: Grep that to find the clause, then Read the PDF at that region only. Do not page through a PDF looking for a clause. Extraction is lossy for equations, figures and some tables — where the clause turns on one of those, open the PDF itself. A citation you did not verify against the primary text is not a verified citation; if a document you need is genuinely missing from that directory, say which one rather than carrying the gap forward silently."
else
  prompt="Review PR #$n in $AC_REPO.

This is an explicit full review. Even if this commit already has an earlier QA
comment, this invocation is a new review pass: governing specs, issue decisions,
or human evidence may have changed without a code push. Apply the current QA
spec, post a new superseding QA comment, and update labels to its verdict. Do
not decline to post merely because the PR tip is unchanged.

State the full current head SHA $head_sha at the top of the review comment.

$decision_ask

Standards PDFs are NOT in this checkout — they are licence-restricted and gitignored. They are at $AC_STDDOCS. Each PDF has a .txt sibling extracted with pdftotext -layout: Grep that to find the clause, then Read the PDF at that region only. Do not page through a PDF looking for a clause. Extraction is lossy for equations, figures and some tables — where the clause turns on one of those, open the PDF itself. A citation you did not verify against the primary text is not a verified citation; if a document you need is genuinely missing from that directory, say which one rather than carrying the gap forward silently."
fi

# Give QA a worktree. Without one it builds its own — three of them on PR
# #299, one under /tmp — and nothing cleans them up. Use a review-only branch
# so it never contends with the implement worktree for the PR's own branch,
# and so it is never left detached (a detached worktree shares one target dir
# with every other detached run).
branch="$(gh_retry gh pr view "$n" -R "$AC_REPO" --json headRefName --jq .headRefName)"
[[ -n $branch ]] || { echo "no branch for PR #$n" >&2; exit 1; }
wt="$WT_BASE/pr-$n"
require_space "$wt" || exit 1

git_retry git fetch -q origin "$branch"
if [[ -d $wt ]]; then
  git -C "$wt" reset -q --hard "origin/$branch"
else
  git worktree add -B "review-pr-$n" "$wt" "origin/$branch" >/dev/null
fi
cd "$wt"
echo "review worktree: $wt  [review-pr-$n @ $branch]" >&2

link_support "$wt"

# The names step's inputs (gate.sh → stale_names.sh). $issue is read above.
names_inputs "$issue" || exit 1

# The gate runs here, outside the session: once per tree, cached, no tool
# timeout. QA reads the record instead of re-running cargo (qa.md → build and
# test). A red gate is still reviewed — the findings belong in the comment.
gate_rc=0
gate_out="$("$AC_GATE" "$wt" 2>&1)" || gate_rc=$?
printf '%s\n' "$gate_out" >&2
(( gate_rc < 2 )) || { echo "gate refused at $wt — not starting a review" >&2; exit 1; }
prompt="$prompt

Workspace gate for $head_sha (bin/gate.sh, exit $gate_rc — already run; do not
re-run fmt/clippy/test for the workspace; \$AC_GATE prints this record again):

$gate_out"

before="$(qa_comments)" || { echo "cannot reach github — not starting a review" >&2; exit 1; }
AC_TAG="pr-$n${since:+-delta}" run qa "$prompt" --read "${rest[@]+"${rest[@]}"}"
after="$(qa_comments)" || { echo "cannot verify whether qa posted — check the PR by hand" >&2; exit 1; }

# Record what was reviewed only if QA actually posted. Otherwise the next pass
# would go delta against a review that does not exist.
if (( after > before )); then
  printf '%s\n' "$head_sha" > "$mark"
else
  echo "warning: qa posted no comment — not recording $head_sha as reviewed" >&2
  exit 1
fi
