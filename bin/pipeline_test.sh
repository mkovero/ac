#!/usr/bin/env bash
# pipeline_test.sh — the 2026-09-17 overnight failure modes, without GitHub,
# providers or cargo. Run: bash bin/pipeline_test.sh
#
#   1. provider_limit_check: a real limit is detected; a session that only
#      quotes the words is not.
#   2. run() --fg without a terminal returns 75 and records the limit.
#   3. master.sh stops the run on a limit instead of trying the next issue.
#   4. triage_evidence counts a spec, not a scope-backfill one-liner.
#   5. master.sh stops after one design preflight on a `none` manifest.
#   6. git_retry retries a network failure and not a real error.
#   7. nothing reads FETCH_HEAD, which concurrent fetches in the shared
#      checkout overwrite.
set -u
BIN="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$BIN/.." && pwd)"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
fail=0
check() { if eval "$1"; then echo "ok   $2"; else echo "FAIL $2"; fail=1; fi; }

export AC_HOME="$T/home" AC_LOG_DIR="$T/log" AC_REPO=x/y AC_MIN_FREE_GB=0
mkdir -p "$T/stub" "$AC_LOG_DIR"
export PATH="$T/stub:$PATH"

# --- 1 ----------------------------------------------------------------------
(
  cd "$REPO" && source "$BIN/common.sh"
  export AC_LIMIT_FILE="$T/limit1"
  printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"tool_result","content":"You'"'"'ve hit your session limit · resets 2:10am"}]}}' \
                '{"type":"result","result":"Reviewed the limit detector."}' > "$T/quoted.jsonl"
  printf '%s\n' '{"type":"result","result":"You'"'"'ve hit your session limit · resets 2:10am (UTC)"}' > "$T/claude.jsonl"
  printf '%s\n' '{"type":"error","message":"You'"'"'ve hit your usage limit. try again at Sep 19th"}' > "$T/codex.jsonl"
  r=0; provider_limit_check "$T/quoted.jsonl" claude jsonl && r=1; echo "$r" > "$T/r_quoted"
  r=0; provider_limit_check "$T/claude.jsonl" claude jsonl && r=1; echo "$r" > "$T/r_claude"
  r=0; provider_limit_check "$T/codex.jsonl" codex jsonl && r=1; echo "$r" > "$T/r_codex"
) 2>/dev/null
check '[[ $(cat $T/r_quoted) == 0 ]]' "a transcript that only quotes the limit text is not a limit"
check '[[ $(cat $T/r_claude) == 1 ]]' "claude session limit in the result record is detected"
check '[[ $(cat $T/r_codex) == 1 ]]'  "codex usage limit error record is detected"

# --- 1b: a finished session whose own summary quotes the phrase (QA, #528) ---
(
  cd "$REPO" && source "$BIN/common.sh"
  export AC_LIMIT_FILE="$T/limit1b"
  printf '%s\n' '{"type":"result","is_error":false,"result":"Reviewed PR: the detector matches \"You'"'"'ve hit your session limit\" in result records."}' > "$T/quoted_result.jsonl"
  r=0; provider_limit_check "$T/quoted_result.jsonl" claude jsonl && r=1; echo "$r" > "$T/r_quoted_result"
  printf '%s\n' 'Summary: run() now detects "You'"'"'ve hit your session limit" and returns 75.' > "$T/quoted.txt"
  r=0; provider_limit_check "$T/quoted.txt" claude text && r=1; echo "$r" > "$T/r_quoted_text"
  printf '%s\n' 'working' "ERROR: You've hit your usage limit. try again at Sep 19th" > "$T/codex_fg.txt"
  AC_LIMIT_FILE="$T/limit1c"; r=0; provider_limit_check "$T/codex_fg.txt" codex text && r=1; echo "$r" > "$T/r_codex_text"
) 2>/dev/null
check '[[ $(cat $T/r_quoted_result) == 0 && ! -e $T/limit1b ]]' "a final result that quotes the limit text is not a limit"
check '[[ $(cat $T/r_quoted_text) == 0 ]]' "plain output that quotes the limit text mid-line is not a limit"
check '[[ $(cat $T/r_codex_text) == 1 ]]' "codex's 'ERROR: You've hit your usage limit' line is a limit"

