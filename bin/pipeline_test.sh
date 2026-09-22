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
#   8. rig verdicts, including decline-site, parse whole.
#   7. nothing reads FETCH_HEAD, which concurrent fetches in the shared
#      checkout overwrite.
#  10. the architect manifest survives a newer architect note without one.
#  11. a section manifest keeps its repo-root entries (README.md, #548).
#  12. an in-place edit adding a root-level entry reaches the next dispatch.
#  13. a prose line inside the manifest section refuses the whole manifest.
#  14. a manifest heading with no entries refuses instead of reading as absent.
#  15. the files fence keeps root entries and refuses a bad line too.
#  16. a `none` manifest is still empty, by master.sh's none regex.
#  17. a bulleted none is the none declaration, not a file named `none`.
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

# 5b: a site-visit issue is not driven
M="$T/m5b"; mk_master "$M"
printf '%s\n' requires-rig site-visit > "$T/labels5b"
: > "$T/calls"
( cd "$REPO" && GH_LABELS="$T/labels5b" GH_TRIAGE_COUNT=0 bash "$M/master.sh" 7 > "$T/m5b.out" 2>&1 )
check '[[ ! -s $T/calls ]] && grep -q "site-visit" $T/m5b.out' "master.sh drives no role on a site-visit issue"

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

# --- 8: rig verdict parsing, incl. decline-site ----------------------------------
(
  cd "$REPO" && source "$BIN/common.sh"
  for t in "pass" "fail" "decline" "decline-site"; do
    printf '%s=%s\n' "$t" "$(rig_verdict_of "$(printf 'table\n\n**rig verdict:** %s\n' "$t")")"
  done
  printf 'declined=%s\n' "$(rig_verdict_of '**rig verdict:** declined')"
  printf 'old=%s\n' "$(grep -oE '\*\*rig verdict:\*\* *(pass|fail|decline)' <<<'**rig verdict:** decline-site' | tail -1 | awk '{print $NF}')"
) > "$T/r_verdict" 2>/dev/null
check 'grep -qx "pass=pass" $T/r_verdict && grep -qx "fail=fail" $T/r_verdict && grep -qx "decline=decline" $T/r_verdict' "rig_verdict_of reads pass, fail and decline"
check 'grep -qx "decline-site=decline-site" $T/r_verdict' "rig_verdict_of reads decline-site whole"
check 'grep -qx "declined=" $T/r_verdict' "rig_verdict_of rejects a word that only starts with a verdict"
check 'grep -qx "old=decline" $T/r_verdict' "control: the old grep would have read decline-site as decline"
check '! grep -q "rig verdict:\\*\\* \*(pass|fail|decline)" "$BIN/rig.sh"' "rig.sh no longer parses the verdict inline"

# --- 9: rig.sh's session prompt is one argument ---------------------------------
# A stray double quote inside the prompt splits it into several words, which
# run() would pass on as provider CLI arguments (caught while writing #530).
prompt_quotes() {  # bare double quotes between the prompt's first and last line
  sed -n '/run rig "Pipeline mode/,/ "\$@" || true$/p' "$1" | sed '1d;$d' | grep -c '"' || true
}
check '[[ $(prompt_quotes "$BIN/rig.sh") == 0 ]]' "rig.sh prompt body contains no bare double quote"

# --- 10: manifest from the newest architect comment that has one ---------------
# #466, 2026-09-17: the design comment was revised in place, then a label note
# followed. Reading the newest architect comment found no manifest and stopped.
mkdir -p "$T/stub10"
cat > "$T/stub10/gh" <<'EOF'
#!/usr/bin/env bash
while (($#)); do [[ $1 == --jq ]] && { jq -r "$2" < "$GH_COMMENTS"; exit; }; shift; done
EOF
chmod +x "$T/stub10/gh"
jq -n '{comments: [
  {body: "<!-- agent: architect -->\n### design decision\n**file manifest**\n```files\nac-rs/old.rs\n```\n"},
  {body: "<!-- agent: architect -->\n### design decision\n**file manifest**\n```files\nac-rs/a.rs\nac-rs/b.rs\n```\n"},
  {body: "<!-- agent: ux -->\n**file manifest**\n```files\nac-rs/ux.rs\n```\n"},
  {body: "<!-- agent: architect -->\n**Labels:** added `needs-ux`. The file manifest above stands.\n"}
]}' > "$T/comments10"
(
  cd "$REPO" && source "$BIN/common.sh"
  export PATH="$T/stub10:$PATH" GH_COMMENTS="$T/comments10"
  manifest_of 10 | tr '\n' ' ' > "$T/r_10"
  # The rejected selection: the newest architect comment of any kind.
  jq -r '[.comments[] | select(.body | test("<!-- agent: architect -->"))] | last | .body // ""' \
    < "$T/comments10" | grep -c '^ac-rs/' > "$T/r_10_old"
) 2>/dev/null
check '[[ $(cat $T/r_10) == "ac-rs/a.rs ac-rs/b.rs " ]]' "manifest_of reads the newest architect manifest past a later label note"
check '[[ $(cat $T/r_10_old) == 0 ]]' "the newest-architect-comment selection finds no manifest on the same thread"
check 'sed -n "/^architect_declared_no_change()/,/^}/p" "$BIN/master.sh" | grep -q ARCH_MANIFEST_JQ' "architect_declared_no_change uses the same selection"

