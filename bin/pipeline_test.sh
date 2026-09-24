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
#  18. two rig passes at one head file two records, in pass order, and a
#      filing onto an existing name refuses instead of overwriting.
#  19. a name the diff removed, still described in the tree, fails the
#      names step and the gate (#554); moved names, docs/superseded/ and
#      paragraphs citing the issue do not, nor do aliased re-exports, string
#      literal keys or code lines; a citing bullet does not exempt its
#      sibling. superseded_names_of's fence.
#  20. replays of PR #553 round 1 and PR #547 round 2 (needs their commits).
#  21. the two-review gate pins each reviewer to the model its label names:
#      qa on codex and codex-qa on claude are refused before any provider CLI
#      or target seeding; both default pairings launch (#563).
#  22. approvals do not survive a design revision (#560): decision_rev; a
#      needs-design pending at startup, an in-loop handback, a revision made
#      outside the runner, a control with a matching digest, and a design pass
#      that edits nothing; a record that can never match stops the loop; the
#      runner and review.sh digest the same issue; needs-ux at startup; the
#      Codex-recheck decision check; review.sh --independent's refusal.
#  23. the epic waiter continues only on a PR whose merge commit is on main:
#      a closed issue, a closed PR, or a PR merged into another branch stops
#      it (5); an unverifiable merge keeps polling; a PR merged before the
#      waiter starts is still the one it checks (#561).
#  24. "yours to merge" needs GitHub's mergeable (#570): CONFLICTING names
#      bin/integrate.sh and sets needs-integration; UNKNOWN is read 5 times,
#      4 sleeps of 5 s, then reported undetermined; a failed read is UNKNOWN;
#      MERGEABLE keeps the old line; the epic runner never calls a conflicting
#      child ready.
#  25. rig.sh carries a measured pass forward without the rig (#579): exit 0,
#      a carried record naming the measured head, no ssh — so no lock_take;
#      a newer carried record is never the source; with no record to carry,
#      the same run reaches lock_take; a session that writes no record file
#      still leaves a filed record, so an older pass never carries over it.
#  26. an epic rerun reads a closed child or blocker as done only when a
#      merged issue-N PR's merge commit is on main (#568): with none, one
#      merged elsewhere, a truncated listing or a failed check, a closed child
#      stops the epic and a closed blocker holds its dependents; a landed one
#      skips or releases as before; a PR that only mentions the issue in prose
#      (armed: pr_for's body fallback would take it) links nothing.
#  27. advance_worktree, revise.sh's step to the PR tip (#532): a clean
#      worktree behind a merge made elsewhere fast-forwards clean (armed: the
#      old reset --mixed leaves a reverse diff on the same fixture); a dirty
#      tree holding the tip's content is kept clean at the tip (the 18876391
#      Codex recovery); a dirty tree that does not match is refused untouched;
#      the PR #569 shape (pre-merge content as edits over an ancestor) is
#      refused with nothing normalised into the index.
set -u
BIN="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$BIN/.." && pwd)"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
fail=0
check() { if eval "$1"; then echo "ok   $2"; else echo "FAIL $2"; fail=1; fi; }

# #580: an AC_LIMIT_FILE inherited from a calling runner is that runner's live
# limit file. Fingerprint it read-only, then unset it so every default resolves
# under $T/log; the check before `exit` proves the suite left it untouched.
caller_limit="${AC_LIMIT_FILE-}"
limit_print() {
  if [[ -z "$1" ]]; then echo none
  elif [[ -r "$1" ]]; then sha256sum < "$1" | cut -d' ' -f1
  elif [[ -e "$1" ]]; then echo unreadable
  else echo absent; fi
}
caller_limit_before="$(limit_print "$caller_limit")"
unset AC_LIMIT_FILE

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

# --- 18: two rig passes at one head file two records, in pass order (#549) -------
# Red on 50114ee5: rig.sh named the record <date>-rig-pr-<N>-<rev12>.md, so the
# second pass at a head wrote the first pass's path and replaced it.
(
  cd "$REPO" && source "$BIN/common.sh"
  export AC_SESSION_DIR="$T/sess18"
  printf 'pass A\n**rig verdict:** pass\n' > "$T/rec18a"
  printf 'pass B\n**rig verdict:** decline-site\n' > "$T/rec18b"
  printf 'pass C\n' > "$T/rec18c"
  file_rig_record 547 d58a04ebac41 2026-09-21T095959Z "$T/rec18a" > "$T/r_18a"; echo $? > "$T/rc_18a"
  file_rig_record 547 d58a04ebac41 2026-09-21T100000Z "$T/rec18b" > "$T/r_18b"; echo $? > "$T/rc_18b"
  rc=0; file_rig_record 547 d58a04ebac41 2026-09-21T100000Z "$T/rec18c" > "$T/r_18c" 2> "$T/err_18c" || rc=$?; echo "$rc" > "$T/rc_18c"
  rc=0; file_rig_record 547 d58a04ebac41 2026-09-21 "$T/rec18c" > /dev/null 2>&1 || rc=$?; echo "$rc" > "$T/rc_18d"
) 2>/dev/null
a18="$T/sess18/2026-09-21-rig-pr-547-d58a04ebac41-095959Z.md"
b18="$T/sess18/2026-09-21-rig-pr-547-d58a04ebac41-100000Z.md"
check '[[ $(cat $T/rc_18a) == 0 && $(cat $T/rc_18b) == 0 && $(cat $T/r_18a) == "$a18" && $(cat $T/r_18b) == "$b18" ]]' "file_rig_record prints the stamped path of each pass"
check 'cmp -s "$a18" "$T/rec18a" && cmp -s "$b18" "$T/rec18b"' "a second pass at one head leaves the first record byte-identical"
check '[[ $(ls "$T/sess18"/*-rig-pr-547-d58a04ebac41-*.md | LC_ALL=C sort | tr "\n" " ") == "$a18 $b18 " ]]' "records for one head sort in pass order by filename"
check '[[ $(cat $T/rc_18c) == 1 && ! -s $T/r_18c ]] && cmp -s "$b18" "$T/rec18b" && grep -qF "$b18" $T/err_18c' "filing onto an existing name refuses, names it and leaves it unchanged"
check 'f=$(ls "$T/sess18"/*.refused-* 2>/dev/null) && cmp -s "$f" "$T/rec18c" && grep -qF "$f" $T/err_18c' "a refused pass keeps its record beside the existing one"
check '[[ $(cat $T/rc_18d) == 2 ]]' "file_rig_record refuses a stamp without a UTC time"
# Control: the old rule, run twice at one head, keeps only the second pass.
mkdir -p "$T/old18"; o18="$T/old18/$(date +%F)-rig-pr-547-d58a04ebac41.md"
cp "$T/rec18a" "$o18"; cp "$T/rec18b" "$o18"
check '[[ $(ls "$T/old18" | wc -l) == 1 ]] && ! cmp -s "$o18" "$T/rec18a"' "control: the old name let a second pass replace the first record"
check '! grep -qE "rig-pr-\\\$pr-\\\$rev\.md" "$BIN/rig.sh" && grep -q "file_rig_record" "$BIN/rig.sh"' "rig.sh files through file_rig_record, not a bare -\$rev.md path"

# --- 19: a removed name still described in the tree (#554) ------------------------
# Red on ffdfe248: gate.sh had no names step, so (h) exited 0 on a passing
# cargo record while README.md still described a const the diff deleted.
R19="$T/repo19"
(
  export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
  git init -q "$R19" && cd "$R19" || exit 1
  mkdir -p src ac-rs docs/superseded
  echo '[workspace]' > ac-rs/Cargo.toml
  printf '%s\n' '/// The bound.' 'pub const OLD_BOUND: f64 = 1.0;' '' '/// Kept.' 'pub fn keep() {}' > src/lib.rs
  printf '%s\n' '# x' '' 'The rule caps at `OLD_BOUND` today.' > README.md
  git add -A && git commit -qm base && git update-ref refs/remotes/origin/main HEAD
  git tag base
  mk() {  # $1 = tag; the working tree as edited since base
    git add -A && git commit -qm "$1" && git tag "$1" && git checkout -q base 2>/dev/null
  }
  drop_const() { printf '%s\n' '/// Kept.' 'pub fn keep() {}' > src/lib.rs; }
  drop_const; mk a
  drop_const; printf '%s\n' '# x' > README.md; mk b
  drop_const; printf '%s\n' '/// Moved.' 'pub const OLD_BOUND: f64 = 1.0;' > src/other.rs; mk c
  drop_const; printf '%s\n' '# x' > README.md; printf '%s\n' 'Capped at `OLD_BOUND`.' > docs/superseded/old.md; mk d
  drop_const; printf '%s\n' '# x' '' 'The rule capped at `OLD_BOUND`,' 'before #77 removed it.' > README.md; mk e
  drop_const; printf '%s\n' '# x' '' '`OLD_BOUND` still caps this today; compare marker #77suffix.' > README.md; mk e2
  printf '%s\n' '/// Error is bounded by the old' '/// speaker allowance plus tape.' 'pub fn keep() {}' > src/lib.rs
  printf '%s\n' '# x' > README.md; mk f
  printf '%s\n' '# x' '' 'The cap * speaker bound holds.' > README.md; mk i
  printf '%s\n' 'pub mod m { pub const NEW_BOUND: f64 = 1.0; }' 'pub use m::NEW_BOUND as OLD_BOUND;' '/// Kept.' 'pub fn keep() {}' > src/lib.rs; mk j
  drop_const; printf '%s\n' '# x' > README.md
  printf '%s\n' '/// Reads OLD_BOUND once.' 'pub fn user(c: &C) -> f64 { c.OLD_BOUND }' > src/user.rs; mk k
  drop_const; printf '%s\n' 'pub const KEY: &str = "OLD_BOUND"; // wire key' > src/wire.rs; mk l
  drop_const
  printf '%s\n' '# x' '' '- `OLD_BOUND` was removed by #77.' '- `OLD_BOUND` caps the window today.' \
    '  and still bounds the search.' '' '| `OLD_BOUND` | gone, #77 |' '| `OLD_BOUND` | caps it |' > README.md; mk m
) > /dev/null 2>&1
sn19() {  # $1 = case, $2 = head tag, rest = env; → r_19$1, rc_19$1
  local c="$1" h="$2"; shift 2
  ( cd "$R19" && env "$@" bash "$BIN/stale_names.sh" --base base --head "$h" > "$T/r_19$c" 2>&1; echo $? > "$T/rc_19$c" )
}
printf '%s\n' 'old speaker allowance' > "$T/names19f"
sn19 a a; sn19 b b; sn19 c c; sn19 d d
sn19 e e AC_ISSUE=77; sn19 e0 e AC_ISSUE=; sn19 e2 e2 AC_ISSUE=77
sn19 f f AC_SUPERSEDED_NAMES="$T/names19f"
check '[[ $(cat $T/rc_19a) == 1 ]] && grep -q "names   FAIL" $T/r_19a && grep -qx "  README.md:3  OLD_BOUND  (symbol)" $T/r_19a' "(a) a deleted const still named in README fails, at file:line"
check '[[ $(cat $T/rc_19b) == 0 ]] && grep -q "names   PASS  *0 reported" $T/r_19b' "(b) the same deletion with the mention gone passes"
check '[[ $(cat $T/rc_19c) == 0 ]] && grep -q "0 removed symbol" $T/r_19c && ! grep -q "README.md:" $T/r_19c' "(c) a const moved to another file is not a removed name"
check '[[ $(cat $T/rc_19d) == 0 ]] && ! grep -q "superseded/" $T/r_19d' "(d) a mention under docs/superseded/ is not reported"
check '[[ $(cat $T/rc_19e) == 0 ]] && grep -qx "  README.md:3  OLD_BOUND  (symbol, cited #77)" $T/r_19e' "(e) a paragraph citing the issue is exempt and still printed as cited"
check '[[ $(cat $T/rc_19e0) == 1 ]] && grep -qx "  README.md:3  OLD_BOUND  (symbol)" $T/r_19e0' "(e) the same mention with AC_ISSUE unset is reported"
check '[[ $(cat $T/rc_19e2) == 1 ]] && grep -qx "  README.md:3  OLD_BOUND  (symbol)" $T/r_19e2' "(e) #77suffix is not a citation of #77: the mention is reported"
check 'awk -v issue=77 '"'"'{ exit !($0 ~ ("#" issue "([^0-9]|$)")) }'"'"' <<< "compare marker #77suffix."' "(e) control: the digit-only boundary of 819239fd accepts #77suffix"
check '[[ $(cat $T/rc_19f) == 1 ]] && grep -qx "  src/lib.rs:1  old speaker allowance  (declared)" $T/r_19f' "(f) a declared phrase split across two /// lines is reported at its first line"
check '! git -C "$R19" grep -q -F "old speaker allowance" f' "(f) control: a line-based grep for the phrase finds nothing"
check '[[ $(git -C "$R19" diff base a | grep -E "^[-+][^-+]" | grep OLD_BOUND | grep -vc "pub const OLD_BOUND") == 0 ]]' "(g) control: the diff itself shows OLD_BOUND only at its definition"
printf '%s\n' 'cap * speaker bound' > "$T/names19i"
sn19 i i AC_SUPERSEDED_NAMES="$T/names19i"
sn19 j j; sn19 k k; sn19 l l; sn19 m m AC_ISSUE=77
check '[[ $(cat $T/rc_19i) == 1 ]] && grep -qx "  README.md:3  cap \* speaker bound  (declared)" $T/r_19i' "(i) a declared phrase with * is still searched, not glob-expanded"
check '[[ $(cat $T/rc_19j) == 0 ]] && ! grep -q "OLD_BOUND  (symbol)" $T/r_19j' "(j) an aliased re-export keeps the name defined"
check '[[ $(cat $T/rc_19k) == 1 ]] && grep -qx "  src/user.rs:1  OLD_BOUND  (symbol)" $T/r_19k && ! grep -q "src/user.rs:2" $T/r_19k' "(k) a removed name on a code line is not reported, its /// line is"
check '[[ $(cat $T/rc_19l) == 0 ]] && grep -q "0 removed symbol" $T/r_19l' "(l) a string literal naming the symbol (a wire key) keeps it defined"
check '[[ $(cat $T/rc_19m) == 1 ]] && grep -qx "  README.md:3  OLD_BOUND  (symbol, cited #77)" $T/r_19m && grep -qx "  README.md:4  OLD_BOUND  (symbol)" $T/r_19m' "(m) a citing bullet does not exempt its sibling"
check 'grep -qx "  README.md:7  OLD_BOUND  (symbol, cited #77)" $T/r_19m && grep -qx "  README.md:8  OLD_BOUND  (symbol)" $T/r_19m' "(m) a citing table row does not exempt the next row"
# Controls: the rev-1 step (b864a313) reported the code line in (k) and let
# the citing bullet exempt its sibling in (m), which is what (k)/(m) catch.
if git -C "$REPO" cat-file -e b864a313:bin/stale_names.sh 2>/dev/null; then
  git -C "$REPO" show b864a313:bin/stale_names.sh > "$T/sn19old.sh"
  ( cd "$R19" && bash "$T/sn19old.sh" --base base --head k > "$T/r_19kold" 2>&1
    AC_ISSUE=77 bash "$T/sn19old.sh" --base base --head m > "$T/r_19mold" 2>&1 )
  check 'grep -q "src/user.rs:2" $T/r_19kold' "(k) control: the rev-1 step reports the code line"
  check 'grep -qx "  README.md:4  OLD_BOUND  (symbol, cited #77)" $T/r_19mold' "(m) control: the rev-1 step exempts the sibling bullet"
