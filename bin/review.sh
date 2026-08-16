#!/usr/bin/env bash
# review.sh <pr> [--full] [--since <sha>] [--fg]
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
head_sha="$(gh pr view "$n" -R "$AC_REPO" --json headRefOid --jq .headRefOid)"

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

Standards PDFs are NOT in this checkout — they are licence-restricted and gitignored. They are at $AC_STDDOCS. Open them there by absolute path. A citation you did not verify against the primary text is not a verified citation; if a document you need is genuinely missing from that directory, say which one rather than carrying the gap forward silently."
else
  prompt="Review PR #$n in $AC_REPO.

Standards PDFs are NOT in this checkout — they are licence-restricted and gitignored. They are at $AC_STDDOCS. Open them there by absolute path. A citation you did not verify against the primary text is not a verified citation; if a document you need is genuinely missing from that directory, say which one rather than carrying the gap forward silently."
fi

# Give QA a worktree. Without one it builds its own — three of them on PR
# #299, one under /tmp — and nothing cleans them up.
wt="$WT_BASE/pr-$n"
branch="$(gh pr view "$n" -R "$AC_REPO" --json headRefName --jq .headRefName)"
if [[ -d $wt ]]; then
  git -C "$wt" fetch -q origin "$branch" && git -C "$wt" reset -q --hard "origin/$branch"
else
  git fetch -q origin "$branch"
  git worktree add --force "$wt" "origin/$branch" >/dev/null 2>&1 \
    || git worktree add --force --track -b "review-$branch" "$wt" "origin/$branch" >/dev/null
fi
cd "$wt"
echo "review worktree: $wt" >&2
link_support "$wt"


before="$(qa_comments)"
AC_TAG="pr-$n${since:+-delta}" run qa "$prompt" --read "${rest[@]+"${rest[@]}"}"
after="$(qa_comments)"

# Record what was reviewed only if QA actually posted. Otherwise the next pass
# would go delta against a review that does not exist.
if (( after > before )); then
  printf '%s\n' "$head_sha" > "$mark"
else
  echo "warning: qa posted no comment — not recording $head_sha as reviewed" >&2
  exit 1
fi
