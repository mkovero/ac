#!/usr/bin/env bash
# common.sh — shared by the .agents/bin runners. Source, do not execute.

set -euo pipefail

AC_REPO="${AC_REPO:-mkovero/ac}"
ROOT="$(git rev-parse --show-toplevel)"

# ONE root for everything this tooling generates. Previously worktrees lived in
# ~/src/ac-wt, build artifacts in ~/.cache/ac-target and transcripts in
# ~/.local/state/ac — three places to check for disk use and three to clean.
#
#   $AC_HOME/wt/<branch>       worktrees
#   $AC_HOME/target/<branch>   build artifacts
#   $AC_HOME/log/              raw session transcripts
#
# Distilled session output stays in work/sessions/ inside the repo: that is the
# return channel to the planning chat and belongs in git.
AC_HOME="${AC_HOME:-$(dirname "$ROOT")/ac-wt}"
WT_BASE="${AC_WT_BASE:-$AC_HOME/wt}"
# No github MCP server is connected — roles reach the tracker through `gh` in
# Bash. Set AC_GH_TOOLS if you add one; the tool names come from `claude mcp list`.
GH_TOOLS="${AC_GH_TOOLS:-}"

# cargo test --workspace on five crates with egui exceeds the default Bash
# timeout, and a session that hits it backgrounds the build instead — then ends
# its turn with nothing to wait on, and the -p process exits taking the
# background task with it. Give it room to run synchronously.
export BASH_DEFAULT_TIMEOUT_MS="${AC_BASH_TIMEOUT_MS:-900000}"
export BASH_MAX_TIMEOUT_MS="${AC_BASH_MAX_TIMEOUT_MS:-1800000}"

# Compile parallelism. Leave headroom: a full-throttle build starves the box
# this is running on, and on a host with cores reserved for RT audio it will
# spill onto them unless -j is bounded or the process is pinned. Set
# AC_CARGO_JOBS to the cores you are willing to give up, not to nproc.
export CARGO_BUILD_JOBS="${AC_CARGO_JOBS:-$(( $(nproc) > 4 ? $(nproc) - 2 : 1 ))}"

# Test parallelism is a separate knob from compile parallelism, and for
# measurement code the right value is often lower: timing-sensitive tests that
# share a machine get flakier, not faster. Unset by default — set it if the
# suite turns out to contend.
if [[ -n ${AC_TEST_THREADS:-} ]]; then export RUST_TEST_THREADS="$AC_TEST_THREADS"; fi

# Per branch, not per worktree and not shared: one dir across worktrees means
# lock contention and fingerprint churn; one per worktree means every dispatch
# pays a cold build. Per branch is warm across runs on the same issue and
# isolated between them.
AC_TARGET_ROOT="${AC_TARGET_ROOT:-$AC_HOME/target}"

# gh through a retry. GitHub 5xx and rate-limit responses are transient and
# common enough to break a long run; a real error (404, auth, bad argument) is
# returned immediately rather than retried.
#
# Call it with the full command: `gh_retry gh pr view 12 ...`.
#
# Critically, a failed call must never look like an empty result: an empty
# label list reads as "no needs-work", which reads as "qa approved". Callers
# check the exit status, and the helpers below fail loudly rather than
# defaulting.
gh_retry() {
  local tries="${AC_GH_RETRIES:-5}" i=1 rc err out
  err="$(mktemp)"
  while :; do
    if out="$(command "$@" 2>"$err")"; then
      rm -f "$err"; printf '%s' "$out"; return 0
    fi
    rc=$?
    if ! grep -qEi 'HTTP (5[0-9]{2}|429)|timed? ?out|temporarily|no server is currently|connection reset|unexpected EOF|EOF occurred|TLS handshake' "$err"; then
      cat "$err" >&2; rm -f "$err"; return "$rc"      # real error — do not retry
    fi
    if (( i >= tries )); then
      echo "gh failed after $tries attempts:" >&2; cat "$err" >&2
      rm -f "$err"; return "$rc"
    fi
    echo "  gh transient error — retry $i/$tries in $(( 2 ** i ))s" >&2
    sleep $(( 2 ** i ))
    (( ++i ))
  done
}