# --- 2 ----------------------------------------------------------------------
cat > "$T/stub/claude" <<'EOF'
#!/usr/bin/env bash
echo "working..."; echo "You've hit your session limit · resets 2:10am (UTC)"; exit 1
EOF
chmod +x "$T/stub/claude"
(
  cd "$REPO" && source "$BIN/common.sh"
  export AC_LIMIT_FILE="$T/limit2" AC_PROVIDER=claude AC_DEVELOPER_PROVIDER=claude
  rc=0; run developer "task" --fg > "$T/run2.out" 2>&1 || rc=$?
  echo "$rc" > "$T/r_run"
)
check '[[ $(cat $T/r_run) == 75 ]]' "run --fg without a terminal returns 75 on a limit"
check '[[ -s $T/limit2 ]] && grep -q "session limit" $T/limit2' "run --fg records the limit"
check 'grep -q "working..." $T/run2.out' "run --fg still shows the provider output"

# --- 2b: unattended --fg keeps a non-limit failure status ---------------------
cat > "$T/stub/claude" <<'EOF'
#!/usr/bin/env bash
echo "working..."; exit 3
EOF
(
  cd "$REPO" && source "$BIN/common.sh"
  export AC_LIMIT_FILE="$T/limit2b" AC_PROVIDER=claude AC_DEVELOPER_PROVIDER=claude
  rc=0; run developer "task" --fg > /dev/null 2>&1 || rc=$?
  echo "$rc" > "$T/r_run2b"
)
check '[[ $(cat $T/r_run2b) == 3 && ! -e $T/limit2b ]]' "run --fg without a terminal keeps a plain failure status (3), no limit"

# --- 2c: remove_worktree takes the target with it -------------------------------
(
  R="$T/repo2c"; git init -q "$R" && cd "$R" && git -c user.email=t@t -c user.name=t commit -q --allow-empty -m init
  source "$BIN/common.sh"
  export AC_TARGETS="$T/targets2c"
  git worktree add -q --detach "$T/wt2c/gone" HEAD
  git worktree add -q --detach "$T/wt2c/kept" HEAD
  mkdir -p "$AC_TARGETS/gone/debug" "$AC_TARGETS/kept/debug"
  echo dirty > "$T/wt2c/kept/untracked-but-locked"; git worktree lock "$T/wt2c/kept"
  remove_worktree "$T/wt2c/gone"
  remove_worktree "$T/wt2c/kept"   # locked: removal fails, target must stay
  { [[ -d $T/wt2c/gone ]] && echo wt-left || echo wt-gone; [[ -d $AC_TARGETS/gone ]] && echo t-left || echo t-gone
    [[ -d $T/wt2c/kept ]] && echo kwt-left || echo kwt-gone; [[ -d $AC_TARGETS/kept ]] && echo kt-left || echo kt-gone; } > "$T/r_2c"
  git worktree unlock "$T/wt2c/kept"
) 2>/dev/null
check '[[ $(tr "\n" " " < $T/r_2c) == "wt-gone t-gone kwt-left kt-left " ]]' "remove_worktree removes the target only when the worktree went"

