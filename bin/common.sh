#!/usr/bin/env bash
# common.sh — shared by the bin/ runners. Source, do not execute.

set -euo pipefail

AC_REPO="${AC_REPO:-mkovero/ac}"
# The MAIN checkout, resolved from anywhere — including from inside a linked
# worktree. `--show-toplevel` returns the current worktree, so deriving paths
# from it nests them one level deeper on every dispatch:
#   ~/src/ac-wt/wt/ac-wt/wt/issue-340/...
# `--git-common-dir` always points at the main repo's .git, so its parent is
# the main checkout wherever this is sourced from.
ROOT="$(dirname "$(git rev-parse --path-format=absolute --git-common-dir)")"

# Where the current command is actually running. Same as ROOT in the main
# checkout; the worktree path when a script is invoked from inside one.
HERE="$(git rev-parse --show-toplevel)"

# Standards PDFs live outside the repo — licence-restricted, gitignored, so no
# worktree checkout will contain them. Roles reach them by absolute path.
export AC_STDDOCS="${AC_STDDOCS:-$ROOT/stddocs}"

# ONE root for everything this tooling generates, always beside the main
# checkout:
#   $AC_HOME/wt/<branch>   worktrees
#   $AC_HOME/target        build artifacts, SHARED
#   $AC_HOME/log           raw session transcripts
#   $AC_HOME/session       distilled session output
#
# Distilled session output lives outside the repo, alongside the logs it is
# distilled from. It used to be written to work/sessions/ and committed; that
# accumulated faster than anyone read it and cost context in every later
# session, so it is now untracked and out of tree.
AC_HOME="${AC_HOME:-$(dirname "$ROOT")/ac-wt}"
WT_BASE="${AC_WT_BASE:-$AC_HOME/wt}"
AC_LOG_DIR="${AC_LOG_DIR:-$AC_HOME/log}"
AC_SESSION_DIR="${AC_SESSION_DIR:-$AC_HOME/session}"

# One shared target dir, not one per branch. Per-branch was warm across runs on
# the same issue, but cost several GB each and left orphans behind every merge.
# Cargo locks the dir, so genuinely parallel dispatch serialises at the build
# step — which is the right trade when parallel runs are rare.
AC_TARGET="${AC_TARGET:-$AC_HOME/target}"

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

# Standards PDFs live outside the repo — licence-restricted, gitignored, so no
# worktree checkout will contain them. Roles reach them by absolute path.
export AC_STDDOCS="${AC_STDDOCS:-$ROOT/stddocs}"

# Raw transcripts: large, noisy, never committed. The distilled final message
# goes to AC_SESSION_DIR, which is also outside the repo.
AC_LOG_DIR="${AC_LOG_DIR:-$AC_HOME/log}"

# Task = delegation tool. Whether a session can actually reach a subagent is
# NOT settled by this list: `.claude/settings.json` denies `Task(Explore)` and
# run() exports CLAUDE_AGENT_SDK_DISABLE_BUILTIN_AGENTS=1, either of which is
# enough to make it dead weight. Do not infer the answer from these three
# settings — read it off a transcript, which is the only place it is observable:
#   jq -r 'select(.type=="system") | .tools // empty | .[]' <raw>
# If Task is absent there, drop it from these lists rather than leaving a tool
# in an allowlist that nothing can call.
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
    echo "  reclaim with: bin/ac-gc.sh" >&2
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
  local cfg="$wt/.cargo/config.toml"
  if [[ ! -e $cfg ]]; then
    mkdir -p "$wt/.cargo"
    printf '# written by bin/common.sh — not tracked, see /.gitignore\n[build]\ntarget-dir = "%s"\n' \
      "$AC_TARGET" > "$cfg"
  fi
  return 0
}

