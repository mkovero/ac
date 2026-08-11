#!/usr/bin/env bash
# implement.sh <issue> [--fg]
#
# Runs the developer role against one issue in its own worktree, so several
# can run at once without contending. Headless by default; --fg when you want
# to watch it, at the cost of the tool scoping (interactive mode asks you
# instead of enforcing the allowlist).

set -euo pipefail
n="${1:?usage: implement.sh <issue> [--fg]}"; fg="${2:-}"
root="$(git rev-parse --show-toplevel)"
wt="${AC_WT_BASE:-$root/../ac-wt}/issue-$n"

git fetch -q origin main
git worktree add -B "issue-$n" "$wt" origin/main >/dev/null 2>&1 || true
cd "$wt"

# absolute path: we are no longer in $root
spec="$root/.agents/developer.md"
prompt="Implement issue #$n in ${AC_REPO:-mkovero/ac}."

if [[ $fg == --fg ]]; then
  claude --system-prompt-file "$spec" "$prompt"
else
  claude -p --system-prompt-file "$spec" "$prompt" \
    --allowedTools "Read,Grep,Glob,Edit,Write,Bash" \
    --permission-mode acceptEdits \
    --max-turns 60 \
    --output-format json \
  | tee "/tmp/impl-$n.json" | jq -r '.result'
fi

echo "worktree: $wt   branch: issue-$n" >&2
