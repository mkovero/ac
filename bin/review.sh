#!/usr/bin/env bash
# review.sh <pr>
#
# QA gets no Edit/Write against the tree. A reviewer that can fix what it finds
# will fix it, and then the finding never reaches you as a finding. It needs
# the GitHub MCP tools to post the review, though — check `claude mcp list` and
# `/mcp` for the exact tool names in your setup, they are not guessable.

set -euo pipefail
n="${1:?usage: review.sh <pr>}"
root="$(git rev-parse --show-toplevel)"

GH_TOOLS="${AC_GH_TOOLS:-}"   # e.g. "mcp__github__create_pending_pull_request_review,..."
[[ -z $GH_TOOLS ]] && echo "note: AC_GH_TOOLS unset — QA can read but not post" >&2

claude -p --system-prompt-file "$root/.agents/qa.md" \
  "Review PR #$n in ${AC_REPO:-mkovero/ac}." \
  --allowedTools "Read,Grep,Glob,Bash${GH_TOOLS:+,$GH_TOOLS}" \
  --max-turns 40 \
  --output-format json \
| tee "/tmp/qa-$n.json" | jq -r '.result'
