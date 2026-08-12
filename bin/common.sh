#!/usr/bin/env bash
# common.sh — shared by the .agents/bin runners. Source, do not execute.

set -euo pipefail

AC_REPO="${AC_REPO:-mkovero/ac}"
ROOT="$(git rev-parse --show-toplevel)"
WT_BASE="${AC_WT_BASE:-$ROOT/../ac-wt}"
GH_TOOLS="${AC_GH_TOOLS:-mcp__github}"

# Raw transcripts are debugging material: large, noisy, gitignored.
# Distilled final messages are the return channel to the planning session:
# small, readable, committed deliberately.
AC_LOG_DIR="${AC_LOG_DIR:-$HOME/.local/state/ac}"
AC_SESSION_DIR="${AC_SESSION_DIR:-$ROOT/work/sessions}"

# Agent = delegation tool. Without it a session cannot reach explorer and reads
# every file itself, in its own context.
TOOLS_WRITE="Read,Grep,Glob,Edit,Write,Bash,Agent"
TOOLS_READ="Read,Grep,Glob,Bash,Agent"

spec() { printf '%s/.agents/%s.md' "$ROOT" "$1"; }

# run <role> <prompt> [--fg] [--read] [extra claude args...]
# --fg drops into interactive Claude Code: you see everything and can steer,
# but the allowlist is not enforced — you are prompted instead, and nothing
# is written to work/sessions.
run() {
  local role="$1" prompt="$2"; shift 2
  local fg="" tools="$TOOLS_WRITE" arg
  local -a extra=()
  for arg in "$@"; do
    case "$arg" in
      --fg)   fg=1 ;;
      --read) tools="$TOOLS_READ" ;;
      *)      extra+=("$arg") ;;
    esac
  done

  if [[ -n $fg ]]; then
    claude --system-prompt-file "$(spec "$role")" "${extra[@]}" "$prompt"
    return
  fi

  local tag="${AC_TAG:-$$}"
  local raw="$AC_LOG_DIR/$(date +%F)-$role-$tag.jsonl"
  local out="$AC_SESSION_DIR/$(date +%F)-$role-$tag.md"
  mkdir -p "$AC_LOG_DIR" "$AC_SESSION_DIR"

  # Header says what this file is. Point-in-time record of one session, not
  # state — the tracker still owns whether the issue or PR is open.
  { printf '<!-- %s session, %s, %s -->\n' "$role" "$tag" "$(date -Iminutes)"
    printf '<!-- record of one run. not status. raw: %s -->\n\n' "${raw/#$HOME/\~}"
  } > "$out"

  CLAUDE_AGENT_SDK_DISABLE_BUILTIN_AGENTS=1 \
  claude -p --system-prompt-file "$(spec "$role")" "$prompt" \
    --model "${AC_MODEL:-sonnet}" \
    --allowedTools "$tools,$GH_TOOLS" \
    --permission-mode acceptEdits \
    --max-turns "${AC_MAX_TURNS:-60}" \
    --output-format stream-json --verbose "${extra[@]}" \
  | tee "$raw" \
  | tee >(jq -r --unbuffered 'select(.type=="result") | .result' >> "$out") \
  | jq -r --unbuffered '
      if .type=="assistant" then
        (.message.content[]? | select(.type=="tool_use") | "→ \(.name)")
      elif .type=="result" then "\n\(.result)"
      else empty end'

  wait                       # process substitution outlives the pipeline
  echo "session: $out" >&2
}