fi

# (h) the gate: a passing cargo record for the tree, a names failure → exit 1.
cat > "$T/stub/rustc" <<'EOF'
#!/usr/bin/env bash
echo "rustc 0.0.0 (stub)"
EOF
cat > "$T/stub/cargo" <<'EOF'
#!/usr/bin/env bash
echo "cargo must not run: the record is seeded" >&2; exit 99
EOF
chmod +x "$T/stub/rustc" "$T/stub/cargo"
seed19() {  # $1 = gate dir, $2 = tag → a pass=1 record for that tag's tree
  local tree key rec
  tree="$(git -C "$R19" rev-parse "$2^{tree}")"
  key="$tree-$(printf '%s' "rustc 0.0.0 (stub)" | sha256sum | cut -c1-12)"
  rec="$1/$key"; mkdir -p "$rec"
  { echo "sha=$(git -C "$R19" rev-parse "$2")"; echo "tree=$tree"; echo "runner=stub"
    for s in fmt clippy test; do echo "$s=0"; echo "${s}_s=0"; done; echo "pass=1"; } > "$rec/result"
}
gate19() {  # $1 = case, $2 = gate script dir, $3 = tag
  seed19 "$T/gate19$1" "$3"
  ( cd "$R19" && git checkout -q "$3" 2>/dev/null \
      && AC_GATE_DIR="$T/gate19$1" AC_TARGETS="$T/targets19" AC_ISSUE="" AC_SUPERSEDED_NAMES="" \
         bash "$2/gate.sh" > "$T/r_19$1" 2>&1; echo $? > "$T/rc_19$1" )
}
gate19 h "$BIN" a
gate19 hb "$(realpath --relative-to="$R19" "$BIN")" b   # relative $0, as a hand run
check '[[ $(cat $T/rc_19h) == 1 ]] && grep -q "^  fmt  *PASS" $T/r_19h && grep -q "names   FAIL" $T/r_19h && grep -qx "  README.md:3  OLD_BOUND  (symbol)" $T/r_19h' "(h) gate.sh exits 1 on a passing record when a removed name survives"
check '[[ $(cat $T/rc_19hb) == 0 ]] && grep -q "names   PASS" $T/r_19hb' "(h) gate.sh exits 0 on the same record shape once the mention is gone"
check '! grep -q "^names" $T/gate19h/*/result' "(h) the names result is not written into the cargo record"
if git -C "$REPO" cat-file -e ffdfe248:bin/gate.sh 2>/dev/null; then
  mkdir -p "$T/old19"
  git -C "$REPO" show ffdfe248:bin/gate.sh > "$T/old19/gate.sh"
  git -C "$REPO" show ffdfe248:bin/common.sh > "$T/old19/common.sh"
  gate19 hold "$T/old19" a
  check '[[ $(cat $T/rc_19hold) == 0 ]]' "(h) control: the gate at ffdfe248 passes case (a)"
fi

# (n) --help prints the whole header comment, however long it grows (#511).
# Red on a26c7fd3: the handler printed the fixed range 2,48p, which ended one
# blank line past the header, so a header grown by two lines lost its last
# line (the control below).
last19='# not re-run to see more output.'
help19() {  # $1 = case, $2 = gate script dir, $3 = flag
  ( cd "$R19" && PATH="$T/stub:$PATH" bash "$2/gate.sh" "$3" > "$T/r_19$1" 2>&1; echo $? > "$T/rc_19$1" )
}
help19 n "$BIN" --help
check '[[ $(cat $T/rc_19n) == 0 ]] && grep -q "^# Exit: " $T/r_19n && [[ $(tail -n1 $T/r_19n) == "$last19" ]] && ! grep -q "cargo must not run" $T/r_19n' "(n) gate.sh --help exits 0 and prints the header through its last line, no cargo"
mkdir -p "$T/grow19"
cp "$BIN/common.sh" "$T/grow19/common.sh"
sed 's/^# Exit: /# Grown line (#511).\n# Grown again.\n&/' "$BIN/gate.sh" > "$T/grow19/gate.sh"
help19 ng "$T/grow19" -h
check '[[ $(cat $T/rc_19ng) == 0 ]] && grep -qx "# Grown again." $T/r_19ng && grep -q "^# Exit: " $T/r_19ng && [[ $(tail -n1 $T/r_19ng) == "$last19" ]]' "(n) a header grown by two lines still prints through its last line with -h"
mkdir -p "$T/grow19old"
cp "$BIN/common.sh" "$T/grow19old/common.sh"
sed "s/-h|--help) awk .*\"\\\$0\"; exit 0 ;;/-h|--help) sed -n '2,48p' \"\$0\"; exit 0 ;;/" "$T/grow19/gate.sh" > "$T/grow19old/gate.sh"
help19 ngold "$T/grow19old" --help
check 'grep -q "sed -n .2,48p" $T/grow19old/gate.sh && grep -qx "# Grown again." $T/r_19ngold && ! grep -qxF "$last19" $T/r_19ngold' "(n) control: the fixed 2,48p handler drops the last line of the grown header"

# superseded_names_of: the fence in the newest manifest-bearing architect comment
m_names() {  # $1 = case → sn_$1 (stdout), snrc_$1
  (
    cd "$REPO" && source "$BIN/common.sh"
    export PATH="$T/stub10:$PATH" GH_COMMENTS="$T/comments$1"
    rc=0; superseded_names_of "$1" > "$T/sn_$1" 2> "$T/snerr_$1" || rc=$?; echo "$rc" > "$T/snrc_$1"
  )
}
m_body 21 '**file manifest**' '```files' 'README.md' '```' '' '**superseded names**' '```names' 'OLD_BOUND' '  A + 2ε(d) ' 'the stored τ is subtracted' '```' '' '**interface changes**' 'none'
jq '.comments += [{body: "<!-- agent: architect -->\n**Labels:** moved to scope-none.\n"}]' "$T/comments21" > "$T/c21" && mv "$T/c21" "$T/comments21"
m_names 21
m_run 21
check '[[ $(cat $T/snrc_21) == 0 && $(tr "\n" "|" < $T/sn_21) == "OLD_BOUND|A + 2ε(d)|the stored τ is subtracted|" ]]' "superseded_names_of reads the names fence past a later label note, verbatim"
check '[[ $(cat $T/rc_21) == 0 && $(m_set $T/r_21) == "README.md " ]]' "the superseded names field does not leak into the manifest"
m_body 22 '**file manifest**' '```files' 'README.md' '```' '' '**superseded names**' '```names' 'none' '```'
m_names 22
m_body 23 '**file manifest**' '```files' 'README.md' '```' '' '**superseded names**' 'none — this design removes nothing'
m_names 23
check '[[ $(cat $T/snrc_22) == 0 && ! -s $T/sn_22 && $(cat $T/snrc_23) == 0 && ! -s $T/sn_23 ]]' "a none fence and a fenceless field are both empty with status 0"
m_body 24 '**file manifest**' '```files' 'README.md' '```' '' '**superseded names**' '```names' 'OLD_BOUND'
m_names 24
check '[[ $(cat $T/snrc_24) != 0 && ! -s $T/sn_24 ]] && grep -q "#24" $T/snerr_24' "a names fence that never closes refuses, naming the issue"

# --- 20: replays of the stale-doc rounds (#554) ------------------------------------
# Declared lists written from each design comment before the first replay ran
# (#552's for PR #553 round 1, #544's for PR #547 round 2), and not edited after.
# They need the historical commits; a checkout without them skips, loudly.
NAMES_552='ARRIVAL_EXCESS_DELAY_ALLOWANCE_S
DistanceWindow::high_s
DistanceCheck::TooLate
A + 2ε
1.52 ms at 2 m
speaker allowance'
NAMES_544='stored absolute τ
stored-absolute-τ
subtracts the stored τ
a reader subtracts
Its only consumer is the onset search'"'"'s causal bound
τ is the capture pair'"'"'s stored `interface_latency`
interface_latency_unverified'
if git -C "$REPO" cat-file -e "752960a0de^{commit}" 2>/dev/null \
    && git -C "$REPO" cat-file -e "d58a04ebac^{commit}" 2>/dev/null; then
  printf '%s\n' "$NAMES_552" > "$T/names552"; printf '%s\n' "$NAMES_544" > "$T/names544"
  ( cd "$REPO" && AC_ISSUE=552 AC_SUPERSEDED_NAMES="$T/names552" \
      bash "$BIN/stale_names.sh" --base 752960a0de^ --head 752960a0de > "$T/r_20a"; echo $? > "$T/rc_20a" )
  ( cd "$REPO" && AC_ISSUE=544 AC_SUPERSEDED_NAMES="$T/names544" \
      bash "$BIN/stale_names.sh" --base d58a04ebac^ --head d58a04ebac > "$T/r_20b"; echo $? > "$T/rc_20b" )
  check '[[ $(cat $T/rc_20a) == 1 ]] && grep -qE "report/ir_stats.rs:140  A \+ 2ε  \(declared\)$" $T/r_20a' "replay PR #553 r1: ir_stats.rs:140 reported from the declared list"
  check 'grep -qE "ir_stats.rs:3393  TooLate  \(symbol, cited #552\)" $T/r_20a && grep -qE "arrival_suite.rs:576 .*cited #552" $T/r_20a' "replay PR #553 r1: the past-tense #552 mentions are cited, not reported"
  # The #544 list MISSES the passage Codex flagged (README.md:199-204, "the one
  # correction ... still applies") and reports README.md:152 instead, a
  # sentence that is correct today ("The stored absolute τ is no longer
  # subtracted") but cites no issue. Pinned as observed so a change shows; the
  # miss is reported on #554, not tuned away.
  check '[[ $(cat $T/rc_20b) == 1 && $(grep -c "(declared)$" $T/r_20b) == 1 ]] && grep -qx "  README.md:152  stored absolute τ  (declared)" $T/r_20b' "replay PR #547 r2, #544 list: one report, README.md:152 (not the flagged passage)"
  check '! grep -qE "^  README.md:(199|20[0-4]) " $T/r_20b' "replay PR #547 r2, #544 list: the flagged passage is missed (recall gap, reported on #554)"
  # Plumbing, not recall: the phrase from Codex's finding, which is split
  # across README lines 203-204, is found at its first line.
  printf '%s\n' 'the one correction `ac plot ir`'"'"'s printed flight-time figure still applies' > "$T/names547"
  ( cd "$REPO" && AC_ISSUE=544 AC_SUPERSEDED_NAMES="$T/names547" \
      bash "$BIN/stale_names.sh" --base d58a04ebac^ --head d58a04ebac > "$T/r_20c"; echo $? > "$T/rc_20c" )
  check '[[ $(cat $T/rc_20c) == 1 ]] && grep -q "^  README.md:203  the one correction .*(declared)$" $T/r_20c' "replay PR #547 r2, Codex's phrase: split across a line break, reported at README.md:203"