# --- 3, 5: master.sh with stubbed gh and role scripts ------------------------
mk_master() {  # $1 = dir; copies master/common and stubs the role scripts
  mkdir -p "$1"; cp "$BIN/master.sh" "$BIN/common.sh" "$1/"
  for s in triage design ux implement review revise integrate rig; do
    printf '#!/usr/bin/env bash\necho "%s $*" >> "%s"\n' "$s" "$T/calls" > "$1/$s.sh"
  done
  chmod +x "$1"/*.sh
}
cat > "$T/stub/gh" <<'EOF'
#!/usr/bin/env bash
a="$*"
case "$a" in
  "auth status"*|"api user"*|"api rate_limit"*) echo ok ;;
  *"--json labels"*) cat "$GH_LABELS" 2>/dev/null ;;
  *"--json state"*) echo OPEN ;;
  *"--json comments"*"length"*) echo "${GH_TRIAGE_COUNT:-0}" ;;
  *"--json comments"*) cat "$GH_ARCH" 2>/dev/null ;;
  *sub_issues*) echo "" ;;
  *"--json body"*) echo "" ;;
  "pr list"*) echo "" ;;
  *) echo "" ;;
esac
EOF
chmod +x "$T/stub/gh"

# 3: triage hits the limit on #1; #2 must not be attempted
M="$T/m3"; mk_master "$M"
cat > "$M/triage.sh" <<EOF
#!/usr/bin/env bash
echo "triage \$*" >> "$T/calls"
echo "x You've hit your session limit · resets 2:10am (UTC)" > "\$AC_LIMIT_FILE"
exit 75
EOF
chmod +x "$M/triage.sh"
: > "$T/calls"; : > "$T/labels"
( cd "$REPO" && GH_LABELS="$T/labels" bash "$M/master.sh" 1 2 > "$T/m3.out" 2>&1; echo $? > "$T/r_m3" )
check '[[ $(cat $T/r_m3) == 75 ]]' "master.sh exits 75 on a provider limit"
check '[[ $(grep -c "^triage" $T/calls) == 1 ]]' "master.sh does not try the next issue after a limit"

# 5: ready-to-implement, no manifest, architect says none
M="$T/m5"; mk_master "$M"
printf '%s\n' ready-to-implement > "$T/labels5"
printf '%s\n' '<!-- agent: architect -->' '**file manifest**' 'none. The empty list is deliberate.' '' '**interface changes**' 'none' > "$T/arch5"
: > "$T/calls"
( cd "$REPO" && GH_LABELS="$T/labels5" GH_ARCH="$T/arch5" GH_TRIAGE_COUNT=1 bash "$M/master.sh" 5 > "$T/m5.out" 2>&1 )
check '[[ $(grep -c "^design" $T/calls) == 1 ]]' "one design preflight on a missing manifest, not a loop"
check 'grep -q "manifest is .none." $T/m5.out' "the stop names the architect's 'none' manifest"
check '! grep -q "step limit" $T/m5.out' "the step limit is not what stopped it"

# --- 4: triage evidence filter ------------------------------------------------
# shellcheck disable=SC2034  # used inside the eval'd checks below
filter="$(sed -n '/^triage_evidence()/,/^}/p' "$BIN/master.sh" | grep -o "\[\.comments.*length")"
printf '%s' '{"comments":[{"body":"<!-- agent: triage -->\nScope label set: `tier-1`."},{"body":"unrelated"}]}' > "$T/c_backfill.json"
printf '%s' '{"comments":[{"body":"<!-- agent: triage -->\n\n### spec\n**problem statement**"}]}' > "$T/c_spec.json"
check '[[ $(jq "$filter" $T/c_backfill.json) == 0 ]]' "a scope-backfill triage comment is not a spec"
check '[[ $(jq "$filter" $T/c_spec.json) == 1 ]]' "a triage spec comment counts"

# --- 6: git_retry --------------------------------------------------------------
cat > "$T/stub/netgit" <<'EOF'
#!/usr/bin/env bash
n=$(cat "$NETGIT_COUNT" 2>/dev/null || echo 0); echo $((n+1)) > "$NETGIT_COUNT"
if (( n < ${NETGIT_FAILS:-0} )); then echo "ssh: Could not resolve hostname github.com: No address associated with hostname" >&2; exit 128; fi
if [[ ${NETGIT_REAL:-} ]]; then echo "fatal: couldn't find remote ref pull/9/head" >&2; exit 128; fi
exit 0
EOF
chmod +x "$T/stub/netgit"
(
  cd "$REPO" && source "$BIN/common.sh"
  sleep() { :; }
  export NETGIT_COUNT="$T/ng"
  rm -f "$T/ng"; rc=0; NETGIT_FAILS=2 git_retry netgit fetch 2>/dev/null || rc=$?; echo "$rc $(cat $T/ng)" > "$T/r_g1"
  rm -f "$T/ng"; rc=0; NETGIT_REAL=1 git_retry netgit fetch 2>/dev/null || rc=$?; echo "$rc $(cat $T/ng)" > "$T/r_g2"
)
check '[[ $(cat $T/r_g1) == "0 3" ]]' "git_retry retries a DNS failure until it clears"
check '[[ $(cat $T/r_g2) == "128 1" ]]' "git_retry returns a real error at once"

# --- 7: no pipeline script reads the shared FETCH_HEAD ---------------------------
check '! grep -n "FETCH_HEAD" "$BIN"/*.sh | grep -v "^$BIN/pipeline_test.sh:" | grep -v -E ":[0-9]+:[[:space:]]*#" | grep -q .' "no bin script uses the shared FETCH_HEAD outside a comment"

exit $fail
