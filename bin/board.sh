#!/usr/bin/env bash
# board.sh — dump live tracker state for a planning session.
#
# Output is for pasting, NOT for committing. The moment it lands in a file it
# becomes a restatement of what the tracker holds authoritatively, and starts
# decaying. Regenerate it; never store it.
#
# stdout is the whole board or nothing. Every query's status is checked; the
# first failure names its section on stderr and exits non-zero before anything
# reaches stdout, because a board with sections missing reads as "no work" —
# the same harm as an empty one. A section that was queried and has no rows
# prints "(none)", so "no work" and "could not ask" stay distinguishable.

set -euo pipefail
source "$(dirname "$0")/common.sh"
R="$AC_REPO"

board=""

fail() {  # fail <heading> <rc>
  echo "board: $1: gh query failed (exit $2)" >&2
  exit "$2"
}

emit() {  # emit <heading> <rows>
  printf -v board '%s\n## %s\n%s\n' "$board" "$1" "${2:-(none)}"
}

section() {  # section <label> <heading>
  local out rc=0
  out=$(gh_retry gh issue list -R "$R" --state open --label "$1" \
        --json number,title,labels \
        --jq '.[] | "- #\(.number) \(.title)  [\(.labels|map(.name)|map(select(startswith("agent:")|not))|join(", "))]"') \
    || rc=$?
  (( rc == 0 )) || fail "$2" "$rc"
  emit "$2" "$out"
}

printf -v board '# board — %s — %s\n' "$R" "$(date -Iminutes)"

# what can actually be dispatched right now, in parallel: spec-complete AND
# not held by any label master.sh refuses to dispatch. The exclusion list
# mirrors master.sh's per-issue refusal block (blocked … epic, before
# `routed`) plus the design/ux routing labels; nothing checks the two stay in
# sync, so bin/board_test.sh pins each label here by name.
rc=0
disp=$(gh_retry gh issue list -R "$R" --state open --label ready-to-implement \
       --json number,title,labels \
       --jq '.[] | select(.labels|map(.name) as $l
                 | ["blocked","needs-ux","site-visit","needs-design","needs-discussion","needs-clarification","epic"]
                 | any(. as $x | $l | index($x))
                 | not)
             | "- #\(.number) \(.title)"') \
  || rc=$?
(( rc == 0 )) || fail "dispatchable" "$rc"
emit "dispatchable" "$disp"

section requires-rig      "awaiting a rig measurement — only you can clear these"
section blocks-others     "blocking other work — clear these first"
section blocked           "blocked (lift condition is in the comment that applied it)"
section needs-discussion  "needs human input"
section needs-design      "awaiting architect"
section needs-work        "QA sent back"

rc=0
prs=$(gh_retry gh pr list -R "$R" --json number,title,isDraft,labels \
      --jq '.[] | "- #\(.number) \(.title)\(if .isDraft then " (draft)" else "" end)\(if (.labels|map(.name)|index("requires-rig")) then "  [REQUIRES RIG]" else "" end)"') \
  || rc=$?
(( rc == 0 )) || fail "open PRs" "$rc"
emit "open PRs" "$prs"

printf '%s' "$board"
