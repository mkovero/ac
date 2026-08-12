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

if [[ $mode == auto ]]; then
  if [[ -f $mark ]]; then
    since="$(cat "$mark")"
    [[ $since == "$head_sha" ]] && echo "note: no new commits since last review" >&2
    mode=delta
  else
    mode=full
  fi
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

State at the top of your comment which commit range you reviewed."
else
  prompt="Review PR #$n in $AC_REPO."
fi

AC_TAG="pr-$n${since:+-delta}" run qa "$prompt" --read "${rest[@]+"${rest[@]}"}"

# Record what was reviewed, so the next pass knows where to start.
printf '%s\n' "$head_sha" > "$mark"
