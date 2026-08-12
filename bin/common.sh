#!/usr/bin/env bash
# common.sh — shared by the .agents/bin runners. Source, do not execute.

set -euo pipefail

AC_REPO="${AC_REPO:-mkovero/ac}"
ROOT="$(git rev-parse --show-toplevel)"
WT_BASE="${AC_WT_BASE:-$ROOT/../ac-wt}"
# No github MCP server is connected — roles reach the tracker through `gh` in
# Bash. Set AC_GH_TOOLS if you add one; the tool names come from `claude mcp list`.
GH_TOOLS="${AC_GH_TOOLS:-}"

# Raw transcripts are debugging material: large, noisy, gitignored.
# Distilled final messages are the return channel to the planning session:
# small, readable, committed deliberately.
AC_LOG_DIR="${AC_LOG_DIR:-$HOME/.local/state/ac}"
AC_SESSION_DIR="${AC_SESSION_DIR:-$ROOT/work/sessions}"

# Task = delegation tool. Without it a session cannot reach explorer and reads
# every file itself, in its own context. Verify against a transcript after any
# upgrade:  jq -r 'select(.type=="system") | .tools // empty | .[]' <raw>
TOOLS_WRITE="Read,Grep,Glob,Edit,Write,Bash,Task"
TOOLS_READ="Read,Grep,Glob,Bash,Task"

# --allowedTools is an AUTO-APPROVE list, not a sandbox: tools absent from it
# still run. Only --disallowedTools binds. Everything below is therefore load
# bearing, not belt-and-braces.
#
# Async and scheduling tools are incoherent in a -p run: the process exits at
# end of turn, so a session that schedules a wakeup or spawns a watcher ends
# having done nothing and reports as if it had. Deny them everywhere.
DENY_ASYNC="ScheduleWakeup,Monitor,PushNotification,RemoteTrigger,SendMessage,\
CronCreate,CronDelete,CronList,TaskCreate,TaskGet,TaskList,TaskOutput,TaskStop,\
TaskUpdate,EnterWorktree,ExitWorktree"

# Note this does NOT make a role read-only: Bash is granted, and `sed -i`
# writes files as well as Edit does. Real enforcement needs a Bash command
# allowlist in .claude/settings.json.
DENY_READ="Edit,Write,NotebookEdit"

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
  local fg="" tools="$TOOLS_WRITE" deny="$DENY_ASYNC" mode="acceptEdits" arg
  local -a extra=()
  for arg in "$@"; do
    case "$arg" in
      --fg)   fg=1 ;;
      --read) tools="$TOOLS_READ"; deny="$DENY_READ,$DENY_ASYNC"; mode="default" ;;
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
    --allowedTools "$tools${GH_TOOLS:+,$GH_TOOLS}" \
    ${deny:+--disallowedTools "$deny"} \
    --permission-mode "$mode" \
    --max-turns "${AC_MAX_TURNS:-60}" \
    --output-format stream-json --verbose "${extra[@]}" \
  | tee "$raw" \
  | jq -r --unbuffered '
      def arg: (.input.file_path // .input.pattern // .input.command
                // .input.description // "") | tostring
               | gsub("[\r\n]+"; " ") | .[0:100];
      if .type=="assistant" then
        (.message.content[]?
         | if .type=="text" then .text
           elif .type=="tool_use" then "  → \(.name)  \(arg)"
           else empty end)
      elif .type=="result" then "\n\(.result)"
      else empty end' || status=$?

  # Header says what this file is: a point-in-time record of one run, not
  # state. The tracker still owns whether the issue or PR is open.
  local sid
  sid=$(jq -r 'select(.type=="system") | .session_id // empty' "$raw" 2>/dev/null | head -1 || true)

  { printf '<!-- %s session %s — %s — exit %s -->\n' \
      "$role" "$tag" "$(date -Iminutes)" "$status"
    printf '<!-- record of one run, not status. raw: %s -->\n' "${raw/#$HOME/\~}"
    printf '<!-- resume: claude --resume %s -->\n\n' "${sid:-unknown}"
    distill "$raw"
  } > "$out"

  [[ -s $raw ]] || echo "warning: empty transcript — check claude exited cleanly" >&2
  echo "session: $out" >&2
  echo "raw:     $raw" >&2
  return "$status"
}
