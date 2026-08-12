#!/usr/bin/env bash
# master.sh <issue-id>... [--fg]
#
# Drives one issue through implement → review → revise → review until QA stops
# objecting, then stops. It does NOT merge, close, or resolve a disagreement —
# those are your gates and stay yours.
#
# Stops on: qa approval (ready for your merge), needs-discussion,
# needs-design, needs-ux, or AC_ROUNDS exhausted.
#
#   AC_ROUNDS=3   max dev→qa cycles before handing back (default 3)
#   AC_MODEL      passed through to every session
#
# Verify your label names first — a wrong one makes this loop do nothing while
# looking like it worked:  gh label list -R mkovero/ac

source "$(dirname "$0")/common.sh"
BIN="$(cd "$(dirname "$0")" && pwd)"
ROUNDS="${AC_ROUNDS:-3}"

fg=""
ids=()
for a in "$@"; do
  case "$a" in --fg) fg="--fg" ;; *) ids+=("$a") ;; esac
done
[[ ${#ids[@]} -gt 0 ]] || { echo "usage: master.sh <issue-id>... [--fg]" >&2; exit 1; }

labels() { gh issue view "$1" -R "$AC_REPO" --json labels --jq '.labels[].name' 2>/dev/null; }
pr_labels() { gh pr view "$1" -R "$AC_REPO" --json labels --jq '.labels[].name' 2>/dev/null; }
has() { grep -qx "$1"; }

# The PR for an issue is the one whose head branch this repo's convention says
# belongs to it. Do not match on title — titles get edited.
pr_for() {
  gh pr list -R "$AC_REPO" --state open --json number,headRefName \
    --jq "[.[] | select(.headRefName | startswith(\"issue-$1\"))] | .[0].number // empty"
}

drive() {
  local n="$1" round=0 pr ls
  ls="$(labels "$n")" || { echo "  #$n: cannot read issue" >&2; return 1; }

  for stop in needs-discussion needs-design needs-ux blocked; do
    if printf '%s\n' "$ls" | has "$stop"; then
      echo "  #$n: labelled $stop — not mine to resolve"; return 0
    fi
  done

  pr="$(pr_for "$n")"

  if [[ -z $pr ]]; then
    printf '%s\n' "$ls" | has ready-to-implement \
      || { echo "  #$n: no PR and not ready-to-implement — skipping"; return 0; }
    echo "  #$n: implementing"
    "$BIN/implement.sh" "$n" $fg || { echo "  #$n: implement failed"; return 1; }
    pr="$(pr_for "$n")"
    [[ -n $pr ]] || { echo "  #$n: no PR was opened — check the session log"; return 1; }
    echo "  #$n: opened PR #$pr"
  else
    echo "  #$n: existing PR #$pr"
  fi

  while (( round < ROUNDS )); do
    (( ++round ))
    echo "  #$n PR #$pr: qa round $round"
    "$BIN/review.sh" "$pr" $fg || { echo "  #$n: review failed"; return 1; }

    ls="$(pr_labels "$pr")"
    if printf '%s\n' "$ls" | has needs-discussion; then
      echo "  #$n PR #$pr: qa escalated — needs-discussion"; return 0
    fi
    if ! printf '%s\n' "$ls" | has needs-work; then
      echo "  #$n PR #$pr: qa satisfied — yours to merge"; return 0
    fi

    if (( round >= ROUNDS )); then
      echo "  #$n PR #$pr: still needs-work after $ROUNDS rounds — stopping"
      echo "     two agents failing to converge is signal. read the reviews."
      return 0
    fi

    echo "  #$n PR #$pr: revising"
    "$BIN/revise.sh" "$pr" $fg || { echo "  #$n: revise failed"; return 1; }
    gh pr edit "$pr" -R "$AC_REPO" \
      --remove-label needs-work --add-label in-review >/dev/null 2>&1 || true
  done
}

for id in "${ids[@]}"; do
  echo "== issue #$id"
  drive "$id" || echo "  #$id: aborted"
done

echo
echo "nothing above was merged or closed. board.sh for current state."
