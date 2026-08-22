#!/bin/sh
# Independent Codex QA over every Claude-approved PR that Codex has not passed.
# GitHub is the state machine — this wrapper holds nothing.
set -eu

for pr in $(gh pr list --state open \
    --search 'label:claude-approved -label:codex-approved' \
    --json number --jq '.[].number'); do
  codex exec "Independent QA review of PR #$pr in mkovero/ac.
Read AGENTS.md, then .agents/codex-qa.md, then follow codex-qa.md."
done