else
  echo "skip replay PR #553/#547: historical commits not in this clone"
fi

# --- 21: each approval label is produced by the model it names (#563) ----------
# The launch marker, not the stderr text, is what fails without the guard: a
# guard that printed and then launched anyway would still match the message.
for p in claude codex; do
  printf '#!/usr/bin/env bash\ntouch "%s/launched-%s"; exit 0\n' "$T" "$p" > "$T/stub/$p"
  chmod +x "$T/stub/$p"
done
r21() {  # r21 <case> <role> [VAR=value...] — run <role> and record rc/stderr
  local c="$1" role="$2"; shift 2
  rm -f "$T"/launched-*
  (
    cd "$REPO" && source "$BIN/common.sh"
    unset AC_PROVIDER AC_QA_PROVIDER AC_CODEX_QA_PROVIDER
    export AC_TARGETS="$T/targets21" AC_LIMIT_FILE="$T/limit21"
    (($#)) && export "$@"
    rc=0; run "$role" "task" --fg --read > /dev/null 2> "$T/err21$c" || rc=$?
    echo "$rc" > "$T/rc21$c"
  )
  ls "$T" | grep '^launched-' > "$T/launch21$c" || true
}
r21 a qa AC_QA_PROVIDER=codex
r21 b codex-qa AC_CODEX_QA_PROVIDER=claude
check '[[ $(cat $T/rc21a) == 2 ]] && grep -q "qa provider is fixed to claude" $T/err21a && [[ ! -s $T/launch21a ]]' "qa on codex is refused (2) and launches nothing"
check '[[ $(cat $T/rc21b) == 2 ]] && grep -q "codex-qa provider is fixed to codex" $T/err21b && [[ ! -s $T/launch21b ]]' "codex-qa on claude is refused (2) and launches nothing"
check 'grep -q "not an independent second review" $T/err21b' "the codex-qa refusal gives the independence reason"
check '[[ ! -e $T/targets21 ]]' "a refused reviewer seeds no target"
r21 c qa
r21 d codex-qa
check '[[ $(cat $T/rc21c) == 0 && $(cat $T/launch21c) == launched-claude ]] && ! grep -q "two-review gate" $T/err21c' "default qa launches claude with no refusal"
check '[[ $(cat $T/rc21d) == 0 && $(cat $T/launch21d) == launched-codex ]] && ! grep -q "two-review gate" $T/err21d' "default codex-qa launches codex with no refusal"
# review.sh --independent refuses before it asks GitHub for a head, so before
# any worktree or gate run.
printf '#!/usr/bin/env bash\ntouch "%s/launched-gh"; exit 1\n' "$T" > "$T/stub/gh"; chmod +x "$T/stub/gh"
rm -f "$T"/launched-*
rc=0; ( cd "$REPO" && unset AC_PROVIDER && AC_CODEX_QA_PROVIDER=claude AC_GATE_DIR="$T/gate21" \
  bash "$BIN/review.sh" --independent 7 ) > /dev/null 2> "$T/err21e" || rc=$?
check '[[ $rc == 2 && ! -e $T/launched-gh ]] && grep -q "codex-qa provider is fixed to codex" $T/err21e' "review.sh --independent with codex-qa on claude is refused before any GitHub or gate work"
rm -f "$T/stub/gh"

# --- 22: approvals do not survive a design revision (#560) ---------------------
# gh over two JSON files, $GH22/issue.json and $GH22/pr.json, applying --jq
# with the real jq: decision_rev's digest is then computed over fixtures by the
# code under test, not by a copy of it in here.
cat > "$T/stub/gh" <<'EOF'
#!/usr/bin/env bash
kind="$1" verb="${2:-}"; shift 2 || shift
jqx="" body=""; add=(); rmv=()
while (($#)); do
  case "$1" in
    --jq) jqx="$2"; shift ;;
    --add-label) add+=("$2"); shift ;;
    --remove-label) rmv+=("$2"); shift ;;
    --body) body="$2"; shift ;;
    -R|--json|--state|--limit|--label) shift ;;
  esac
  shift
done
upd() { jq "$@" "$GH22/pr.json" > "$GH22/t" && mv "$GH22/t" "$GH22/pr.json"; }
case "$kind $verb" in
  "api rate_limit") echo 5000 ;;
  "api "*) ;;
  "issue view") jq -r "${jqx:-.}" "$GH22/issue.json" ;;
  "pr view")    jq -r "${jqx:-.}" "$GH22/pr.json" ;;
  "pr list")    jq -r "${jqx:-.}" <<< '[{"number":7,"headRefName":"issue-22-x","body":"closes #22"}]' ;;
  "pr edit")
    for l in "${rmv[@]}"; do echo "remove $l" >> "$GH22/edits"; upd --arg l "$l" '.labels |= map(select(.name != $l))'; done
    for l in "${add[@]}"; do echo "add $l" >> "$GH22/edits"; upd --arg l "$l" '.labels |= (. + [{name: $l}] | unique_by(.name))'; done ;;
  "pr comment") printf '%s\n' "$body" >> "$GH22/runner" ;;
  *) echo "gh stub: unhandled: $kind $verb" >&2; exit 1 ;;
