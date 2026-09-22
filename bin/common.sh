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
# Where run() records a provider account limit. master.sh sets a per-run path.
export AC_LIMIT_FILE="${AC_LIMIT_FILE:-$AC_LOG_DIR/provider-limit}"
AC_SESSION_DIR="${AC_SESSION_DIR:-$AC_HOME/session}"

# One target dir per worktree: $AC_TARGET/wt/<name> for $AC_HOME/wt/<name>.
# Under target/ on purpose — $AC_HOME is a git repo whose .gitignore already
# excludes target/ and log/; a new top-level directory would not be.
#
# A target dir shared across worktrees is not merely slow, it is wrong. Cargo
# decides freshness from mtimes, so a worktree whose files are older than the
# last build from ANOTHER worktree reports `Fresh` and runs that other tree's
# code (reproduced on cargo 1.95, 2026-09-16: worktree b printed a's output).
# Claude sessions knew this from operator memory and made their own
# target-qa-<n> dirs, cold, every pass — 50 of 141 qa runs. Codex sessions did
# not, and built into the shared dir.
#
# A new one is seeded by reflink copy (btrfs: ~2 s for 18 GB) from the newest
# existing target, with the workspace crates' fingerprints removed: registry
# dependencies stay warm, every workspace crate rebuilds from this worktree.
# $AC_TARGET/debug, the old shared build, is still read as a seed.
AC_TARGET="${AC_TARGET:-$AC_HOME/target}"
AC_TARGETS="${AC_TARGETS:-$AC_TARGET/wt}"

# bin/gate.sh records, one directory per tree+toolchain. Logs, so under log/.
export AC_GATE_DIR="${AC_GATE_DIR:-$AC_HOME/log/gate}"
export AC_GATE="$ROOT/bin/gate.sh"

target_for() { printf '%s/%s\n' "$AC_TARGETS" "$(basename "$1")"; }

# Strip the workspace crates' fingerprints so cargo rebuilds them from <wt>.
unfingerprint() {
  local wt="$1" t="$2" names n
  names="$(cargo metadata --no-deps --format-version 1 \
             --manifest-path "$wt/ac-rs/Cargo.toml" 2>/dev/null \
           | jq -r '.packages[].name' 2>/dev/null)" || true
  [[ -n $names ]] || names="ac-core ac-daemon ac-cli ac-scene ac-view"
  for n in $names; do rm -rf "$t/debug/.fingerprint/$n-"*; done
}

