#!/usr/bin/env bash
# board_test.sh — board.sh against a stub gh, without GitHub (#562).
# Run: bash bin/board_test.sh
#
#   1. an auth failure exits non-zero, names a section, and prints no board.
#   2. a network failure, once retries run out, exits non-zero.
#   3. every query succeeding with no rows exits 0 and prints every heading
#      with "(none)" — the empty-section case that used to exit 1 silently.
#   4. dispatchable leaves out each label master.sh refuses, and the #539
#      shape (ready-to-implement + site-visit); a plain issue is listed.
#   5. a failure on the last query (open PRs) still leaves stdout empty.
set -u
BIN="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$BIN/.." && pwd)"
T="$(mktemp -d)"; trap 'rm -rf "$T"' EXIT
fail=0
check() { if eval "$1"; then echo "ok   $2"; else echo "FAIL $2"; fail=1; fi; }

export AC_HOME="$T/home" AC_LOG_DIR="$T/log" AC_REPO=x/y AC_GH_RETRIES=1
mkdir -p "$T/stub" "$T/fx" "$AC_LOG_DIR"
export PATH="$T/stub:$PATH"

# The stub answers `gh issue list --label L` from $T/fx/L.json and
# `gh pr list` from $T/fx/prs.json (default `[]`), and applies the caller's
# --jq with real jq, so board.sh's own filters run. STUB_FAIL picks a failure:
#   auth — every call fails as unauthenticated (a real error, not retried)
#   net  — every call fails as network unreachable (transient)
#   prs  — only `gh pr list` fails
cat > "$T/stub/gh" <<'EOF'
#!/usr/bin/env bash
kind="$1 $2"; label=""; q="."
while (( $# )); do
  case "$1" in
    --label) label="$2"; shift ;;
    --jq)    q="$2"; shift ;;
  esac
  shift
done
case "${STUB_FAIL:-}" in
  auth) echo 'HTTP 401: Bad credentials. To re-authenticate, run: gh auth login' >&2; exit 1 ;;
  net)  echo 'error connecting to api.github.com: network is unreachable' >&2; exit 1 ;;
  prs)  [[ $kind == "pr list" ]] && { echo 'HTTP 401: Bad credentials' >&2; exit 1; } ;;
esac
if [[ $kind == "pr list" ]]; then f="$FX/prs.json"; else f="$FX/$label.json"; fi
[[ -f $f ]] || f=/dev/null
{ [[ $f == /dev/null ]] && echo '[]' || cat "$f"; } | jq -r "$q"
EOF
chmod +x "$T/stub/gh"
export FX="$T/fx"

board() {  # board <tag> — run board.sh from inside the repo, keep out/err/rc
  local rc=0
  ( cd "$REPO" && bash "$BIN/board.sh" ) >"$T/$1.out" 2>"$T/$1.err" || rc=$?
  echo "$rc" > "$T/$1.rc"
}

# --- 1 ----------------------------------------------------------------------
STUB_FAIL=auth board auth
check '[[ $(cat $T/auth.rc) != 0 ]]'          "auth failure exits non-zero"
check 'grep -q "board: dispatchable: gh query failed" $T/auth.err' \
                                               "auth failure names the section on stderr"
check '[[ ! -s $T/auth.out ]]'                 "auth failure prints no board on stdout"

# --- 2 ----------------------------------------------------------------------
STUB_FAIL=net board net
check '[[ $(cat $T/net.rc) != 0 ]]'           "network failure exits non-zero"
check 'grep -q "gh query failed" $T/net.err'   "network failure says a query failed"
check '[[ ! -s $T/net.out ]]'                  "network failure prints no board on stdout"

# --- 3 ----------------------------------------------------------------------
board empty
check '[[ $(cat $T/empty.rc) == 0 ]]'         "all-empty board exits 0"
check '[[ ! -s $T/empty.err ]]'               "all-empty board writes nothing to stderr"
for h in "dispatchable" \
         "awaiting a rig measurement — only you can clear these" \
         "blocking other work — clear these first" \
         "blocked (lift condition is in the comment that applied it)" \
         "needs human input" \
         "awaiting architect" \
         "QA sent back" \
         "open PRs"; do
  check "grep -A1 -xF '## $h' \$T/empty.out | tail -n1 | grep -qxF '(none)'" \
        "empty section printed with (none): $h"
done

# --- 4 ----------------------------------------------------------------------
excluded=(blocked needs-ux site-visit needs-design needs-discussion needs-clarification epic)
{
  echo '['
  n=100
  for l in "${excluded[@]}"; do
    printf '{"number":%d,"title":"held by %s","labels":[{"name":"ready-to-implement"},{"name":"%s"}]},\n' "$n" "$l" "$l"
    (( ++n ))
  done
  echo '{"number":539,"title":"site visit shape","labels":[{"name":"site-visit"},{"name":"ready-to-implement"}]},'
  echo '{"number":7,"title":"plain","labels":[{"name":"ready-to-implement"},{"name":"agent:developer"}]}'
  echo ']'
} > "$FX/ready-to-implement.json"
board disp
sed -n '/^## dispatchable$/,/^$/p' "$T/disp.out" > "$T/disp.sec"
check '[[ $(cat $T/disp.rc) == 0 ]]'          "dispatchable fixture board exits 0"
check 'grep -qxF -- "- #7 plain" $T/disp.sec' "a plain ready-to-implement issue is dispatchable"
check '! grep -q "#539" $T/disp.sec'           "ready-to-implement + site-visit (#539 shape) is not dispatchable"
for l in "${excluded[@]}"; do
  check "! grep -qF 'held by $l' \$T/disp.sec" "dispatchable excludes $l"
done
check '[[ $(grep -c "^- #" $T/disp.sec) == 1 ]]' "only the plain issue is dispatchable"
rm -f "$FX/ready-to-implement.json"

# --- 5 ----------------------------------------------------------------------
STUB_FAIL=prs board prs
check '[[ $(cat $T/prs.rc) != 0 ]]'           "failure on the last query exits non-zero"
check 'grep -q "board: open PRs: gh query failed" $T/prs.err' \
                                               "failure on the last query names open PRs"
check '[[ ! -s $T/prs.out ]]'                  "failure on the last query leaves stdout empty"

exit $fail