# Heavy trees an implementation never needs. Cheaper and more reliable than a
# Read deny rule: a file that is not on disk cannot be found by any tool.
sparse_trim() {
  local wt="$1"
  [[ -n ${AC_NO_SPARSE:-} ]] && return 0
  local -a pat
  read -r -a pat <<< "${AC_SPARSE:-/* !/work/ !/audit/}"
  git -C "$wt" sparse-checkout init --no-cone 2>/dev/null || return 0
  git -C "$wt" sparse-checkout set "${pat[@]}"
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

# The architect's file manifest for an issue: repo-relative paths, one per line.
# Empty output means no manifest — the caller decides whether that is fatal.
manifest_of() {
  local body out
  body=$(gh_retry gh issue view "$1" -R "$AC_REPO" --json comments \
    --jq '[.comments[] | select(.body | test("<!-- agent: architect -->"))] | last | .body // ""') \
    || return 1

  # Newer comments may use an explicit files fence. Prefer it because its end
  # marker is unambiguous.
  out=$(printf '%s\n' "$body" \
    | sed -n '/^```files[[:space:]]*$/,/^```[[:space:]]*$/p' \
    | sed '1d;$d; s/^[[:space:]]*//; s/[[:space:]]*$//' \
    | grep -v '^$' || true)
  if [[ -n $out ]]; then printf '%s\n' "$out"; return 0; fi

  # The architect template in existing issues uses a Markdown section with
  # one bare path per line. Stop at the next bold field and emit paths only;
  # prose such as "(none — coordination-only epic)" is not a manifest.
  printf '%s\n' "$body" | awk '
    /^\*\*file manifest\*\*[[:space:]]*$/ { in_manifest=1; next }
    in_manifest && /^\*\*/ { exit }
    in_manifest {
      line=$0
      sub(/^[[:space:]]*[-*][[:space:]]*/, "", line)
      gsub(/`/, "", line)
      sub(/[[:space:]]*$/, "", line)
      if (line ~ /^[[:alnum:]_.-]+\//) print line
    }'
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

# Provider selection, in descending precedence:
#   AC_<ROLE>_PROVIDER=codex  one role (AC_DEVELOPER_PROVIDER, AC_QA_PROVIDER...)
#   AC_PROVIDER=codex         every non-QA role in this invocation
#   claude                    backwards-compatible default
provider_for() {
  local role="$1" key value
  key="AC_${role^^}_PROVIDER"
  key="${key//-/_}"
  if [[ -n ${!key:-} ]]; then
    value="${!key}"
  elif [[ $role == qa ]]; then
    value=claude
  else
    value="${AC_PROVIDER:-claude}"
  fi
  case "$value" in
    claude|codex) printf '%s\n' "$value" ;;
    *) echo "unsupported provider '$value' for $role (expected claude or codex)" >&2; return 2 ;;
  esac
}

# A role-specific model wins, followed by the old global AC_MODEL. Provider
# defaults are deliberately separate: Claude aliases are not Codex model IDs.
model_for() {
  local role="$1" provider="$2" key value
  key="AC_${role^^}_MODEL"; key="${key//-/_}"
  value="${!key:-${AC_MODEL:-}}"
  if [[ -z $value ]]; then
    case "$provider:$role" in
      claude:architect|claude:ux|claude:triage) value="${AC_CLAUDE_MODEL:-opus}" ;;
      claude:*) value="${AC_CLAUDE_MODEL:-sonnet}" ;;
      codex:*) value="${AC_CODEX_MODEL:-}" ;;
    esac
  fi
  printf '%s\n' "$value"
}

distill_codex() {
  local raw="$1" last="$2"
  if [[ -s $last ]]; then cat "$last"; return; fi
  jq -rs '[.[] | select(.type=="item.completed") | .item
           | select(.type=="agent_message") | .text] | last // empty' \
    "$raw" 2>/dev/null || true
}

# run <role> <prompt> [--fg] [--read] [extra provider args...]
# --fg drops into the selected provider's interactive CLI: you see everything
# and can steer, and nothing is written to $AC_SESSION_DIR.
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
  local provider model
  provider="$(provider_for "$role")" || return
  model="$(model_for "$role" "$provider")"

  # The current approval labels are reviewer identities, not generic slots:
  # qa owns claude-approved and codex-qa owns codex-approved. Until those specs
  # and labels are migrated together, letting Codex occupy qa would make both
  # supposedly independent gates Codex reviews.
  if [[ $role == qa && $provider != claude ]]; then
    echo "qa provider is fixed to claude by the current two-review gate" >&2
    echo "migrate claude-approved/codex-approved to provider-neutral review slots first" >&2
    return 2
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

  command -v "$provider" >/dev/null 2>&1 \
    || { echo "provider CLI not found: $provider" >&2; return 127; }

  # Codex has no --system-prompt-file equivalent. Make reading the same role
  # spec the first task instruction; AGENTS.md is loaded by Codex itself.
  local task_prompt="$prompt"
  if [[ $provider == codex ]]; then
    task_prompt="Read $(spec "$role") fully before doing anything else. It is your role specification and is binding.

$prompt"
  fi

  if [[ -n $fg ]]; then
    # Same options as the -p run below, minus only the three that are about
    # being non-interactive: -p itself, the stream-json plumbing, and
    # --max-turns (you are sitting there and can stop it).
    #
    # This used to pass the system prompt and nothing else, so --fg ran a
    # different model with different tools under a different permission mode
    # than the run it exists to reproduce. A debugging mode that does not
    # reproduce the thing being debugged sends you after the wrong cause.
    #
    # --permission-mode still differs in effect, not in value: interactively
    # it prompts where -p auto-approves, which is the point of --fg.
    if [[ $provider == claude ]]; then
      claude --system-prompt-file "$(spec "$role")" \
        --model "$model" \
        --allowedTools "$tools${GH_TOOLS:+,$GH_TOOLS}" \
        ${deny:+--disallowedTools "$deny"} \
        --permission-mode "$mode" \
        "${extra[@]}" "$task_prompt"
    else
      # Read-only roles still run tests and gh label/comment operations. Codex
      # therefore needs a writable sandbox; the binding role spec forbids
      # source edits, like the existing Claude review convention around Bash.
      local sandbox=workspace-write
      local -a model_arg=()
      [[ -n $model ]] && model_arg=(-m "$model")
      codex -C "$PWD" -s "$sandbox" -a on-request \
        "${model_arg[@]}" "${extra[@]}" "$task_prompt"
    fi
    return
  fi

  export CARGO_TARGET_DIR="$AC_TARGET"
  mkdir -p "$CARGO_TARGET_DIR"

  local tag="${AC_TAG:-$$}" stamp status=0
  mkdir -p "$AC_LOG_DIR" "$AC_SESSION_DIR"
  stamp="$(date +%F)-$role-$tag"

  # The tag is not unique. revise.sh uses pr-<n>-rev for EVERY round, so round
  # two overwrote round one — transcript, distilled output, and the --resume id
  # with it. Same for a re-run of implement.sh on one issue in a day. Suffix
  # instead of clobbering: the run you want to read is usually the earlier one,
  # and a tool that deletes the evidence of its own cost cannot be audited.
  if [[ -e "$AC_LOG_DIR/$stamp.jsonl" || -e "$AC_SESSION_DIR/$stamp.md" ]]; then
    local i=2
    while [[ -e "$AC_LOG_DIR/$stamp-$i.jsonl" || -e "$AC_SESSION_DIR/$stamp-$i.md" ]]; do
      (( ++i ))
    done
    stamp="$stamp-$i"
  fi

  local raw="$AC_LOG_DIR/$stamp.jsonl"
  local out="$AC_SESSION_DIR/$stamp.md"
  local last="$AC_LOG_DIR/$stamp.last.md"
  local prefix="<${provider}/${role}> "

  # Stream to the terminal and retain the provider's native JSONL transcript.
  if [[ $provider == claude ]]; then
    claude -p --system-prompt-file "$(spec "$role")" "$task_prompt" \
      --model "$model" \
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
      else empty end' \
    | sed -u "s|^|$prefix|" || status=$?
  else
    local sandbox=workspace-write
    local -a model_arg=()
    [[ -n $model ]] && model_arg=(-m "$model")
    codex exec -C "$PWD" -s "$sandbox" \
      -c 'approval_policy="never"' \
      -c "sandbox_${sandbox//-/_}.network_access=true" \
      --add-dir "$AC_TARGET" --json -o "$last" \
      "${model_arg[@]}" "${extra[@]}" "$task_prompt" \
    | tee "$raw" \
    | jq -r --unbuffered '
        if .type=="item.completed" and .item.type=="agent_message" then .item.text
        elif .type=="item.started" and .item.type=="command_execution" then
          "  → Bash  " + ((.item.command // "") | gsub("[\\r\\n]+"; " ") | .[0:100])
        else empty end' \
    | sed -u "s|^|$prefix|" || status=$?
  fi

  # Header says what this file is: a point-in-time record of one run, not
  # state. The tracker still owns whether the issue or PR is open.
  local sid
  if [[ $provider == claude ]]; then
    sid=$(jq -r 'select(.type=="system") | .session_id // empty' "$raw" 2>/dev/null | head -1 || true)
  else
    sid=$(jq -r 'select(.type=="thread.started") | .thread_id // empty' "$raw" 2>/dev/null | head -1 || true)
  fi

  { printf '<!-- %s session %s — %s — exit %s -->\n' \
      "$role" "$tag" "$(date -Iminutes)" "$status"
    printf '<!-- record of one run, not status. raw: %s -->\n' "${raw/#$HOME/\~}"
    printf '<!-- provider: %s; model: %s -->\n' "$provider" "${model:-default}"
    if [[ $provider == claude ]]; then
      printf '<!-- resume: claude --resume %s -->\n\n' "${sid:-unknown}"
      distill "$raw"
    else
      printf '<!-- resume: codex exec resume %s -->\n\n' "${sid:-unknown}"
      distill_codex "$raw" "$last"
    fi
  } > "$out"

  # A capped run ends mid-task with a final message that reads like progress,
  # not like failure. Say so plainly rather than leaving it to be inferred.
  local used
  used=$(jq -r 'select(.type=="result") | .num_turns // empty' "$raw" 2>/dev/null | tail -1 || true)
  if [[ $provider == claude && -n $used ]] && (( used >= turns )); then
    echo "WARNING: hit the $turns-turn cap (used $used) — this run was cut off." >&2
    echo "  work is uncommitted in the worktree. resume:" >&2
    echo "  cd \$(git rev-parse --show-toplevel) && claude --resume ${sid:-<id>}" >&2
    echo "  or raise it: AC_MAX_TURNS=$(( turns * 2 )) ..." >&2
  fi

  [[ -s $raw ]] || echo "warning: empty transcript — check $provider exited cleanly" >&2
  echo "session: $out" >&2
  echo "raw:     $raw" >&2
  return "$status"
}