esac
EOF
chmod +x "$T/stub/gh"
export GH22="$T/g22" H22=1111111111111111111111111111111111111111
A22_OLD=$'<!-- agent: architect -->\n\n### design decision\nuse option A\n\n**file manifest**\nbin/master.sh'
A22_NEW=$'<!-- agent: architect -->\n\n### design decision\nuse option B\n\n**file manifest**\nbin/master.sh'
export A22_NEW
w22_issue() {  # $1 = issue labels (space-separated), $2 = architect comment body
  jq -n --arg ls "$1" --arg b "$2" '{body: "",
    labels: ($ls | split(" ") | map(select(. != "") | {name: .})),
    comments: [{id: "IC_triage", createdAt: "2026-09-22T10:00:00Z", body: "<!-- agent: triage -->\n\n### spec\nx"},
               {id: "IC_arch",   createdAt: "2026-09-22T11:00:00Z", body: $b},
               {id: "IC_other",  createdAt: "2026-09-22T11:30:00Z", body: "a human remark"}]}' > "$GH22/issue.json"
}
w22_pr() {  # $1 = PR labels, $2 = decision both records name
  jq -n --arg ls "$1" --arg d "$2" --arg h "$H22" '{headRefOid: $h,
    headRefName: "issue-22-x", body: "closes #22", closingIssuesReferences: [{number: 22}],
    mergeable: "MERGEABLE",
    labels: ($ls | split(" ") | map(select(. != "") | {name: .})), comments: [],
    reviews: [{submittedAt: "2026-09-22T12:00:00Z", body: "<!-- agent: qa -->\n\n## qa — PR #7 at \($h)\ndecision: \($d)\n\n### verdict\napprove"},
              {submittedAt: "2026-09-22T12:10:00Z", body: "<!-- agent: codex-qa -->\n\n## codex qa — PR #7 at \($h)\ndecision: \($d)\n\n**verdict:** pass"}]}' > "$GH22/pr.json"
}
rev22() { ( cd "$REPO" && source "$BIN/common.sh" && decision_rev 22 7 ); }
mk22() {  # $1 = dir. design/review stubs that act on the fixtures
  mk_master "$1"
  cat > "$1/design.sh" <<'EOF'
#!/usr/bin/env bash
echo "design $*" >> "$GH22/calls"
# The architect revises in place and hands back to implementation.
jq --arg b "$A22_NEW" '.comments |= map(if .id == "IC_arch" then .body = $b else . end)
  | .labels |= (map(select(.name != "needs-design")) + [{name: "ready-to-implement"}] | unique_by(.name))' \
  "$GH22/issue.json" > "$GH22/t" && mv "$GH22/t" "$GH22/issue.json"
EOF
  cat > "$1/review.sh" <<'EOF'
#!/usr/bin/env bash
echo "review $*" >> "$GH22/calls"
echo "REVIEW-RAN $*"   # an ordering marker in master.sh's own output
source "$(dirname "$0")/common.sh"; set +e
# The digest the real review.sh states: decision_of_pr. W22_REV / W22_CREV
# stand in for a Claude / Codex session that writes another line (22f, 22g).
h=$(jq -r .headRefOid "$GH22/pr.json"); rev=$(decision_of_pr 7)
crev="${W22_CREV:-$rev}"; rev="${W22_REV:-$rev}"
at="2026-09-23T00:00:$(printf '%02d' "$(wc -l < "$GH22/calls")")Z"
rec() { jq --arg at "$at" --arg b "$1" '.reviews += [{submittedAt: $at, body: $b}]' "$GH22/pr.json" > "$GH22/t" && mv "$GH22/t" "$GH22/pr.json"; }
if [[ $1 == --independent ]]; then
  rec "<!-- agent: codex-qa -->

## codex qa — PR #7 at $h
decision: $crev

**verdict:** pass"
  gh pr edit 7 --add-label codex-approved
  exit 0
fi
printf '%s\n' "$h" > "$AC_LOG_DIR/reviewed-pr-7.sha"
if [[ -f $GH22/handback && ! -f $GH22/handback.done ]]; then
  # qa decides the design is wrong: no approval, needs-design on the issue.
  touch "$GH22/handback.done"
  rec "<!-- agent: qa -->

## qa — PR #7 at $h
decision: $rev

### verdict
request-changes: design"
  gh pr edit 7 --remove-label claude-approved
  jq '.labels += [{name: "needs-design"}]' "$GH22/issue.json" > "$GH22/t" && mv "$GH22/t" "$GH22/issue.json"
  exit 0
fi
rec "<!-- agent: qa -->

## qa — PR #7 at $h
decision: $rev

### verdict
approve"
gh pr edit 7 --add-label claude-approved
EOF
  chmod +x "$1"/*.sh
}
run22() {  # $1 = case; fixtures already written to $GH22
  ( cd "$REPO" && AC_LOG_DIR="$GH22/log" bash "$T/m22$1/master.sh" 22 > "$T/m22$1.out" 2>&1 )
}
first_line() { grep -n "$1" "$2" | head -1 | cut -d: -f1; }
passed_before_review() {  # "both QA gates passed" printed before any review ran
  local p r; p=$(first_line "both QA gates passed" "$1"); r=$(first_line "REVIEW-RAN" "$1")
  [[ -n $p ]] && { [[ -z $r ]] || (( p < r )); }
}

# decision_rev itself: none without design comments, a digest that moves with
# an in-place edit, and a non-design comment that does not move it.
mkdir -p "$GH22"
w22_pr "" x
jq -n '{comments: [{id: "a", createdAt: "2026-09-22T10:00:00Z", body: "a human remark"}]}' > "$GH22/issue.json"
r22none=$(rev22)
w22_issue "" "$A22_OLD"; D0=$(rev22)
w22_issue "" "$A22_NEW"; D1=$(rev22)
jq '.comments[2].body = "an edited human remark"' "$GH22/issue.json" > "$GH22/t" && mv "$GH22/t" "$GH22/issue.json"
D1b=$(rev22)
check '[[ $r22none == none ]]' "decision_rev is 'none' with no architect or ux comment"
check '[[ $D0 =~ ^[0-9a-f]{12}$ && $D1 =~ ^[0-9a-f]{12}$ && $D0 != "$D1" ]]' "decision_rev moves when the architect edits in place"
check '[[ $D1 == "$D1b" ]]' "decision_rev ignores comments that are not architect or ux"
# The stub's read of a missing fixture fails as a non-transient gh error would.
r22fail=$( cd "$REPO" && source "$BIN/common.sh" && GH22="$T/absent" decision_rev 22 7 2>/dev/null ) && r22frc=0 || r22frc=$?
check '[[ $r22frc != 0 && $r22fail != none ]]' "decision_rev fails on an API error instead of reading as 'none'"

# record_names_decision reads the field line, not any mention of a digest. The
# Codex finding on PR #567: an old header field plus the current digest in a
# sentence passed as covering the current decision.
rnd22() { ( source "$BIN/common.sh" && record_names_decision "$1" "$2" ); }
dor22() { ( source "$BIN/common.sh" && decision_of_record "$1" ); }
hdr22='<!-- agent: qa -->

## qa — PR #7 at abc'
R22prose="$hdr22
decision: $D0

the runner requested \`decision: $D1\`."
R22two="$hdr22
decision: $D1

decision: $D0"
R22same="$hdr22
decision: $D1
decision: $D1"
R22one="$hdr22"$'\r\n'"**decision:** \`$D1\`"$'\r\n\r\n### verdict'
check '! rnd22 "$R22prose" "$D1" && [[ $(dor22 "$R22prose") == "$D0" ]]' \
  "record_names_decision: an old field with the current digest in prose is refused, and reported as the old field"
check '! rnd22 "$R22two" "$D1" && ! rnd22 "$R22two" "$D0" && [[ $(dor22 "$R22two") == "(conflicting fields: $D1 $D0)" ]]' \
  "record_names_decision: two conflicting field lines are refused for either digest"
check '! rnd22 "$R22same" "$D1"' "record_names_decision: a repeated field line is refused even when both name the digest"
check 'rnd22 "$R22one" "$D1" && ! rnd22 "$hdr22" "$D1" && [[ $(dor22 "$hdr22") == "(none recorded)" ]]' \
  "record_names_decision: one emphasised field line (CRLF) passes; no field is refused"

# 22a: startup-pending. needs-design is on the issue when the run begins; the
# PR carries both approvals at an already-reviewed head under the old design.
# Red on 17910bad: design ran, force stayed empty, the reviewed-SHA cache hit
# and "both QA gates passed" came before any review.
reset22() { rm -rf "$GH22"; mkdir -p "$GH22/log"; : > "$GH22/calls"; : > "$GH22/edits"; }
reset22; mk22 "$T/m22a"
w22_issue "needs-design agent:triage" "$A22_OLD"; w22_pr "claude-approved codex-approved" "$D0"
printf '%s\n' "$H22" > "$GH22/log/reviewed-pr-7.sha"
run22 a
check 'grep -qx "remove claude-approved" $GH22/edits && grep -qx "remove codex-approved" $GH22/edits' "22a startup-pending: both approvals removed after the architect pass"
check 'grep -qx "review 7 --full" $GH22/calls && grep -qx "review --independent 7" $GH22/calls' "22a startup-pending: full Claude QA and Codex QA both run again"
check '! passed_before_review $T/m22a.out' "22a startup-pending: 'both QA gates passed' is not reported before a review"
check 'grep -q "decision: $D1" $GH22/pr.json && grep -q "both QA gates passed" $T/m22a.out' "22a startup-pending: passes once both records name the new decision"

# 22b: in-loop handback. Claude QA sends it back mid-run; codex-approved from
# the old design must not let Codex be skipped. The forced full pass is kept.
reset22; mk22 "$T/m22b"
w22_issue "ready-to-implement" "$A22_OLD"; w22_pr "claude-approved codex-approved" "$D0"
touch "$GH22/handback"
run22 b
check '[[ $(grep -c "^review 7" $GH22/calls) == 2 ]] && [[ $(sed -n 2p $GH22/calls) == design* ]] && grep -qx "review 7 --full" $GH22/calls' "22b in-loop: handback, architect, then a forced full Claude pass"
check '[[ $(tail -1 $GH22/calls) == "review --independent 7" ]]' "22b in-loop: Codex reviews the revised design instead of being skipped"

# 22c: revised outside the runner. No label pending; the records name D0 but
# the architect comment now digests to D1.
reset22; mk22 "$T/m22c"
w22_issue "ready-to-implement" "$A22_NEW"; w22_pr "claude-approved codex-approved" "$D0"
printf '%s\n' "$H22" > "$GH22/log/reviewed-pr-7.sha"
run22 c
check 'grep -qx "remove claude-approved" $GH22/edits && grep -qx "remove codex-approved" $GH22/edits' "22c outside the runner: both approvals naming a superseded decision are removed"
check '! passed_before_review $T/m22c.out && grep -qx "review 7 --full" $GH22/calls' "22c outside the runner: reviewed again before any pass is reported"
check 'grep -q "names decision \`$D0\`" $GH22/runner && grep -q "\`$D1\`" $GH22/runner' "22c outside the runner: the runner comment names both digests"

# 22d: control for 22c — the records name the current decision, so the cache
# is trusted. This is what shows 22c goes red for the digest, not by accident.
reset22; mk22 "$T/m22d"
w22_issue "ready-to-implement" "$A22_NEW"; w22_pr "claude-approved codex-approved" "$D1"
printf '%s\n' "$H22" > "$GH22/log/reviewed-pr-7.sha"
run22 d
check 'grep -q "both QA gates passed" $T/m22d.out && [[ ! -s $GH22/calls && ! -s $GH22/edits ]]' "22d control: matching decision passes from the cache, no review, nothing removed"
# 22e: startup-pending, and the architect pass leaves its comment unchanged.
# The digest cannot see that pass, so only R1 (clear on any design pass) can:
# 22a alone would also go green through the digest comparison.
reset22; mk22 "$T/m22e"
w22_issue "needs-design" "$A22_OLD"; w22_pr "claude-approved codex-approved" "$D0"
printf '%s\n' "$H22" > "$GH22/log/reviewed-pr-7.sha"
A22_NEW="$A22_OLD" run22 e
check 'grep -qx "remove claude-approved" $GH22/edits && grep -qx "remove codex-approved" $GH22/edits && ! passed_before_review $T/m22e.out' "22e startup-pending, unedited design: approvals still cleared before any pass"
# 22f: the Claude QA session records a decision the runner never accepts (an
# omitted line, a mistyped digest). The design does not move, so a second
# removal under the same decision stops the loop instead of re-reviewing
# without bound (216 review calls in 60 s before the stop existed).
reset22; mk22 "$T/m22f"
w22_issue "ready-to-implement" "$A22_NEW"; w22_pr "" x
( cd "$REPO" && AC_LOG_DIR="$GH22/log" W22_REV=none timeout 30 bash "$T/m22f/master.sh" 22 > "$T/m22f.out" 2>&1 ); r22f=$?
check '[[ $r22f != 124 ]] && (( $(grep -c "^review 7" $GH22/calls) <= 2 )) && ! grep -q "both QA gates passed" $T/m22f.out' \
  "22f a Claude record that can never name the current decision stops the loop instead of re-reviewing without bound"
check 'grep -q "removed again under the same decision $D1: claude-approved" $T/m22f.out && ! jq -r ".labels[].name" $GH22/pr.json | grep -qx claude-approved' \
  "22f the stop names the label and the decision, and leaves the stale approval removed"

# 22g: the same for the Codex record: codex_gate must not re-run without bound.
reset22; mk22 "$T/m22g"
w22_issue "ready-to-implement" "$A22_NEW"; w22_pr "" x
( cd "$REPO" && AC_LOG_DIR="$GH22/log" W22_CREV=none timeout 30 bash "$T/m22g/master.sh" 22 > "$T/m22g.out" 2>&1 ); r22g=$?
check '[[ $r22g != 124 ]] && (( $(grep -c "^review --independent 7" $GH22/calls) <= 2 )) && ! grep -q "both QA gates passed" $T/m22g.out && grep -q "removed again under the same decision $D1: codex-approved" $T/m22g.out' \
  "22g a Codex record that can never name the current decision stops the loop too"

# 22l: the Claude QA session writes the old decision as its field and the
# current one in prose (Codex finding on PR #567). The runner must treat the
# record as stale, never as covering D1, and never report both gates passed.
reset22; mk22 "$T/m22l"
w22_issue "ready-to-implement" "$A22_NEW"; w22_pr "" x
( cd "$REPO" && AC_LOG_DIR="$GH22/log" W22_REV="$D0

the runner requested \`decision: $D1\`." timeout 30 bash "$T/m22l/master.sh" 22 > "$T/m22l.out" 2>&1 ); r22l=$?
check '[[ $r22l != 124 ]] && ! grep -q "both QA gates passed" $T/m22l.out && grep -q "removed again under the same decision $D1: claude-approved" $T/m22l.out' \
  "22l an old decision field with the current digest in prose never reaches 'both QA gates passed'"

# decision_issue: the runner and review.sh digest the same issue whatever the
# PR's closing reference says. Branch first, then GitHub's reference, then the
# body — closingIssuesReferences is empty on a non-default base.
di22() { jq "$1" "$GH22/pr.json" > "$GH22/t" && mv "$GH22/t" "$GH22/pr.json"; ( cd "$REPO" && source "$BIN/common.sh" && decision_issue 7 ); }
reset22; w22_issue "" "$A22_NEW"; w22_pr "" x
i22a=$(di22 '.closingIssuesReferences = [{number: 9}]')
i22b=$(di22 '.headRefName = "feat-x" | .closingIssuesReferences = []')
i22c=$(di22 '.headRefName = "feat-x" | .closingIssuesReferences = [{number: 9}]')
i22d=$(di22 '.headRefName = "feat-x" | .closingIssuesReferences = [] | .body = "no reference"')
check '[[ $i22a == 22 && $i22b == 22 && $i22c == 9 && -z $i22d ]]' "decision_issue: branch, then closing reference, then body, else empty"
# 22h: the runner's side of the QA finding. A branch that is not issue-N-* and
# no closing reference (non-default base): master.sh's digest still covers #22,
# so approvals under D0 are cleared and one re-review under D1 converges. The
# review side here is the stub, which calls decision_of_pr itself; the real
# review.sh's digest source is tested after the 22j cases.
reset22; mk22 "$T/m22h"
w22_issue "ready-to-implement" "$A22_NEW"; w22_pr "claude-approved codex-approved" "$D0"
jq '.headRefName = "feat-x" | .closingIssuesReferences = []' "$GH22/pr.json" > "$GH22/t" && mv "$GH22/t" "$GH22/pr.json"
printf '%s\n' "$H22" > "$GH22/log/reviewed-pr-7.sha"
( cd "$REPO" && AC_LOG_DIR="$GH22/log" timeout 30 bash "$T/m22h/master.sh" 22 > "$T/m22h.out" 2>&1 ); r22h=$?
check '[[ $r22h != 124 ]] && grep -qx "remove claude-approved" $GH22/edits && grep -q "both QA gates passed" $T/m22h.out && [[ $(grep -c "^review 7" $GH22/calls) == 1 ]]' \
  "22h no closing reference: runner and review agree on the digest, one re-review, then passed"
# Agreement alone would also hold if both read `none`; then a later revision
# of #22 would go unseen. The new records must name #22's digest.
check '[[ $(jq -r "[.reviews[] | select(.submittedAt > \"2026-09-23\") | .body | test(\"decision: $D1\")] | all and length == 2" $GH22/pr.json) == true ]]' \
  "22h no closing reference: the re-review's records name #22's decision, not none"

# 22i: needs-ux pending at startup (acceptance 2), and a ux pass that edits
# nothing: only R1 can clear the approvals.
reset22; mk22 "$T/m22i"
cat > "$T/m22i/ux.sh" <<'UXEOF'
#!/usr/bin/env bash
echo "ux $*" >> "$GH22/calls"
jq '.labels |= (map(select(.name != "needs-ux")) + [{name: "ready-to-implement"}] | unique_by(.name))' \
  "$GH22/issue.json" > "$GH22/t" && mv "$GH22/t" "$GH22/issue.json"
UXEOF
chmod +x "$T/m22i/ux.sh"
w22_issue "needs-ux" "$A22_OLD"; w22_pr "claude-approved codex-approved" "$D0"
printf '%s\n' "$H22" > "$GH22/log/reviewed-pr-7.sha"
run22 i
check '[[ $(head -1 $GH22/calls) == "ux 22" ]] && grep -qx "remove claude-approved" $GH22/edits && grep -qx "remove codex-approved" $GH22/edits && ! passed_before_review $T/m22i.out && grep -qx "review 7 --full" $GH22/calls' \
  "22i startup-pending needs-ux: approvals cleared after the ux pass, full review before any pass"

# 22j: Codex recheck. Codex failed at base B and claude-approved is off. If the
# qa record at B names a superseded decision, nothing is carried forward: full
# Claude QA. The control (the record names the current decision) rechecks.
B22=2222222222222222222222222222222222222222
w22_recheck() {  # $1 = decision the records at B name
  jq --arg b "$B22" --arg d "$1" '.reviews = [
    {submittedAt: "2026-09-22T12:00:00Z", body: "<!-- agent: qa -->\n\n## qa — PR #7 at \($b)\ndecision: \($d)\n\n### verdict\napprove"},
    {submittedAt: "2026-09-22T12:10:00Z", body: "<!-- agent: codex-qa -->\n\n## codex qa — PR #7 at \($b)\ndecision: \($d)\n\n**verdict:** fail"}]' \
    "$GH22/pr.json" > "$GH22/t" && mv "$GH22/t" "$GH22/pr.json"
  printf '%s\n' "$B22" > "$GH22/log/codex-base-pr-7.sha"
}
reset22; mk22 "$T/m22j"
w22_issue "ready-to-implement" "$A22_NEW"; w22_pr "in-review" x; w22_recheck "$D0"
( cd "$REPO" && AC_LOG_DIR="$GH22/log" timeout 30 bash "$T/m22j/master.sh" 22 > "$T/m22j.out" 2>&1 )
check 'grep -q "predates decision $D1 — full Claude QA path" $T/m22j.out && ! grep -q -- "--recheck" $GH22/calls && grep -qx "review 7 --full" $GH22/calls' \
  "22j recheck: a Claude approval under a superseded decision is not carried forward"
reset22; mk22 "$T/m22k"
w22_issue "ready-to-implement" "$A22_NEW"; w22_pr "in-review" x; w22_recheck "$D1"
( cd "$REPO" && AC_LOG_DIR="$GH22/log" timeout 30 bash "$T/m22k/master.sh" 22 > "$T/m22k.out" 2>&1 )
check 'grep -q -- "--recheck $B22 7" $GH22/calls && ! grep -q "predates decision" $T/m22k.out' \
  "22j control: under the current decision the Codex recheck runs"

# review.sh --independent refuses a Claude QA record under another decision
# before it builds a worktree: the real review.sh over the case-22 fixtures.
reset22; w22_issue "" "$A22_NEW"; w22_pr "claude-approved" "$D0"
rm -f "$T"/launched-*
rc=0; ( cd "$REPO" && unset AC_PROVIDER AC_CODEX_QA_PROVIDER && AC_LOG_DIR="$GH22/log" AC_WT_BASE="$T/wt22" \
  bash "$BIN/review.sh" --independent 7 ) > /dev/null 2> "$T/err22r" || rc=$?
check '[[ $rc == 1 && ! -e $T/wt22/codex-pr-7 && ! -e $T/launched-codex ]] && grep -q "names decision $D0, not the current $D1" $T/err22r' \
  "review.sh --independent refuses a Claude QA record naming a superseded decision, before any worktree"
# The same with the current digest also mentioned in the record's prose.
reset22; w22_issue "" "$A22_NEW"; w22_pr "claude-approved" "$D0

the runner requested \`decision: $D1\`."
rm -f "$T"/launched-*
rc=0; ( cd "$REPO" && unset AC_PROVIDER AC_CODEX_QA_PROVIDER && AC_LOG_DIR="$GH22/log" AC_WT_BASE="$T/wt22" \
  bash "$BIN/review.sh" --independent 7 ) > /dev/null 2> "$T/err22p" || rc=$?
check '[[ $rc == 1 && ! -e $T/wt22/codex-pr-7 && ! -e $T/launched-codex ]] && grep -q "names decision $D0, not the current $D1" $T/err22p' \
  "review.sh --independent refuses an old decision field with the current digest in prose"
# review.sh states decision_of_pr's digest, not issue_of_pr's: with no closing
# reference (non-default base) the refusal must still compare against #22's D1.
reset22; w22_issue "" "$A22_NEW"; w22_pr "claude-approved" "$D0"
jq '.closingIssuesReferences = []' "$GH22/pr.json" > "$GH22/t" && mv "$GH22/t" "$GH22/pr.json"
rm -f "$T"/launched-*
rc=0; ( cd "$REPO" && unset AC_PROVIDER AC_CODEX_QA_PROVIDER && AC_LOG_DIR="$GH22/log" AC_WT_BASE="$T/wt22" \
  bash "$BIN/review.sh" --independent 7 ) > /dev/null 2> "$T/err22s" || rc=$?
check '[[ $rc == 1 ]] && grep -q "names decision $D0, not the current $D1" $T/err22s' \
  "review.sh digests decision_issue's issue, not the empty closing reference"
# The main-path site sits behind a git fetch, a worktree and the gate, so it is
# pinned structurally: review.sh takes every digest from decision_of_pr and
# never calls decision_rev with an issue of its own choosing.
check '! grep -qE "decision_rev \"" "$BIN/review.sh" && [[ $(grep -c "decision=\"\$(decision_of_pr " "$BIN/review.sh") == 2 ]]' \
  "review.sh computes both digests (independent and main path) through decision_of_pr"
rm -f "$T/stub/gh"; unset GH22 A22_NEW

# --- 23: the epic waiter needs the merge commit on main (#561) -----------------
# The waiter used to return 0 on a CLOSED issue, and on any mergedAt. The stub
# answers gh's --jq with real jq over fixture JSON, so any query shape reads
# the same fixture. It answers the compare API only in the main...<oid> order:
# reversed, `ahead` would mean landed, and 23c goes red. With W_PR_AFTER set,
# every PR read after the first issue read answers from that fixture instead:
# the human merged between the waiter's PR read and its issue read (23g, 23h).
mkdir -p "$T/stub23"
cat > "$T/stub23/gh" <<'EOF'
#!/usr/bin/env bash
q=""; args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do [[ ${args[i]} == --jq ]] && q="${args[i+1]}"; done
case "$*" in
  "pr view"*)
    f="$W_PR"
    [[ -n ${W_PR_AFTER:-} && -e $W_MARK ]] && f="$W_PR_AFTER" ;;
  "issue view"*) f="$W_ISSUE"; touch "$W_MARK" ;;
  "pr list"*) f="$W_LIST" ;;
  "api repos/x/y/compare/main..."*)
    [[ -z ${W_CMP_FAIL:-} ]] || { echo "HTTP 404: Not Found" >&2; exit 1; }
    f="$W_CMP" ;;
  *) echo "unexpected: gh $*" >&2; exit 1 ;;
esac
if [[ -n $q ]]; then jq -r "$q" "$f"; else cat "$f"; fi
EOF
chmod +x "$T/stub23/gh"
w_oid=0123456789abcdef0123456789abcdef01234567
w_json() { printf '%s\n' "$2" > "$T/w_$1.json"; }  # w_json <name> <json>
w_json open_pr '{"state":"OPEN","mergedAt":null,"mergeCommit":null,"baseRefName":"main","mergeable":"MERGEABLE","mergeStateStatus":"CLEAN"}'
w_json closed_pr '{"state":"CLOSED","mergedAt":null,"mergeCommit":null,"baseRefName":"main","mergeable":"UNKNOWN","mergeStateStatus":"UNKNOWN"}'
w_json stacked_pr '{"state":"MERGED","mergedAt":"2026-09-22T10:00:00Z","mergeCommit":{"oid":"'$w_oid'"},"baseRefName":"feature-x","mergeable":"UNKNOWN","mergeStateStatus":"UNKNOWN"}'
w_json main_pr '{"state":"MERGED","mergedAt":"2026-09-22T10:00:00Z","mergeCommit":{"oid":"'$w_oid'"},"baseRefName":"main","mergeable":"UNKNOWN","mergeStateStatus":"UNKNOWN"}'
w_json issue_open '{"state":"OPEN","stateReason":null}'
w_json issue_np '{"state":"CLOSED","stateReason":"NOT_PLANNED"}'
w_json issue_done '{"state":"CLOSED","stateReason":"COMPLETED"}'
w_json cmp_behind '{"status":"behind"}'
w_json cmp_diverged '{"status":"diverged"}'
# w_run <case> <pr> <issue> <compare> [VAR=value...] → out23<case>, rc23<case>.
# The stub sleep ends the call with 42, so 42 means "polled again".
w_run() {
  local c="$1" pr="$2" issue="$3" cmp="$4"; shift 4
  (
    cd "$REPO" && source "$BIN/common.sh"
    export PATH="$T/stub23:$PATH" AC_MERGE_POLL_SECONDS=1 \
      W_PR="$T/w_$pr.json" W_ISSUE="$T/w_$issue.json" W_CMP="$T/w_$cmp.json" \
      W_MARK="$T/w_mark_$c"
    rm -f "$W_MARK"
    (($#)) && export "$@"
    eval "$(sed -n '/^pr_landed()/,/^}/p;/^wait_for_merge()/,/^}/p' "$BIN/master.sh")"
    sleep() { exit 42; }
    rc=0; ( wait_for_merge 7 70 ) > "$T/out23$c" 2>&1 || rc=$?
    echo "$rc" > "$T/rc23$c"
  )
}
w_run a open_pr issue_np cmp_behind
w_run b stacked_pr issue_open cmp_diverged
w_run c main_pr issue_done cmp_behind
w_run d main_pr issue_open cmp_behind W_CMP_FAIL=1
w_run e open_pr issue_open cmp_behind
w_run f closed_pr issue_open cmp_behind
w_run g open_pr issue_done cmp_behind W_PR_AFTER="$T/w_main_pr.json"
w_run h open_pr issue_done cmp_behind W_PR_AFTER="$T/w_main_pr.json" W_CMP_FAIL=1
check '[[ $(cat $T/rc23a) == 5 ]] && grep -q "#7" $T/out23a && grep -q NOT_PLANNED $T/out23a && ! grep -qi merged $T/out23a' "a closed issue with an unmerged PR stops the waiter (5), naming the child and close reason"
check '[[ $(cat $T/rc23b) == 5 ]] && grep -q "PR #70" $T/out23b && grep -q feature-x $T/out23b && ! grep -qi merged $T/out23b' "a PR merged into another branch stops the waiter (5), naming the PR and its base"
check '[[ $(cat $T/rc23c) == 0 ]] && grep -q "landed on main (0123456); continuing epic" $T/out23c' "a PR whose merge commit is on main continues the epic"
check '[[ $(cat $T/rc23d) == 42 ]] && ! grep -qi merged $T/out23d' "a failed ancestry check keeps polling and never continues"
check '[[ $(cat $T/rc23e) == 42 ]] && grep -q "awaiting your merge" $T/out23e' "an open child with an open PR keeps waiting"
check '[[ $(cat $T/rc23f) == 5 ]] && grep -q "closed without landing" $T/out23f && ! grep -qi merged $T/out23f' "a PR closed without a merge stops the waiter (5)"
check '[[ $(cat $T/rc23g) == 0 ]] && grep -q "landed on main (0123456); continuing epic" $T/out23g && ! grep -q "no landed change" $T/out23g' "a merge landing between the PR read and the issue read continues the epic"
check '[[ $(cat $T/rc23h) == 42 ]] && ! grep -q "no landed change" $T/out23h && ! grep -qi merged $T/out23h' "a merge landing between the reads with a failed ancestry check keeps polling"

# 23i: the handoff from drive to the waiter. qa_loop approved PR #70, and the
# human merged it before drive_epic reached the waiter, so an open-only PR list
# no longer shows it. The waiter must still get #70 and find it landed. The
# same harness with the old open-only lookup at the handoff (23j) must stop
# short, or 23i cannot tell the two apart.
check '[[ $(grep -c "STATE=awaiting-merge" "$BIN/master.sh") == 1 ]] && grep -qF "STATE=awaiting-merge; STATE_PR=\"\$pr\"" "$BIN/master.sh"' \
  "master.sh sets awaiting-merge in one place, and it pins the PR qa_loop approved"
w_json empty_list '[]'
w_epic() {  # w_epic <case> <drive_epic source filter>
  local c="$1" filter="$2"
  (
    cd "$REPO" && source "$BIN/common.sh"
    export PATH="$T/stub23:$PATH" AC_MERGE_POLL_SECONDS=1 AC_WAIT_MERGE=1 \
      W_PR="$T/w_main_pr.json" W_ISSUE="$T/w_issue_open.json" W_CMP="$T/w_cmp_behind.json" \
      W_LIST="$T/w_empty_list.json" W_MARK="$T/w_mark_$c"
    eval "$(sed -n '/^pr_for()/,/^}/p;/^pr_landed()/,/^}/p;/^wait_for_merge()/,/^}/p' "$BIN/master.sh")"
    eval "$(sed -n '/^drive_epic()/,/^}/p' "$BIN/master.sh" | sed "$filter")"
    children() { echo 7; }
    blockers_of() { :; }
    limit_stop() { :; }
    drive() { STATE=awaiting-merge; STATE_PR=70; }
    sleep() { exit 42; }
    rc=0; ( drive_epic 5 ) > "$T/out23$c" 2>&1 || rc=$?
    echo "$rc" > "$T/rc23$c"
  )
}
w_epic i ''
w_epic j 's/wait_pr="\$STATE_PR"/wait_pr="$(pr_for "$c" || true)"/'
check '[[ $(cat $T/rc23i) == 0 ]] && grep -q "PR #70 landed on main (0123456); continuing epic" $T/out23i && grep -q "all children processed" $T/out23i' \
  "a PR merged before the waiter starts is still checked for landing, and the epic continues"
check 'grep -q "cannot identify the open PR for #7" $T/out23j && ! grep -q "all children processed" $T/out23j' \
  "control: an open-only lookup at the handoff loses the merged PR"

# --- 24: "yours to merge" needs GitHub's mergeable, not only labels (#570) -------
# PR #569 was reported "yours to merge" while GitHub had it CONFLICTING. The
# stub counts its pr view reads and answers --jq with real jq over a fixture;
# with M_FAIL set it fails every read with a real (non-transient) error; with
# M_UNKNOWN_READS=k the first k reads answer UNKNOWN before the fixture. The
# sleep stub records its argument instead of sleeping.
mkdir -p "$T/stub24"
cat > "$T/stub24/gh" <<'EOF'
#!/usr/bin/env bash
q=""; args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do [[ ${args[i]} == --jq ]] && q="${args[i+1]}"; done
case "$*" in
  "pr view"*)
    echo x >> "$M_CALLS"
    [[ -z ${M_FAIL:-} ]] || { echo "GraphQL: Could not resolve to a PullRequest with the number of 569." >&2; exit 1; }
    (( $(wc -l < "$M_CALLS") > ${M_UNKNOWN_READS:-0} )) || { echo UNKNOWN; exit 0; } ;;
  *) echo "unexpected: gh $*" >&2; exit 1 ;;
esac
jq -r "$q" "$M_PR"
EOF
chmod +x "$T/stub24/gh"
# g24 <case> <mergeable JSON value> [VAR=value...] → out24<case>,
# st24<case> (STATE|STATE_PR), calls24<case> (one line per read), sleeps24<case>.
g24() {
  local c="$1" m="$2"; shift 2
  printf '{"mergeable":%s}\n' "$m" > "$T/g24_$c.json"
  (
    cd "$REPO" && source "$BIN/common.sh"
    export PATH="$T/stub24:$PATH" M_PR="$T/g24_$c.json" M_CALLS="$T/calls24$c"
    : > "$M_CALLS"; : > "$T/sleeps24$c"
    (($#)) && export "$@"
    eval "$(sed -n '/^pr_mergeable()/,/^}/p;/^report_approved()/,/^}/p' "$BIN/master.sh")"
    sleep() { echo "$*" >> "$T/sleeps24$c"; }
    STATE=""; STATE_PR=""
    report_approved 7 569 > "$T/out24$c" 2>&1
    echo "$STATE|$STATE_PR" > "$T/st24$c"
  )
}
g24 a '"CONFLICTING"'
g24 b '"UNKNOWN"'
g24 c '"MERGEABLE"' M_FAIL=1
g24 d '"MERGEABLE"'
g24 u '"MERGEABLE"' M_UNKNOWN_READS=2
check '! grep -q "yours to merge" $T/out24a && ! grep -qi ready $T/out24a && grep -q "conflicts with main" $T/out24a && grep -q "bin/integrate.sh 569" $T/out24a' \
  "24a: a CONFLICTING PR is not reported mergeable, and the report names bin/integrate.sh <pr>"
check '[[ $(cat $T/st24a) == "needs-integration|569" && $(wc -l < $T/calls24a) == 1 ]]' \
  "24a: a CONFLICTING PR sets needs-integration with the PR, after one read"
# 24a-red: the check the old master.sh fails for the reason it was wrong —
# a "yours to merge" printed outside the helper, on labels alone.
outside24() {  # non-comment "yours to merge" lines outside report_approved
  awk '/^report_approved\(\)/ { f = 1 } f && /^}/ { f = 0; next }
       !f && !/^[[:space:]]*#/ && /yours to merge/' "$1"
}
check '[[ -z $(outside24 "$BIN/master.sh") ]] && sed -n "/^report_approved()/,/^}/p" "$BIN/master.sh" | grep -q "yours to merge"' \
  "24a: every \"yours to merge\" in master.sh is inside report_approved"
sed '0,/report_approved "\$n" "\$pr"/s//echo "  #$n PR #$pr: both QA gates passed — yours to merge"/' "$BIN/master.sh" > "$T/master24_old.sh"
check '[[ -n $(outside24 "$T/master24_old.sh") ]]' \
  "24a control: one label-only echo restored in qa_loop fails that check"
check '[[ $(wc -l < $T/calls24b) == 5 && $(tr "\n" " " < $T/sleeps24b) == "5 5 5 5 " ]]' \
  "24b: UNKNOWN is read 5 times with sleep 5 between reads, then given up"
check '! grep -q "yours to merge" $T/out24b && grep -q "mergeability could not be determined" $T/out24b && [[ $(cat $T/st24b) == "needs-human|" ]]' \
  "24b: UNKNOWN after the retries is not reported mergeable; the report says it could not be determined"
check '[[ $(wc -l < $T/calls24c) == 5 && $(tr "\n" " " < $T/sleeps24c) == "5 5 5 5 " ]]' \
  "24c: a failed mergeable read counts as UNKNOWN and uses up an attempt"
check '! grep -q "yours to merge" $T/out24c && grep -q "mergeability could not be determined" $T/out24c && [[ $(cat $T/st24c) == "needs-human|" ]]' \
  "24c: a failed read is never reported mergeable, even over a MERGEABLE fixture"
check '[[ $(cat $T/out24d) == "  #7 PR #569: both QA gates passed — yours to merge" && $(cat $T/st24d) == "awaiting-merge|569" ]]' \
  "24d: a MERGEABLE PR gets the unchanged \"yours to merge\" line and awaiting-merge"
check '[[ $(wc -l < $T/calls24d) == 1 && ! -s $T/sleeps24d ]]' \
  "24d: a MERGEABLE PR costs one read and no wait"
# 24u: the verdict comes from a re-read, not the first read — a loop that read
# once and then only slept would report needs-human here.
check '[[ $(wc -l < $T/calls24u) == 3 && $(tr "\n" " " < $T/sleeps24u) == "5 5 " && $(cat $T/st24u) == "awaiting-merge|569" ]] && [[ $(cat $T/out24u) == "  #7 PR #569: both QA gates passed — yours to merge" ]]' \
  "24u: UNKNOWN that resolves to MERGEABLE on the third read stops retrying and reports yours to merge"

# 24e: the epic runner never calls a conflicting child ready. Without
# AC_WAIT_MERGE it names the integrate step and stops; with it, the existing
# waiter integrates (wait_for_merge's CONFLICTING branch, unchanged).
w_json conflict_pr '{"state":"OPEN","mergedAt":null,"mergeCommit":null,"baseRefName":"main","mergeable":"CONFLICTING","mergeStateStatus":"DIRTY"}'
e24() {  # e24 <case> [VAR=value...] → out24<case>, rc24<case>, integrate24<case>
  local c="$1"; shift
  mkdir -p "$T/bin24$c"
  printf '#!/usr/bin/env bash\necho "integrate $*" >> "%s"\n' "$T/integrate24$c" > "$T/bin24$c/integrate.sh"
  chmod +x "$T/bin24$c/integrate.sh"
  (
    cd "$REPO" && source "$BIN/common.sh"
    unset AC_WAIT_MERGE KEEP_GOING
    export PATH="$T/stub23:$PATH" AC_MERGE_POLL_SECONDS=1 \
      W_PR="$T/w_conflict_pr.json" W_ISSUE="$T/w_issue_open.json" W_CMP="$T/w_cmp_behind.json" \
      W_LIST="$T/w_empty_list.json" W_MARK="$T/w_mark_24$c"
    (($#)) && export "$@"
    eval "$(sed -n '/^pr_for()/,/^}/p;/^pr_landed()/,/^}/p;/^wait_for_merge()/,/^}/p;/^drive_epic()/,/^}/p' "$BIN/master.sh")"
    BIN="$T/bin24$c"; fg=""
    children() { echo 7; }
    blockers_of() { :; }
    limit_stop() { :; }
    drive() { STATE=needs-integration; STATE_PR=70; }
    sleep() { exit 42; }
    rc=0; ( drive_epic 5 ) > "$T/out24$c" 2>&1 || rc=$?
    echo "$rc" > "$T/rc24$c"
  )
}
e24 e
e24 f AC_WAIT_MERGE=1
check '[[ $(cat $T/rc24e) == 0 ]] && grep -q "integrate.sh 70" $T/out24e && ! grep -q "ready for your merge" $T/out24e && ! grep -q "Merge it" $T/out24e && [[ ! -e $T/integrate24e ]]' \
  "24e: without AC_WAIT_MERGE a conflicting epic child names bin/integrate.sh <pr>, is not called ready, and nothing runs"
check '[[ $(cat $T/rc24f) == 0 && $(cat $T/integrate24f) == "integrate 70" ]] && ! grep -q "ready for your merge" $T/out24f' \
  "24e: with AC_WAIT_MERGE the waiter integrates a conflicting epic child"

# --- 25: rig.sh carries a measured pass forward without the rig (#579) -----------
# The rig is reached only through ssh/scp (lock_take, ship.sh, the session), so
# a stubbed ssh that logs and answers "busy" shows whether the run touched it.
# carry-forward.sh and lib.sh are the production files; build-portable.sh is a
# stub that stages identical fixture binaries (no cargo) at the PR head.
R25="$T/repo25"; O25="$T/origin25.git"
git init -q "$R25" && git -C "$R25" -c user.email=t@t -c user.name=t commit -q --allow-empty -m init
git clone -q --bare "$R25" "$O25" && git -C "$R25" remote add origin "$O25"
git -C "$R25" push -q origin HEAD:refs/pull/7/head
H25="$(git -C "$R25" rev-parse HEAD)"
A25=210a7ff15d58aaaaaaaaaaaaaaaaaaaaaaaaaaaa
X25=ffffffffffff0000000000000000000000000000
mkdir -p "$T/stub25" "$T/rig25" "$T/home/rig-hosts"
printf 'RIG_HOST=h\nRIG_USER=u\nRIG_SSH_KEY=k\n' > "$T/home/rig-hosts/pupu.access.env"
for s in ssh scp; do
  printf '#!/usr/bin/env bash\necho "%s $*" >> "%s"\necho "rig busy: stub" >&2\nexit 3\n' "$s" "$T/ssh25" > "$T/stub25/$s"
done
ln -s "$REPO/scripts/rig/carry-forward.sh" "$REPO/scripts/rig/lib.sh" "$T/rig25/"
cat > "$T/mkstage25.sh" <<EOF
# mkstage25 <dir> <rev> — a stage whose binaries are the same at every rev.
source "$REPO/scripts/rig/lib.sh"
mkstage25() {
  mkdir -p "\$1"
  for n in ac ac-daemon ir_probe transfer_probe; do echo "binary \$n" > "\$1/\$n"; done
  cp /bin/true "\$1/it_loopback_ir"
  (cd "\$1" && sha256sum ac ac-daemon ir_probe transfer_probe it_loopback_ir > SHA256SUMS)
  carry_sums "\$1" /x/target-rig > "\$1/CARRYSUMS"
  printf 'rev=%s\ndirty_files=0\ncargo_target_dir=/x/target-rig\ncarry_rule=%s\n' "\$2" "\$CARRY_RULE" > "\$1/MANIFEST.txt"
}
EOF
cat > "$T/rig25/build-portable.sh" <<EOF
#!/usr/bin/env bash
source "$T/mkstage25.sh"
r="\$(git rev-parse HEAD)"
mkstage25 "\$AC_HOME/target-rig-stage/\${r:0:12}" "\$r"
EOF
printf '#!/usr/bin/env bash\necho "ship $*" >> "%s"\nexit 1\n' "$T/ssh25" > "$T/rig25/ship.sh"
chmod +x "$T/stub25"/* "$T/rig25/build-portable.sh" "$T/rig25/ship.sh"
cat > "$T/stub25/gh" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  *headRefOid*) echo "$GH25_HEAD"; exit ;;
  *closingIssuesReferences*) echo 579; exit ;;
  "pr comment"*)
    while (($#)); do [[ $1 == --body-file ]] && f=$2; shift; done
    n=$(jq '.comments | length' "$GH25_DB")
    jq --rawfile b "$f" --arg n "$n" \
      '.comments += [{createdAt: ("2026-09-24T00:00:0" + $n + "Z"), body: $b, url: ("https://x/c/" + $n)}]' \
      "$GH25_DB" > "$GH25_DB.new" && mv "$GH25_DB.new" "$GH25_DB"
    echo "https://x/c/$n"; exit ;;
esac
while (($#)); do [[ $1 == --jq ]] && { jq -r "$2" < "$GH25_DB"; exit; }; shift; done
EOF
chmod +x "$T/stub25/gh"
r25() {  # r25 <case> — run rig.sh 7 against session dir $T/sess25<case>
  local c="$1"
  printf '{"comments":[{"createdAt":"2026-09-23T00:00:00Z","body":"<!-- agent: qa -->\\nrig verification required: x","url":"q"}],"reviews":[]}\n' > "$T/db25$c"
  (
    cd "$R25"
    export PATH="$T/stub25:$PATH" GH25_HEAD="$H25" GH25_DB="$T/db25$c" AC_RIG=pupu \
      AC_RIG_SCRIPTS="$T/rig25" AC_SESSION_DIR="$T/sess25$c" AC_WT_BASE="$T/wt25$c" \
      AC_TARGET="$T/home/target" AC_TARGETS="$T/home/target/wt" AC_GH_RETRIES=1
    rc=0; bash "$BIN/rig.sh" 7 > "$T/out25$c" 2>&1 || rc=$?
    echo "$rc" > "$T/rc25$c"
  )
}
( cd "$REPO" && source "$BIN/common.sh" && source "$T/mkstage25.sh"
  mkstage25 "$T/stageA25" "$A25"
  mkdir -p "$T/sess25a" "$T/sess25b"
  # a: a measured pass at A, then a newer carried record at X. The carried one
  # must be skipped; A is the source.
  f="$T/sess25a/2026-09-23-rig-pr-7-${A25:0:12}-100000Z.md"
  printf 'measured\n**rig verdict:** pass\n' > "$f"
  append_rig_block "$f" measured "$A25" pass "https://x/c/a" "$T/stageA25"
  g="$T/sess25a/2026-09-23-rig-pr-7-${X25:0:12}-110000Z.md"
  printf 'carried\n**rig verdict:** pass\n' > "$g"
  append_rig_block "$g" carried "$X25" pass "https://x/c/x" "$A25" "$f"
  # b: nothing filed for the PR.
) 2>/dev/null
rm -f "$T/ssh25"; r25 a; mv "$T/ssh25" "$T/ssh25a" 2>/dev/null || : > "$T/ssh25a"
r25 b; mv "$T/ssh25" "$T/ssh25b" 2>/dev/null || : > "$T/ssh25b"
posted25="$(jq -r '.comments[-1].body' "$T/db25a")"
filed25="$(ls "$T/sess25a"/*-rig-pr-7-"${H25:0:12}"-*.md 2>/dev/null)"
check '[[ $(cat $T/rc25a) == 0 && ! -s $T/ssh25a ]]' "25a: rig.sh exits 0 on a carry without any ssh, so without lock_take or ship"
check '[[ $posted25 == "<!-- agent: rig -->"* && $posted25 == *"$H25"* && $posted25 == *"measured at (A): \`$A25\`"* && $(grep -c "| match |" <<<"$posted25") == 5 && $(grep -c "^\*\*rig verdict:\*\* pass$" <<<"$posted25") == 1 ]]' \
  "25a: the carried comment names the head, the measured head A (not the newer carried X), five matches, one pass verdict"
check '[[ -n $filed25 ]] && grep -qx "kind=carried" "$filed25" && grep -qx "measured_at=$A25" "$filed25" && grep -qx "comment=https://x/c/1" "$filed25"' \
  "25a: the carried record is filed with a kind=carried block naming A and its own comment"
check '[[ $(cat $T/rc25b) == 3 ]] && grep -q "^ssh " $T/ssh25b && ! grep -q "^ship " $T/ssh25b && grep -q "no carry" $T/out25b' \
  "25b: control — with no record to carry, the same run reaches lock_take (stub ssh: busy, exit 3)"

# 25c: a measured session. The rig lock and ship succeed (stubs), the stubbed
# session writes its record and posts a pass; the runner files the record and
# appends a kind=measured block with this stage's digests, which a later head
# with the same binaries then carries from.
for s in ssh scp; do
  printf '#!/usr/bin/env bash\necho "%s $*" >> "%s"\ncat > /dev/null\nexit 0\n' "$s" "$T/ssh25" > "$T/stub25/$s"
done
printf '#!/usr/bin/env bash\necho "ship $*" >> "%s"\n' "$T/ssh25" > "$T/rig25/ship.sh"
cat > "$T/stub25/claude" <<'EOF'
#!/usr/bin/env bash
printf 'measured session\n**rig verdict:** pass\n' > rig-record.md
printf '<!-- agent: rig -->\n## rig — PR #7 at %s\n\n**rig verdict:** pass\n' "$GH25_HEAD" > "$GH25_DB.body"
gh pr comment 7 --body-file "$GH25_DB.body" >/dev/null
echo '{"type":"result","result":"done"}'
EOF
chmod +x "$T/stub25/claude"
rm -f "$T/ssh25"
(export AC_PROVIDER=claude AC_RIG_PROVIDER=claude; r25 c)
filed25c="$(ls "$T/sess25c"/*-rig-pr-7-"${H25:0:12}"-*.md 2>/dev/null)"
(
  cd "$REPO" && source "$T/mkstage25.sh"
  mkstage25 "$T/stageB25" 0b0b0b0b0b0b0000000000000000000000000000
  rc=0; AC_SESSION_DIR="$T/sess25c" "$REPO/scripts/rig/carry-forward.sh" 7 "$T/stageB25" > /dev/null 2>&1 || rc=$?
  echo "$rc" > "$T/rc25c_carry"
) 2>/dev/null
check '[[ $(cat $T/rc25c) == 0 ]] && grep -q "^ship " $T/ssh25 && [[ -n $filed25c ]] && grep -qx "kind=measured" "$filed25c" && grep -qx "head=$H25" "$filed25c" && grep -qx "verdict=pass" "$filed25c" && grep -q "^carrysum=.*  it_loopback_ir$" "$filed25c" && [[ $(tail -1 "$filed25c") == "\`\`\`" ]]' \
  "25c: a measured session's filed record ends with a kind=measured block holding the stage's digests"
check '[[ $(cat $T/rc25c_carry) == 0 ]]' "25c: carry-forward.sh reads that block back: a later head with the same binaries carries"

# 25d/25e: a session that writes no rig-record.md still leaves a filed record,
# so an older measured pass at identical binaries (A, filed earlier) is not the
# newest source afterwards. d: the session posts `fail`; the runner files that
# comment with a kind=measured, verdict=fail block. e: the session posts
# nothing; the runner files a placeholder with no block. Then A's record is
# added with an earlier stamp and rig.sh runs again at the same head: without
# the runner's filing it would carry A's pass over the session's outcome.
cat > "$T/stub25/claude" <<'EOF'
#!/usr/bin/env bash
if [[ $STUB25_POST == fail ]]; then
  printf '<!-- agent: rig -->\n## rig — PR #7 at %s\n\n**rig verdict:** fail\n' "$GH25_HEAD" > "$GH25_DB.body"
  gh pr comment 7 --body-file "$GH25_DB.body" >/dev/null
fi
echo '{"type":"result","result":"done"}'
EOF
chmod +x "$T/stub25/claude"
for c in d e; do
  post=fail; [[ $c == e ]] && post=none
  (export AC_PROVIDER=claude AC_RIG_PROVIDER=claude STUB25_POST=$post; r25 $c)
  cp "$T/rc25$c" "$T/rc25${c}1"
  ls "$T/sess25$c"/*-rig-pr-7-"${H25:0:12}"-*.md > "$T/filed25${c}1" 2>/dev/null || : > "$T/filed25${c}1"
  cp "$T/sess25a/2026-09-23-rig-pr-7-${A25:0:12}-100000Z.md" "$T/sess25$c/"
  sleep 1   # the second pass's filing stamp must differ from the first's
  (export AC_PROVIDER=claude AC_RIG_PROVIDER=claude STUB25_POST=$post; r25 $c)
done
filed25d="$(head -1 "$T/filed25d1")"
filed25e="$(head -1 "$T/filed25e1")"
check '[[ $(wc -l < $T/filed25d1) == 1 ]] && grep -qx "kind=measured" "$filed25d" && grep -qx "verdict=fail" "$filed25d" && grep -q "^\*\*rig verdict:\*\* fail$" "$filed25d" && [[ $(cat $T/rc25d1) == 0 ]]' \
  "25d: a session that posts fail with no record file gets its comment filed, with a kind=measured verdict=fail block"
check '! grep -q "Carried forward" <<<"$(jq -r ".comments[].body" "$T/db25d")" && grep -q "no carry — the newest measured record is already at $H25" "$T/out25d"' \
  "25d: a rerun at the same head does not carry the older pass at A over that fail"
check '[[ $(wc -l < $T/filed25e1) == 1 ]] && ! grep -q "rig-runner" "$filed25e" && grep -q "Placeholder filed by bin/rig.sh" "$filed25e" && [[ $(cat $T/rc25e1) == 1 ]]' \
  "25e: a session that posts nothing and writes no record file gets a placeholder with no machine block"
check '! grep -q "Carried forward" <<<"$(jq -r ".comments[].body" "$T/db25e")" && grep -q "no carry — the newest measured record .* has no runner machine block" "$T/out25e"' \
  "25e: a rerun at the same head does not carry the older pass at A past the placeholder"

# --- 26: an epic rerun needs a closed child's change on main (#568) ---------------
# Fixtures live per case in $D: issue_<n>.json, list_<n>.json (merged PRs for a
# `--search head:issue-<n>` listing; list_open.json with no --search), pr_<p>.json
# and cmp_<oid>.json. A missing compare fixture fails the call, so pr_landed
# reads `unknown`. The stub applies --limit like gh does, so a full page is a
# full page. drive() records every issue it is handed: "not driven" is read,
# not inferred. KEEP_GOING=1 throughout, so a stop is not KEEP_GOING's doing.
mkdir -p "$T/stub26"
cat > "$T/stub26/gh" <<'EOF'
#!/usr/bin/env bash
q=""; lim=30; search=""; args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
  case "${args[i]}" in
    --jq) q="${args[i+1]}" ;;
    --limit) lim="${args[i+1]}" ;;
    --search) search="${args[i+1]}" ;;
  esac
done
case "$*" in
  "issue view"*) f="$D/issue_$3.json" ;;
  "pr view"*) f="$D/pr_$3.json" ;;
  "pr list"*)
    if [[ -n $search ]]; then f="$D/list_${search#head:issue-}.json"; else f="$D/list_open.json"; fi
    [[ -e $f ]] || f="$D/empty.json" ;;
  "api repos/x/y/compare/main..."*) f="$D/cmp_${2##*...}.json" ;;
  *) echo "unexpected: gh $*" >&2; exit 1 ;;
esac
[[ -e $f ]] || { echo "HTTP 404: Not Found" >&2; exit 1; }
[[ $1 == pr && $2 == list ]] && { jq ".[:$lim]" "$f" > "$f.page"; f="$f.page"; }
if [[ -n $q ]]; then jq -r "$q" "$f"; else cat "$f"; fi
EOF
chmod +x "$T/stub26/gh"
o_main=1111111111111111111111111111111111111111
o_off=2222222222222222222222222222222222222222
o_fail=3333333333333333333333333333333333333333
# f26 <case> <name> <json>: one fixture file for one case.
f26() { mkdir -p "$T/d26$1"; printf '%s\n' "$3" > "$T/d26$1/$2.json"; }
p26() {  # p26 <case> <pr> <oid>: a PR merged into main at <oid>
  f26 "$1" "pr_$2" '{"state":"MERGED","mergedAt":"2026-09-22T10:00:00Z","mergeCommit":{"oid":"'"$3"'"},"baseRefName":"main"}'
}
s26() {  # s26 <case> <issue> <OPEN|CLOSED>...
  local c="$1"; shift
  while (($#)); do f26 "$c" "issue_$1" '{"state":"'"$2"'"}'; shift 2; done
}
base26() {  # shared fixtures: PRs 101 landed, 102 merged off main, 103 unverifiable
  local c="$1"
  f26 "$c" empty '[]'
  f26 "$c" "cmp_$o_main" '{"status":"behind"}'
  f26 "$c" "cmp_$o_off" '{"status":"diverged"}'
  p26 "$c" 101 "$o_main"; p26 "$c" 102 "$o_off"; p26 "$c" 103 "$o_fail"
  p26 "$c" 104 "$o_main"
}
# e26 <case> <children> <blocker rows "child:blocker ..."> → out26<case>,
# rc26<case>, driven26<case>
e26() {
  local c="$1"
  # Not `kids` or `$c` below: drive_epic's own locals would shadow them.
  kids26="$2" rows26="$3" driven26="$T/driven26$1"
  : > "$T/driven26$c"
  (
    cd "$REPO" && source "$BIN/common.sh"
    export PATH="$T/stub26:$PATH" D="$T/d26$c" KEEP_GOING=1
    unset AC_WAIT_MERGE
    eval "$(sed -n '/^issue_branch_jq()/,/^}/p;/^pr_for()/,/^}/p;/^pr_landed()/,/^}/p;/^issue_landed()/,/^}/p;/^drive_epic()/,/^}/p' "$BIN/master.sh")"
    children() { printf '%s\n' $kids26; }
    blockers_of() { local r; for r in $rows26; do [[ ${r%%:*} == "$2" ]] && echo "${r#*:}"; done; }
    limit_stop() { :; }
    drive() { echo "$1" >> "$driven26"; STATE=needs-human; }
    rc=0; ( drive_epic 5 ) > "$T/out26$c" 2>&1 || rc=$?
    echo "$rc" > "$T/rc26$c"
  )
}
for c in a b c d e f g h; do base26 $c; done
# a: closed child, no merged issue-11 PR at all.
s26 a 11 CLOSED 12 OPEN
e26 a "11 12" ""
# b: closed child whose only issue-11 PR merged, but not onto main.
s26 b 11 CLOSED 12 OPEN
f26 b list_11 '[{"number":102,"headRefName":"issue-11-x"}]'
e26 b "11 12" ""
# c: blockers closed without a landed PR (11: none; 15: unverifiable) hold
# their dependents; an unrelated later child still runs.
s26 c 11 CLOSED 12 OPEN 13 OPEN 14 OPEN 15 CLOSED
f26 c list_15 '[{"number":103,"headRefName":"issue-15"}]'
e26 c "12 13 14" "12:11 14:15"
# d: closed child that landed is skipped; the next child runs.
s26 d 11 CLOSED 12 OPEN
f26 d list_11 '[{"number":101,"headRefName":"issue-11"}]'
e26 d "11 12" ""
# e: closed blocker that landed releases its dependent.
s26 e 11 CLOSED 12 OPEN
f26 e list_11 '[{"number":101,"headRefName":"issue-11-y"}]'
e26 e "12" "12:11"
# f: prose trap. PR 104 is merged, on main, and says "no longer closes #11" —
# but its branch is not issue-11. The search pre-filter hands it over anyway.
s26 f 11 CLOSED 12 OPEN
f26 f list_11 '[{"number":104,"headRefName":"other-branch","body":"This no longer closes #11."}]'
f26 f list_open '[{"number":104,"headRefName":"other-branch","body":"This no longer closes #11."}]'
e26 f "11 12" ""
# g: a full page (== the limit) may have cut the landed PR: unknown, not none.
s26 g 11 CLOSED 12 OPEN
f26 g list_11 "$(jq -nc '[range(1000;1100) | {number: ., headRefName: "issue-110-\(.)"}]')"
e26 g "11 12" ""
# h: the only candidate's ancestry check fails.
s26 h 11 CLOSED 12 OPEN
f26 h list_11 '[{"number":103,"headRefName":"issue-11"}]'
e26 h "11 12" ""
# Arming for f and g, with the same stub and fixtures.
arm26() {
  (
    cd "$REPO" && source "$BIN/common.sh"
    export PATH="$T/stub26:$PATH" D="$T/d26$1"
    eval "$(sed -n '/^issue_branch_jq()/,/^}/p;/^pr_for()/,/^}/p;/^pr_landed()/,/^}/p;/^issue_landed()/,/^}/p' "$BIN/master.sh")"
    shift; "$@"
  )
}
check '[[ $(cat $T/rc26a) == 0 && ! -s $T/driven26a ]] && ! grep -q skipping $T/out26a && grep -q "no merged issue-11 PR was found — stopping epic #5" $T/out26a && grep -q "Reopen #11 or remove it from #5" $T/out26a' \
  "26a: a closed child with no merged issue-N PR stops the epic; nothing after it is driven"
check '[[ $(cat $T/rc26b) == 0 && ! -s $T/driven26b ]] && ! grep -q skipping $T/out26b && grep -q "merged PR(s) #102 have no merge commit on main" $T/out26b' \
  "26b: a closed child whose PR merged off main stops the epic"
check '[[ $(cat $T/driven26c) == 13 ]] && grep -q "#12: blocked by #11 (closed, not on main) — skipping" $T/out26c && grep -q "#14: blocked by #15 (closed, landing unknown) — skipping" $T/out26c' \
  "26c: closed blockers not on main or unverifiable hold their dependents; an unrelated child still runs"
check '[[ $(cat $T/driven26d) == 12 ]] && grep -q "#11: closed, landed on main via PR #101 (1111111) — skipping" $T/out26d' \
  "26d: a closed child that landed is skipped and the next child runs"
check '[[ $(cat $T/driven26e) == 12 ]] && ! grep -q blocked $T/out26e' \
  "26e: a closed blocker that landed releases its dependent"
check '[[ $(cat $T/rc26f) == 0 && ! -s $T/driven26f ]] && ! grep -q skipping $T/out26f && [[ $(arm26 f issue_landed 11) == none ]]' \
  "26f: a merged PR that only mentions the issue in prose does not land it"
check '[[ $(arm26 f pr_for 11) == 104 && $(arm26 f pr_landed 104) == "landed $o_main" ]]' \
  "26f control: the trap is armed — pr_for's body fallback takes PR 104, and 104 is on main"
check '[[ ! -s $T/driven26g ]] && grep -q "could not be checked — stopping rather than guessing" $T/out26g && [[ $(arm26 g issue_landed 11) == unknown ]]' \
  "26g: a merged-PR listing as long as the limit reads unknown and stops the epic"
check '[[ ! -s $T/driven26h ]] && grep -q "could not be checked" $T/out26h && ! grep -q skipping $T/out26h' \
  "26h: a failed ancestry check on the only candidate stops the epic"
check '! grep -q "closed, skipping" "$BIN/master.sh" && ! grep -q "skips closed children" "$BIN/master.sh"' \
  "26: the old closed-means-done wording is gone from master.sh"

# --- 27: advance_worktree never hands a reverse diff to the developer (#532) ----
# Branch b: C1 → P (the PR's own commit). main: C1 → M (what main gained: an
# edit and a new file). Tip = merge of main into b, made in another clone and
# pushed — the integrate.sh shape. Each case gets its own clone of the origin.
(
  export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t
  O="$T/o27.git"; git init -q --bare "$O"
  S="$T/s27"; git init -q -b b "$S" && cd "$S"
  printf 'a1\n' > a.txt; printf 'b1\n' > b.txt; git add . && git commit -qm C1
  git checkout -qb main; printf 'a-main\n' > a.txt; printf 'new\n' > new.txt
  git add . && git commit -qm M
  git checkout -q b; printf 'b-pr\n' > b.txt; printf 'c-pr\n' > c.txt
  git add . && git commit -qm P
  git remote add origin "$O" && git push -q origin b main
  git rev-parse b~1 > "$T/c1_27"; git rev-parse b > "$T/p_27"
  B="$T/b27"; git clone -q -b b "$O" "$B" && cd "$B"
  git merge -q --no-ff origin/main -m "integrate main" && git push -q origin b
  git rev-parse HEAD > "$T/tip_27"
) > /dev/null 2>&1
C1_27="$(cat "$T/c1_27")"; P27="$(cat "$T/p_27")"; TIP27="$(cat "$T/tip_27")"
mk27() {  # $1 = dir, $2 = local HEAD; origin/b is the merge tip
  git clone -q -b b "$T/o27.git" "$1" && git -C "$1" reset -q --hard "$2"
}
adv27() {  # $1 = dir, $2 = tag → rc, stderr, HEAD, porcelain under $T/*27$2
  ( cd "$1" && source "$BIN/common.sh"
    rc=0; advance_worktree b 2> "$T/err27$2" || rc=$?; echo "$rc" > "$T/rc27$2" )
  git -C "$1" rev-parse HEAD > "$T/head27$2"
  git -C "$1" status --porcelain --untracked-files=all > "$T/st27$2"
}

# 27a: clean worktree at P (the incident: 76ae71c7 behind 6e478485).
mk27 "$T/a27" "$P27" > /dev/null 2>&1
cp -a "$T/a27" "$T/a27old"
git -C "$T/a27old" reset -q --mixed origin/b
git -C "$T/a27old" status --porcelain --untracked-files=all > "$T/st27old"
adv27 "$T/a27" a
check '[[ -n $TIP27 && $TIP27 != "$P27" && $(git -C $T/a27 rev-parse origin/b) == "$TIP27" && $(git -C $T/a27 rev-list --parents -n1 $TIP27 | wc -w) == 3 ]]' \
  "27: fixture tip is a merge commit made outside the worktree under test"
check 'grep -q "a.txt" $T/st27old && grep -q "new.txt" $T/st27old' \
  "27a armed: the old reset --mixed leaves the reverse of the merge on the same fixture"
check '[[ $(cat $T/rc27a) == 0 && $(cat $T/head27a) == "$TIP27" && ! -s $T/st27a ]]' \
  "27a: a clean worktree behind a merge made elsewhere ends clean at the tip"

# 27b: HEAD at P, working tree already holds the tip (a cut-off Codex push).
mk27 "$T/b27w" "$P27" > /dev/null 2>&1
git -C "$T/b27w" archive "$TIP27" | tar -x -C "$T/b27w"
git -C "$T/b27w" status --porcelain --untracked-files=all > "$T/st27bpre"
adv27 "$T/b27w" b
check 'grep -q "a.txt" $T/st27bpre && grep -q "new.txt" $T/st27bpre && [[ $(cat $T/rc27b) == 0 && $(cat $T/head27b) == "$TIP27" && ! -s $T/st27b ]]' \
  "27b: a dirty tree matching the tip is kept and ends clean at the tip (18876391 recovery)"

# 27c: HEAD at P, an unrelated local edit.
mk27 "$T/c27" "$P27" > /dev/null 2>&1
printf 'local edit\n' >> "$T/c27/b.txt"; sum27c="$(sha256sum < "$T/c27/b.txt")"
adv27 "$T/c27" c
check '[[ $(cat $T/rc27c) != 0 && $(cat $T/head27c) == "$P27" && $(sha256sum < $T/c27/b.txt) == "$sum27c" ]] && grep -qF "$T/c27" $T/err27c && grep -q "branch b " $T/err27c && grep -q "$TIP27" $T/err27c' \
  "27c: a dirty tree that does not match is refused untouched, naming worktree, branch and tip"

# 27d: the PR #569 shape. HEAD at C1, the pre-merge PR content (P) sitting as
# uncommitted edits — what an earlier --mixed left behind.
mk27 "$T/d27" "$C1_27" > /dev/null 2>&1
git -C "$T/d27" archive "$P27" | tar -x -C "$T/d27"
cp -a "$T/d27" "$T/d27old"; git -C "$T/d27old" reset -q --mixed origin/b
adv27 "$T/d27" d
check 'git -C $T/d27old diff --quiet origin/b -- a.txt new.txt; [[ $? == 1 ]]' \
  "27d armed: the old reset --mixed turns the #569 shape into a reverse diff of the merged files"
check '[[ $(cat $T/rc27d) != 0 && $(cat $T/head27d) == "$C1_27" && $(git -C $T/d27 write-tree) == $(git -C $T/d27 rev-parse "$C1_27^{tree}") ]] && [[ $(cat $T/d27/b.txt) == b-pr && $(cat $T/d27/a.txt) == a1 && ! -e $T/d27/new.txt ]]' \
  "27d: the #569 shape is refused with HEAD, index and working tree as they were"
check 'grep -q "^advance_worktree \"\$branch\" || exit 1$" $BIN/revise.sh && [[ $(grep -n "^advance_worktree" $BIN/revise.sh | cut -d: -f1) -lt $(grep -n "^link_support" $BIN/revise.sh | cut -d: -f1) && $(grep -n "^advance_worktree" $BIN/revise.sh | cut -d: -f1) -lt $(grep -n "run developer" $BIN/revise.sh | cut -d: -f1) ]] && ! grep -q "reset --mixed" $BIN/revise.sh' \
  "27: revise.sh exits on a refusal before link_support and the developer run"

# --- 7: no pipeline script reads the shared FETCH_HEAD ---------------------------
check '! grep -n "FETCH_HEAD" "$BIN"/*.sh | grep -v "^$BIN/pipeline_test.sh:" | grep -v -E ":[0-9]+:[[:space:]]*#" | grep -q .' "no bin script uses the shared FETCH_HEAD outside a comment"

# --- #580: the caller's inherited limit file is untouched ---------------------
if [[ -z "$caller_limit" ]]; then
  check 'true' "no AC_LIMIT_FILE inherited, so none to leave untouched (vacuous, #580)"
else
  check '[[ $(limit_print "$caller_limit") == "$caller_limit_before" ]]' \
    "the inherited AC_LIMIT_FILE $caller_limit ($caller_limit_before) is unchanged by the suite (#580)"
fi

exit $fail