# prepare_target <worktree> — create (seeding if possible) and print its target.
prepare_target() {
  local wt="$1" t seed="" cand names n
  t="$(target_for "$wt")"
  # A target is only trusted for the worktree path that stamped it. Anything
  # else — a name reused by another path, a dir made by hand — loses its
  # workspace fingerprints before first use here.
  if [[ -d $t/debug && $(cat "$t/.worktree" 2>/dev/null) != "$wt" ]]; then
    unfingerprint "$wt" "$t"
    printf '%s\n' "$wt" > "$t/.worktree"
  fi
  if [[ ! -d $t/debug ]]; then
    mkdir -p "$t"
    # Newest candidate that no cargo is building into right now: a copy taken
    # mid-write could carry a torn dependency artifact with a valid fingerprint.
    for cand in $(ls -dt "$AC_TARGETS"/*/debug "$AC_TARGET/debug" 2>/dev/null); do
      [[ $cand == "$t/debug" ]] && continue
      if [[ -e $cand/.cargo-lock ]] && ! flock -n "$cand/.cargo-lock" true 2>/dev/null; then
        continue
      fi
      seed="$cand"; break
    done
    if [[ -n $seed ]] && cp -a --reflink=always "$seed" "$t/" 2>/dev/null; then
      unfingerprint "$wt" "$t"
      echo "target: seeded $t from $seed" >&2
    else
      rm -rf "$t/debug"
      echo "target: cold $t (no reflink seed)" >&2
    fi
    printf '%s\n' "$wt" > "$t/.worktree"
  fi
  printf '%s\n' "$t"
}

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
    # rc must come from the command itself. `if cmd; then …; fi; rc=$?` reads
    # the if's status, which is 0 when no branch ran — every failure then
    # returned 0 with empty output, and every `|| { …stopping; }` guard on a
    # gh_retry call was dead (an outage on 2026-09-16 read a PR head as "").
    rc=0
    out="$(command "$@" 2>"$err")" || rc=$?
    if (( rc == 0 )); then
      rm -f "$err"; printf '%s' "$out"; return 0
    fi
    # Transient: server-side trouble, and the local network being down.
    # "could not resolve host" is DNS; GraphQL's "Could not resolve to a
    # PullRequest" is a real error and must not match.
    if ! grep -qEi 'HTTP (5[0-9]{2}|429)|timed? ?out|temporarily|no server is currently|connection reset|unexpected EOF|EOF occurred|TLS handshake|network is unreachable|error connecting to|could not resolve host|connection refused|no route to host|dial tcp' "$err"; then
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

# git through a retry, for the network half of git (fetch, pull, push). Only a
# network failure is retried; an auth or ref error returns at once. On
# 2026-09-17 a DNS outage made one plain `git fetch` lose a Codex recheck and
# stopped a queue.
git_retry() {
  local tries="${AC_GIT_RETRIES:-5}" i=1 rc err
  err="$(mktemp)"
  while :; do
    rc=0
    command "$@" 2>"$err" || rc=$?
    if (( rc == 0 )); then cat "$err" >&2; rm -f "$err"; return 0; fi
    if ! grep -qEi 'could not resolve host|network is unreachable|connection (timed out|refused|reset)|operation timed out|temporary failure in name resolution|early EOF|the remote end hung up unexpectedly' "$err"; then
      cat "$err" >&2; rm -f "$err"; return "$rc"
    fi
    if (( i >= tries )); then
      echo "git failed after $tries attempts:" >&2; cat "$err" >&2
      rm -f "$err"; return "$rc"
    fi
    echo "  git network error — retry $i/$tries in $(( 15 * i ))s" >&2
    sleep $(( 15 * i ))
    (( ++i ))
  done
}

# remove_worktree <wt> — the worktree and its per-worktree target dir. Since
# #484 each worktree has its own (~25 GB apparent) target; removing only the
# worktree left them behind, and on 2026-09-17 they took the disk from 77 GB
# to 5 GB free overnight.
remove_worktree() {
  local wt="$1" t
  t="$(target_for "$wt")"
  git worktree remove --force "$wt" >/dev/null 2>&1 || true
  [[ -d $wt ]] || rm -rf "$t"
}

# rig_verdict_of <record text> — the last `**rig verdict:** X` line's X:
# pass | fail | decline-site | decline, or empty. decline-site is listed first
# so the plain `decline` alternative cannot claim its prefix.
rig_verdict_of() {
  grep -oE '\*\*rig verdict:\*\*[[:space:]]*(pass|fail|decline-site|decline)([^[:alnum:]-]|$)' <<<"$1" \
    | tail -1 | sed -E 's/.*\*\*[[:space:]]*//; s/[^[:alnum:]-]+$//' || true
}

# file_rig_record <pr> <rev12> <stamp> <src> — file one rig pass's record as
# $AC_SESSION_DIR/<YYYY-MM-DD>-rig-pr-<pr>-<rev12>-<HHMMSS>Z.md and print that
# path. <stamp> is one UTC clock read, `date -u +%FT%H%M%SZ`: date and time
# from separate reads misorder two passes straddling UTC midnight on a non-UTC
# host. One file per pass, so a later pass at the same head sorts after the
# earlier one and never replaces it (#549). The write is no-clobber: if the
# name exists, the existing file stays byte-identical, <src> is kept beside it
# as <name>.refused-<pid>, both paths are named and the return is 1.
file_rig_record() {
  local pr="$1" rev="$2" stamp="$3" src="$4" out keep
  if [[ ! $stamp =~ ^([0-9]{4}-[0-9]{2}-[0-9]{2})T([0-9]{6}Z)$ ]]; then
    echo "file_rig_record: stamp '$stamp' is not YYYY-MM-DDTHHMMSSZ" >&2
    return 2
  fi
  out="$AC_SESSION_DIR/${BASH_REMATCH[1]}-rig-pr-$pr-$rev-${BASH_REMATCH[2]}.md"
  mkdir -p "$AC_SESSION_DIR" || return 1
  if ( set -o noclobber; cat "$src" > "$out" ) 2>/dev/null; then
    printf '%s\n' "$out"
    return 0
  fi
  keep="$out.refused-$$"
  cp "$src" "$keep" || keep="(could not keep a copy; source was $src)"
  echo "file_rig_record: $out already exists; left it unchanged, this pass's record is at $keep" >&2
  return 1
}

# provider_limit_check <file> <provider> <mode> — did the provider stop on an
# account limit? mode `jsonl` reads only the provider's own result/error
# records, `text` only the tail of plain output. In both, the phrase must START
# a line (optionally after "Error:"/"ERROR:"): a provider's limit notice is the
# whole message, while a session that finished normally and merely quotes the
# phrase in its summary (reviewing this function, say) has it mid-sentence.
# On a hit: writes $AC_LIMIT_FILE and returns 0.
provider_limit_check() {
  local file="$1" provider="$2" mode="$3" text re
  re="^[[:space:]]*((Error|ERROR):[[:space:]]*)?(You've|You have) hit your (session|usage|weekly) limit[^\"]{0,100}"
  [[ -s $file ]] || return 1
  if [[ $mode == jsonl ]]; then
    if [[ $provider == claude ]]; then
      text="$(jq -r 'select(.type=="result") | .result // empty' "$file" 2>/dev/null | tail -5)"
    else
      text="$(jq -r 'select(.type=="error") | .message // empty' "$file" 2>/dev/null | tail -5)"
    fi
  else
    text="$(tail -20 "$file")"
  fi
  text="$(grep -oE "$re" <<<"$text" | tail -1)" || true
  [[ -n $text ]] || return 1
  mkdir -p "$(dirname "$AC_LIMIT_FILE")"
  printf '%s %s: %s\n' "$(date -u +%FT%TZ)" "$provider" "$text" > "$AC_LIMIT_FILE"
  echo "PROVIDER LIMIT ($provider): $text" >&2
  return 0
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
#
# awk reads to EOF instead of exiting on the match. An early exit closes the
# pipe while git is still writing; git dies of SIGPIPE, pipefail makes that the
# function's status, and set -e then kills the caller at the assignment with no
# message. With ~50 worktrees that happened on most calls.
worktree_of_branch() {
  git worktree list --porcelain 2>/dev/null | awk -v b="refs/heads/$1" '
    /^worktree /  { wt = $2 }
    /^branch /    { if (!found && $2 == b) { print wt; found = 1 } }'
}

# Resolve a worktree for <branch>, preferring <path>. Emits the path to use.
# Handles: already checked out elsewhere; local branch exists; neither.
ensure_worktree() {
  local branch="$1" want="$2" existing
  existing="$(worktree_of_branch "$branch")"
  if [[ -n $existing && -d $existing ]]; then
    printf '%s\n' "$existing"; return 0
  fi
  git_retry git fetch -q origin "$branch" 2>/dev/null || true
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
  #
  # Rewritten when it is ours: worktrees made before per-worktree targets point
  # at the shared dir.
  local cfg="$wt/.cargo/config.toml" marker='# written by bin/common.sh'
  if [[ ! -e $cfg ]] || head -1 "$cfg" | grep -q "^$marker"; then
    mkdir -p "$wt/.cargo"
    printf '%s — not tracked, see /.gitignore\n[build]\ntarget-dir = "%s"\n' \
      "$marker" "$(prepare_target "$wt")" > "$cfg"
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

# newest_record <pr> <role> — body of the newest PR comment or review whose
# first line is `<!-- agent: <role> -->`. `gh pr view --json comments` omits
# review bodies, so both are merged. Empty when the role never posted.
newest_record() {
  gh_retry gh pr view "$1" -R "$AC_REPO" --json comments,reviews --jq "
    ([.comments[] | {at: .createdAt, body: .body}]
     + [.reviews[] | {at: .submittedAt, body: .body}])
    | map(select(.body | startswith(\"<!-- agent: $2 -->\")))
    | sort_by(.at) | last | .body // empty"
}

# The architect's file manifest for an issue: repo-relative paths, one per line.
# Empty output means no manifest — the caller decides whether that is fatal.
# The newest architect comment that carries a **file manifest** field. Not
# simply the newest architect comment: a design revised in place is followed
# by a short label note with no manifest (#466, 2026-09-17), and reading that
# note stopped master.sh with "still no file manifest" beside a full one.
ARCH_MANIFEST_JQ='[.comments[] | select((.body | test("<!-- agent: architect -->")) and (.body | test("(^|\n)\\*\\*file manifest\\*\\*")))] | last | .body // ""'

#
# stdout is the whole manifest or nothing (#548). Exit 0 with empty output
# means no manifest field, or one declared `none`. Any line that is neither a
# path nor that declaration refuses the manifest: non-zero, the issue and the
# lines on stderr. The old section parser kept only lines containing a `/`, so
# a root-level README.md vanished without a word, three times on #537/#544.
#
# Candidate lines come from a ```files fence if the comment has one (its end
# marker is unambiguous), else from the `**file manifest**` section up to the
# next bold field. Both are classified by the same rule:
#   blank                  skipped
#   path                   repo-relative, [A-Za-z0-9_.-] components joined
#                          by `/`, no `.` or `..` component; emitted once each
#   none (first line only) same regex as architect_declared_no_change in
#                          master.sh — the two are coupled; the rest of the
#                          field is explanation and is not parsed
#   anything else          refused
# A leading `- `/`* ` bullet and backticks are stripped before the path test.
# awk reads all input before deciding (END), so a closed pipe cannot cut it
# short — the same SIGPIPE concern as worktree_of_branch.
manifest_of() {
  local n="$1" body out
  body=$(gh_retry gh issue view "$n" -R "$AC_REPO" --json comments \
    --jq "$ARCH_MANIFEST_JQ") || return 1
  [[ -n $body ]] || return 0

  out=$(printf '%s\n' "$body" | LC_ALL=C awk -v n="$n" '
    { L[NR] = $0 }
    function refuse(i) {
      print "manifest on #" n ": cannot interpret line(s):" > "/dev/stderr"
      for (i = 1; i <= nbad; i++) print "  \"" bad[i] "\"" > "/dev/stderr"
      exit 3
    }
    function ispath(p,   c, k, m) {
      if (p !~ /^[A-Za-z0-9_.-]+(\/[A-Za-z0-9_.-]+)*$/) return 0
      m = split(p, c, "/")
      for (k = 1; k <= m; k++) if (c[k] == "." || c[k] == "..") return 0
      return 1
    }
    END {
      from = 0; to = -1
      for (i = 1; i <= NR; i++) if (L[i] ~ /^```files[[:space:]]*$/) { from = i + 1; break }
      if (from) {
        for (i = from; i <= NR; i++) if (L[i] ~ /^```[[:space:]]*$/) { to = i - 1; break }
        if (to < 0) { bad[++nbad] = L[from - 1] " (fence never closed)"; refuse() }
      } else {
        for (i = 1; i <= NR; i++) if (L[i] ~ /^\*\*file manifest\*\*[[:space:]]*$/) { from = i + 1; break }
        if (!from) {
          for (i = 1; i <= NR; i++) if (L[i] ~ /^\*\*file manifest\*\*/) bad[++nbad] = L[i]
          if (!nbad) bad[++nbad] = "(no **file manifest** heading on a line of its own)"
          refuse()
        }
        to = NR
        for (i = from; i <= NR; i++) if (L[i] ~ /^\*\*/) { to = i - 1; break }
      }
      first = 1; npath = 0
      for (i = from; i <= to; i++) {
        line = L[i]
        sub(/^[[:space:]]*[-*][[:space:]]+/, "", line)
        gsub(/`/, "", line)
        sub(/^[[:space:]]+/, "", line); sub(/[[:space:]]+$/, "", line)
        if (line == "") continue
        if (first) { first = 0; if (tolower(line) ~ /^[[:space:]`(]*none/) exit 0 }
        if (!ispath(line)) { bad[++nbad] = L[i]; continue }
        if (!(line in seen)) { seen[line] = 1; P[++npath] = line }
      }
      if (!npath && !nbad) bad[++nbad] = "(the field has no entries)"
      if (nbad) refuse()
      for (i = 1; i <= npath; i++) print P[i]
    }') || return 1
  [[ -z $out ]] || printf '%s\n' "$out"
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
  elif [[ $role == codex-qa ]]; then
    value=codex
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
      claude:*) value="${AC_CLAUDE_MODEL:-opus}" ;;
      codex:*) value="${AC_CODEX_MODEL:-}" ;;
    esac
  fi
  printf '%s\n' "$value"
}

# A codex run that fails before its first message (usage limit, auth) leaves
# no agent_message, so the session file used to be header-only and master.sh
# could only say the worker "exited before pushing". Fall back to the last
# error event so the reason reaches the session record.
distill_codex() {
  local raw="$1" last="$2" msg
  if [[ -s $last ]]; then cat "$last"; return; fi
  msg=$(jq -rs '[.[] | select(.type=="item.completed") | .item
           | select(.type=="agent_message") | .text] | last // empty' \
    "$raw" 2>/dev/null || true)
  if [[ -n $msg ]]; then printf '%s\n' "$msg"; return; fi
  jq -rs '[.[] | select(.type=="error") | .message] | last // empty
          | "codex error: " + .' "$raw" 2>/dev/null || true
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
      qa|codex-qa|architect|audit|rig) turns=120 ;;  # rig: preflight, probe, runs, record
      *)                  turns=80  ;;
    esac
  fi
  local provider model
  provider="$(provider_for "$role")" || return
  model="$(model_for "$role" "$provider")"
  local target
  target="$(prepare_target "$(git rev-parse --show-toplevel)")"
  mkdir -p "$AC_GATE_DIR"
  local -a codex_write_dirs=(--add-dir "$target" --add-dir "$AC_GATE_DIR")

  # Codex's workspace-write sandbox leaves git metadata read-only. A worktree's
  # git dir lives under the main repo's .git, outside -C, and --add-dir of .git
  # itself stays protected, so a codex developer could not commit. On #459 it
  # worked around that with throwaway metadata, which pushed the sparse
  # checkout's absent work/ and audit/ as deletions. The subdirectories below
  # are writable; .git's root files (config, packed-refs) are not, so
  # `git push -u` and deleting a packed branch still fail inside codex.
  local git_dir git_common
  if git_dir="$(git rev-parse --path-format=absolute --git-dir 2>/dev/null)" \
     && git_common="$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)"; then
    if [[ $git_dir != "$git_common" ]]; then
      codex_write_dirs+=(--add-dir "$git_dir")
    fi
    codex_write_dirs+=(--add-dir "$git_common/objects" --add-dir "$git_common/refs" \
                       --add-dir "$git_common/logs")
  fi
  # ssh refuses to run inside the sandbox (it rejects the ownership of
  # /etc/ssh/ssh_config.d as the sandbox presents it), so fetch and push over
  # https with the gh credential helper instead, for this process only.
  local -a codex_env=(GIT_CONFIG_COUNT=1
                      GIT_CONFIG_KEY_0=url.https://github.com/.insteadOf
                      GIT_CONFIG_VALUE_0=git@github.com:)

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
    local -a fgcmd=()
    if [[ $provider == claude ]]; then
      fgcmd=(claude --system-prompt-file "$(spec "$role")"
        --model "$model"
        --allowedTools "$tools${GH_TOOLS:+,$GH_TOOLS}"
        ${deny:+--disallowedTools "$deny"}
        --permission-mode "$mode"
        "${extra[@]}" "$task_prompt")
    else
      # Read-only roles still run tests and gh label/comment operations. Codex
      # therefore needs a writable sandbox; the binding role spec forbids
      # source edits, like the existing Claude review convention around Bash.
      local sandbox=workspace-write
      local -a model_arg=()
      [[ -n $model ]] && model_arg=(-m "$model")
      fgcmd=(env "${codex_env[@]}" codex -C "$PWD" -s "$sandbox" -a on-request
        "${codex_write_dirs[@]}"
        "${model_arg[@]}" "${extra[@]}" "$task_prompt")
    fi
    # On a terminal, run it as before. Unattended (systemd units run --fg with
    # no TTY), keep a copy of the output so an account limit is detectable:
    # on 2026-09-17 a session limit made every step fail at once and a queue
    # burned through 13 issues in two minutes.
    if [[ -t 1 ]]; then
      "${fgcmd[@]}"
      return
    fi
    local fgout fgst=0
    fgout="$(mktemp)"
    "${fgcmd[@]}" 2>&1 | tee "$fgout" || fgst=$?
    provider_limit_check "$fgout" "$provider" text && fgst=75
    rm -f "$fgout"
    return "$fgst"
  fi

  export CARGO_TARGET_DIR="$target"

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
    env "${codex_env[@]}" codex exec -C "$PWD" -s "$sandbox" \
      -c 'approval_policy="never"' \
      -c "sandbox_${sandbox//-/_}.network_access=true" \
      "${codex_write_dirs[@]}" --json -o "$last" \
      "${model_arg[@]}" "${extra[@]}" "$task_prompt" \
    | tee "$raw" \
    | jq -r --unbuffered '
        if .type=="item.completed" and .item.type=="agent_message" then .item.text
        elif .type=="item.started" and .item.type=="command_execution" then
          "  → Bash  " + ((.item.command // "") | gsub("[\\r\\n]+"; " ") | .[0:100])
        elif .type=="error" then "ERROR: " + (.message // "codex reported an error")
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
  provider_limit_check "$raw" "$provider" jsonl && status=75
  return "$status"
}
