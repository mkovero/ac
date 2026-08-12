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

# Extract the session's final message from a finished transcript.
# Prefer the result event; fall back to the last assistant text block, because
# not every version emits result into the stream — an interrupted run has none
# either, and a header-only session file is worse than a partial one.
distill() {
  local raw="$1" r
  r=$(jq -r 'select(.type=="result") | .result // empty' "$raw" 2>/dev/null || true)
  if [[ -n $r ]]; then printf '%s\n' "$r"; return; fi
  jq -rs '[.[] | select(.type=="assistant") | .message.content[]?
           | select(.type=="text") | .text] | last // empty' "$raw" 2>/dev/null || true
}

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

  local tag="${AC_TAG:-$$}" stamp status=0
  stamp="$(date +%F)-$role-$tag"
  local raw="$AC_LOG_DIR/$stamp.jsonl"
  local out="$AC_SESSION_DIR/$stamp.md"
  mkdir -p "$AC_LOG_DIR" "$AC_SESSION_DIR"

  # Stream to the terminal, keep the raw transcript. Distillation happens after
  # the run, not inside the pipe — a process-substitution tee races the
  # pipeline's exit and truncates exactly the long sessions worth reading.
  CLAUDE_AGENT_SDK_DISABLE_BUILTIN_AGENTS=1 \
  claude -p --system-prompt-file "$(spec "$role")" "$prompt" \
    --model "${AC_MODEL:-sonnet}" \
    --allowedTools "$tools,$GH_TOOLS" \
    --permission-mode acceptEdits \
    --max-turns "${AC_MAX_TURNS:-60}" \
    --output-format stream-json --verbose "${extra[@]}" \
  | tee "$raw" \
  | jq -r --unbuffered '
      if .type=="assistant" then
        (.message.content[]? | select(.type=="tool_use") | "→ \(.name)")
      elif .type=="result" then "\n\(.result)"
      else empty end' || status=$?

  # Header says what this file is: a point-in-time record of one run, not
  # state. The tracker still owns whether the issue or PR is open.
  { printf '<!-- %s session %s — %s — exit %s -->\n' \
      "$role" "$tag" "$(date -Iminutes)" "$status"
    printf '<!-- record of one run, not status. raw: %s -->\n\n' "${raw/#$HOME/\~}"
    distill "$raw"
  } > "$out"

  [[ -s $raw ]] || echo "warning: empty transcript — check claude exited cleanly" >&2
  echo "session: $out" >&2
  echo "raw:     $raw" >&2
  return "$status"
}