# --- 11-15: every manifest line is a path, the none declaration, or refused ----
# #548, 2026-09-21: the section parser kept only lines containing a `/`, so
# README.md was dropped from #544's dispatch three times, silently.
m_run() {  # $1 = case; comments JSON in $T/comments$1 → r_$1 (stdout), rc_$1, err_$1
  (
    cd "$REPO" && source "$BIN/common.sh"
    export PATH="$T/stub10:$PATH" GH_COMMENTS="$T/comments$1"
    rc=0; manifest_of "$1" > "$T/r_$1" 2> "$T/err_$1" || rc=$?; echo "$rc" > "$T/rc_$1"
  )
}
m_body() {  # $1 = case, $2... = architect comment lines
  local c="$1"; shift
  printf '%s\n' '<!-- agent: architect -->' '### design decision' "$@" \
    | jq -Rs '{comments: [{body: .}]}' > "$T/comments$c"
}
m_set() { LC_ALL=C sort "$1" | tr '\n' ' '; }

# 11: root-level and nested entries, section form
m_body 11 '**file manifest**' 'README.md' 'ac-rs/a.rs' '- `ARCHITECTURE.md`' 'ac-rs/a.rs' '' '**interface changes**' 'none'
m_run 11
check '[[ $(cat $T/rc_11) == 0 && $(m_set $T/r_11) == "ARCHITECTURE.md README.md ac-rs/a.rs " && $(wc -l < $T/r_11) == 3 ]]' "a section manifest keeps its root-level entries, once each"

# 12: v1 then v2 of the same comment, v2 adds a root-level entry. manifest_of
# caches nothing, so on main this failed only through the root-level drop;
# the added entry is root-level so the case can still go red.
m_body 12 '**file manifest**' 'ac-rs/a.rs' '' '**risks**'
m_run 12; cp "$T/r_12" "$T/r_12_v1"
m_body 12 '**file manifest**' 'ac-rs/a.rs' 'TESTING.md' '' '**risks**'
m_run 12
check '[[ $(m_set $T/r_12_v1) == "ac-rs/a.rs " && $(m_set $T/r_12) == "TESTING.md ac-rs/a.rs " ]]' "an in-place edit adding a root-level entry reaches the next read"

# 13: prose between entries
m_body 13 '**file manifest**' 'ac-rs/a.rs' 'ac-rs/x.rs — changes Y' 'CLAUDE.md' '' '**risks**'
m_run 13
check '[[ $(cat $T/rc_13) != 0 && ! -s $T/r_13 ]] && grep -q "#13" $T/err_13 && grep -qF "ac-rs/x.rs — changes Y" $T/err_13' "a prose line in the manifest refuses it, naming the issue and the line"

# 14: heading present, nothing under it
m_body 14 '**file manifest**' '' '**interface changes**' 'none'
m_run 14
check '[[ $(cat $T/rc_14) != 0 && ! -s $T/r_14 ]] && grep -q "#14" $T/err_14' "a manifest heading with no entries refuses"

# 15: files fence, root-level entry, then a bad line
m_body 15 '**file manifest**' '```files' 'README.md' 'ac-rs/a.rs' '```'
m_run 15; cp "$T/r_15" "$T/r_15_ok"; cp "$T/rc_15" "$T/rc_15_ok"
m_body 15 '**file manifest**' '```files' 'README.md' 'ac-rs/*.rs' '../escape.rs' '```'
m_run 15
check '[[ $(cat $T/rc_15_ok) == 0 && $(m_set $T/r_15_ok) == "README.md ac-rs/a.rs " ]]' "the files fence keeps a root-level entry"
check '[[ $(cat $T/rc_15) != 0 && ! -s $T/r_15 ]] && grep -qF "ac-rs/*.rs" $T/err_15 && grep -qF "../escape.rs" $T/err_15' "the files fence refuses a glob and a .. path"

# The none declaration still reads as empty, with the same regex master.sh uses.
m_body 16 '**file manifest**' '(none — coordination-only epic)' '' '**risks**'
m_run 16
check '[[ $(cat $T/rc_16) == 0 && ! -s $T/r_16 ]]' "a none manifest is empty with status 0"
check 'grep -qF "~ /^[[:space:]\`(]*none/" "$BIN/master.sh" && grep -qF "~ /^[[:space:]\`(]*none/" "$BIN/common.sh"' "manifest_of and architect_declared_no_change share the none regex"

# A bulleted none is the none declaration, not a path named `none`. Red at
# 268b83fb: the none regex ran on the raw line, so `- \`none\`` fell through to
# the path test and came out as a one-entry manifest.
m_body 17 '**file manifest**' '- `none`' '' '**risks**'
m_run 17
check '[[ $(cat $T/rc_17) == 0 && ! -s $T/r_17 ]]' "a bulleted none declaration is empty, not a path named none"

# --- 7: no pipeline script reads the shared FETCH_HEAD ---------------------------
check '! grep -n "FETCH_HEAD" "$BIN"/*.sh | grep -v "^$BIN/pipeline_test.sh:" | grep -v -E ":[0-9]+:[[:space:]]*#" | grep -q .' "no bin script uses the shared FETCH_HEAD outside a comment"

exit $fail
