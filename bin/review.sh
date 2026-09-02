#!/usr/bin/env bash
# review.sh <pr> [--full] [--since <sha>] [--fg]
# review.sh --independent [--daemon|<pr>...]
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

  local pr head wt labels
  for pr in "${prs[@]}"; do
    labels="$(gh_retry gh pr view "$pr" -R "$AC_REPO" --json labels --jq '.labels[].name')"
    has_label() { printf '%s\n' "$labels" | grep -qx "$1"; }
    has_label claude-approved || { echo "<codex/qa> Skipping PR #$pr: claude-approved absent."; continue; }
    has_label codex-approved && continue
    has_label needs-work && continue
    has_label requires-rig && continue
    head="$(gh_retry gh pr view "$pr" -R "$AC_REPO" --json headRefOid --jq .headRefOid)"
    wt="$WT_BASE/codex-pr-$pr"
    [[ ! -e $wt ]] || { echo "review worktree already exists: $wt" >&2; return 1; }
    require_space "$wt"; mkdir -p "$WT_BASE" "$AC_TARGET"
    git fetch -q origin "pull/$pr/head"
    [[ $(git rev-parse FETCH_HEAD) == "$head" ]] || { echo "PR #$pr changed while preparing review" >&2; return 1; }
    git worktree add --detach "$wt" "$head" >/dev/null
    link_support "$wt"
    local rc=0
    ( cd "$wt" && AC_TAG="pr-$pr" run codex-qa "Review PR #$pr in $AC_REPO as the independent Codex QA worker.

Inspect the linked issue, decisions, complete diff, checks, tests, and relevant
history. On pass add codex-approved and remove needs-work; on blocking defects
add needs-work and remove codex-approved. Never touch claude-approved,
in-review, requires-rig, or agent labels. Re-check the PR HEAD before applying
the final decision; if it changed, do not approve." --read ) || rc=$?
    git worktree remove --force "$wt" || true
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
- Re-run the full mechanical gate against the new tip regardless of how small
  the delta looks: cargo test --workspace, cargo clippy -- -D warnings,
  cargo fmt --check. Not the delta's crate — the workspace. Two changes that
  each pass alone can break in combination, and that is precisely what a
  narrowed pass would miss.
- Read the delta $since..$head_sha, not the whole diff. Do not re-litigate
  parts you already accepted.
- For each point you raised in your earlier review: is it addressed? Say so
  explicitly, one line each. A point silently dropped is a point not fixed.
- The delta touches measurement values, output formatting, or display units →
  the standards check applies to it on the same terms as a first pass.
- The delta may break something outside itself. Where it plausibly does, say
  where you looked.

State at the top of your comment which commit range you reviewed.

Standards PDFs are NOT in this checkout — they are licence-restricted and gitignored. They are at $AC_STDDOCS. Each PDF has a .txt sibling extracted with pdftotext -layout: Grep that to find the clause, then Read the PDF at that region only. Do not page through a PDF looking for a clause. Extraction is lossy for equations, figures and some tables — where the clause turns on one of those, open the PDF itself. A citation you did not verify against the primary text is not a verified citation; if a document you need is genuinely missing from that directory, say which one rather than carrying the gap forward silently."
else
  prompt="Review PR #$n in $AC_REPO.

This is an explicit full review. Even if this commit already has an earlier QA
comment, this invocation is a new review pass: governing specs, issue decisions,
or human evidence may have changed without a code push. Apply the current QA
spec, post a new superseding QA comment, and update labels to its verdict. Do
not decline to post merely because the PR tip is unchanged.

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

git fetch -q origin "$branch"
if [[ -d $wt ]]; then
  git -C "$wt" reset -q --hard "origin/$branch"
else
  git worktree add -B "review-pr-$n" "$wt" "origin/$branch" >/dev/null
fi
cd "$wt"
echo "review worktree: $wt  [review-pr-$n @ $branch]" >&2

link_support "$wt"


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