# Fail fast and clearly when the API is down, rather than midway through a loop.
gh_up() {
  gh_retry gh api rate_limit --jq '.rate.remaining' >/dev/null 2>&1 && return 0
  echo "GitHub API unreachable — not starting. Check https://www.githubstatus.com" >&2
  return 1
}

# One place that decides where a worktree's build artifacts go. Used by run(),
# by the per-worktree cargo config, and by ac-gc.sh.
target_dir_for() { printf '%s/%s\n' "$AC_TARGET_ROOT" "${1:-detached}"; }

# Standards PDFs live outside the repo — licence-restricted, gitignored, so no
# worktree checkout will contain them. Roles reach them by absolute path.
export AC_STDDOCS="${AC_STDDOCS:-$ROOT/stddocs}"

# Raw transcripts: large, noisy, never committed. The distilled final message
# goes to AC_SESSION_DIR, which is in the repo.
AC_LOG_DIR="${AC_LOG_DIR:-$AC_HOME/log}"
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

# Edit is denied so a reviewer cannot quietly patch what it should be
# reporting. Write is NOT denied: a review has to be composed somewhere before
# `gh pr review --body-file` can post it, and with Bash and python3 available
# the denial blocked nothing while costing several turns per run discovering a
# workaround. This is a convention against fixing-instead-of-reporting, not an
# enforced sandbox — that would need a Bash command allowlist.
DENY_READ="Edit,NotebookEdit"

spec() { printf '%s/.agents/%s.md' "$ROOT" "$1"; }

# Gitignored directories a worktree needs but will not get from a checkout.
# stddocs holds the standards PDFs; without it qa.md's "consult document, no
# memory" rule cannot be followed, and the pass degrades to an open note while
# still reading like a completed review.
# Where is <branch> checked out, if anywhere? A branch can live in only one
# worktree at a time, so a revise must reuse the implement worktree rather than
# try to create a second one.
worktree_of_branch() {
  git worktree list --porcelain 2>/dev/null | awk -v b="refs/heads/$1" '
    /^worktree /  { wt = $2 }
    /^branch /    { if ($2 == b) { print wt; exit } }'
}

# Resolve a worktree for <branch>, preferring <path>. Emits the path to use.
# Handles: already checked out elsewhere; local branch exists; neither.
ensure_worktree() {
  local branch="$1" want="$2" existing
  existing="$(worktree_of_branch "$branch")"
  if [[ -n $existing && -d $existing ]]; then
    printf '%s\n' "$existing"; return 0
  fi
  git fetch -q origin "$branch" 2>/dev/null || true
  if git show-ref -q "refs/heads/$branch"; then
    git worktree add "$want" "$branch" >/dev/null 2>&1 || return 1
  else
    git worktree add --track -b "$branch" "$want" "origin/$branch" >/dev/null 2>&1 || return 1
  fi
  printf '%s\n' "$want"
}

# A cold workspace build is several GB. Running out mid-session leaves a
# half-written worktree and a session that fails in a confusing way, so check
# before creating one rather than after.
require_space() {
  local path="$1" need="${AC_MIN_FREE_GB:-15}" avail
  mkdir -p "$(dirname "$path")" 2>/dev/null || true
  avail=$(df -BG --output=avail "$(dirname "$path")" 2>/dev/null | tail -1 | tr -dc '0-9')
  [[ -z $avail ]] && return 0
  if (( avail < need )); then
    echo "refusing to start: ${avail}G free, need ${need}G." >&2
    echo "  reclaim with: .agents/bin/ac-gc.sh" >&2
    echo "  or override:  AC_MIN_FREE_GB=5 ..." >&2
    return 1
  fi
  if (( avail < need * 2 )); then
    echo "note: ${avail}G free — getting tight" >&2
  fi
  return 0
}

