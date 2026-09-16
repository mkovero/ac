#!/usr/bin/env bash
# gh_retry_test.sh — gh_retry must fail on errors and retry network outages.
# Run: bash bin/gh_retry_test.sh. Fails on the pre-fix gh_retry (every error
# returned 0 with empty output).
set -u
eval "$(sed -n '/^gh_retry()/,/^}/p' "$(dirname "$0")/common.sh")"
fail=0
check() { if eval "$1"; then echo "ok   $2"; else echo "FAIL $2"; fail=1; fi; }
T="$(mktemp -d)"; export T
cat > "$T/real" <<'X'
#!/usr/bin/env bash
echo "GraphQL: Could not resolve to a PullRequest with the number of 1." >&2; exit 1
X
cat > "$T/netdown" <<'X'
#!/usr/bin/env bash
n=$(cat "$T/count" 2>/dev/null || echo 0); echo $((n+1)) > "$T/count"
if (( n < ${OUTAGE:-99} )); then
  echo 'Post "https://api.github.com/graphql": dial tcp 140.82.121.5:443: connect: network is unreachable' >&2
  echo "error connecting to api.github.com" >&2; exit 1
fi
echo abc123
X
chmod +x "$T/real" "$T/netdown"
sleep() { :; }   # no real waiting in the test
out="$(gh_retry "$T/real" 2>/dev/null)"; rc=$?
check "(( rc != 0 ))" "non-transient error returns non-zero (rc=$rc)"
check "[[ -z \$out ]]" "and prints nothing on stdout"
rm -f "$T/count"; out="$(OUTAGE=2 gh_retry "$T/netdown" 2>/dev/null)"; rc=$?
check "(( rc == 0 )) && [[ \$out == abc123 ]]" "network outage is retried until it clears (rc=$rc out=$out)"
rm -f "$T/count"; out="$(AC_GH_RETRIES=3 gh_retry "$T/netdown" 2>/dev/null)"; rc=$?
check "(( rc != 0 ))" "outage outlasting the retries returns non-zero (rc=$rc)"
rm -rf "$T"; exit $fail
