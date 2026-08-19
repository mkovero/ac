#!/usr/bin/env bash
# board.sh — dump live tracker state for a planning session.
#
# Output is for pasting, NOT for committing. The moment it lands in a file it
# becomes a restatement of what the tracker holds authoritatively, and starts
# decaying. Regenerate it; never store it.

set -euo pipefail
R="${AC_REPO:-mkovero/ac}"

section() {  # section <label> <heading>
  local out
  out=$(gh_retry gh issue list -R "$R" --state open --label "$1" \
        --json number,title,labels \
        --jq '.[] | "- #\(.number) \(.title)  [\(.labels|map(.name)|map(select(startswith("agent:")|not))|join(", "))]"')
  [[ -n $out ]] && printf '\n## %s\n%s\n' "$2" "$out"
}

printf '# board — %s — %s\n' "$R" "$(date -Iminutes)"

# what can actually be dispatched right now, in parallel: spec-complete AND
# not waiting on a predecessor. this is the list to fan out from.
disp=$(gh_retry gh issue list -R "$R" --state open --label ready-to-implement \
       --json number,title,labels \
       --jq '.[] | select(.labels|map(.name)|index("blocked")|not) | "- #\(.number) \(.title)"')
[[ -n $disp ]] && printf '\n## dispatchable\n%s\n' "$disp"

section requires-rig      "awaiting a rig measurement — only you can clear these"
section blocks-others     "blocking other work — clear these first"
section blocked           "blocked (lift condition is in the comment that applied it)"
section needs-discussion  "needs human input"
section needs-design      "awaiting architect"
section needs-work        "QA sent back"

prs=$(gh_retry gh pr list -R "$R" --json number,title,isDraft,labels \
      --jq '.[] | "- #\(.number) \(.title)\(if .isDraft then " (draft)" else "" end)\(if (.labels|map(.name)|index("requires-rig")) then "  [REQUIRES RIG]" else "" end)"')
[[ -n $prs ]] && printf '\n## open PRs\n%s\n' "$prs"