link_support() {
  local wt="$1" d
  for d in ${AC_SUPPORT_DIRS:-stddocs}; do
    if [[ -e "$ROOT/$d" && ! -e "$wt/$d" ]]; then
      ln -s "$ROOT/$d" "$wt/$d"
    fi
  done

  # Pin the target dir on the worktree itself, not just on the session. Without
  # this, any cargo you run by hand in the worktree builds into <wt>/ac-rs/
  # target — which is where the multi-GB strays came from. Cargo walks up from
  # ac-rs/ and finds this; ac-rs/.cargo/config.toml (tracked, holds the mold
  # settings) still applies, and the deeper file wins on any shared key.
  #
  # Needs `/.cargo/` in the repo .gitignore — root-anchored, so the tracked
  # ac-rs/.cargo is unaffected. Without that a developer session doing
  # `git add -A` will commit it.
  local br cfg
  br="$(git -C "$wt" branch --show-current 2>/dev/null)"
  cfg="$wt/.cargo/config.toml"
  if [[ -n $br && ! -e $cfg ]]; then
    mkdir -p "$wt/.cargo"
    printf '# written by .agents/bin — not tracked, see /.gitignore\n[build]\ntarget-dir = "%s"\n' \
      "$(target_dir_for "$br")" > "$cfg"
  fi
  return 0
}

# Count QA's output on a PR. It may land as an issue comment OR as a review
# (gh pr review --comment creates the latter, and --json comments does not
# return those). Count both, or a good review reads as silence.
# Returns the count, or fails. Never 0-on-error: that would read as "qa said
# nothing" when the truth is "we could not ask".
qa_evidence() {
  local c r
  c=$(gh_retry gh pr view "$1" -R "$AC_REPO" --json comments \
      --jq '[.comments[] | select(.body | test("agent: *qa"; "i"))] | length') || return 1
  r=$(gh_retry gh pr view "$1" -R "$AC_REPO" --json reviews \
      --jq '[.reviews[] | select(.body | test("agent: *qa"; "i"))] | length') || return 1
  echo $(( ${c:-0} + ${r:-0} ))
}

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

  # Read-heavy roles need more turns than a focused implementation: qa reruns
  # the gate, reads the diff, then chases each acceptance criterion through the
  # tests. Hitting the cap mid-investigation costs the whole run — it ends with
  # nothing posted, which is indistinguishable from having found nothing.
  local turns="${AC_MAX_TURNS:-}"
  if [[ -z $turns ]]; then
    case "$role" in
      developer)          turns=160 ;;  # implementation across crates is long
      qa|architect|audit) turns=120 ;;
      *)                  turns=80  ;;
    esac
  fi
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

  # Set here, not at source time: the caller has cd'd into its worktree by now.
  local branch
  branch="$(git branch --show-current 2>/dev/null || echo detached)"
  export CARGO_TARGET_DIR="$(target_dir_for "$branch")"
  mkdir -p "$CARGO_TARGET_DIR"

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
    --max-turns "$turns" \
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

  # A capped run ends mid-task with a final message that reads like progress,
  # not like failure. Say so plainly rather than leaving it to be inferred.
  local used
  used=$(jq -r 'select(.type=="result") | .num_turns // empty' "$raw" 2>/dev/null | tail -1 || true)
  if [[ -n $used ]] && (( used >= turns )); then
    echo "WARNING: hit the $turns-turn cap (used $used) — this run was cut off." >&2
    echo "  work is uncommitted in the worktree. resume:" >&2
    echo "  cd \$(git rev-parse --show-toplevel) && claude --resume ${sid:-<id>}" >&2
    echo "  or raise it: AC_MAX_TURNS=$(( turns * 2 )) ..." >&2
  fi

  [[ -s $raw ]] || echo "warning: empty transcript — check claude exited cleanly" >&2
  echo "session: $out" >&2
  echo "raw:     $raw" >&2
  return "$status"
}
