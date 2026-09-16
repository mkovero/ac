#!/usr/bin/env bash
# xrun_soak_remote_test.sh — regression test for
# scripts/rig/lib/xrun_soak_remote.sh, the soak block xrun-soak.sh runs on
# the rig. No rig, no JACK, no real ac-daemon: stub `ac`, the JACK tools,
# journalctl and pgrep/pkill on PATH; the real util-linux `script` and
# coreutils `timeout`, whose exit-status behaviour is what the finding is
# about. Same assembly (set -eu + REMOTE_USE_BUILD + the file's own text)
# xrun-soak.sh sends over ssh, run locally with `bash -c` instead.
#
#   bash scripts/rig/xrun_soak_remote_test.sh
#
# PR #441 codex-qa finding this covers (35ddb7a1): `script -qc` returns 0
# whatever its child did, and nothing checked elapsed time or the mid-run
# daemon, so an `ac monitor` that drew one xruns=0 frame and exited passed a
# half-length soak. Each early-exit shape below must fail; a held load with
# no xruns must pass. Takes about 12 s (a 2 s soak per case).
#
# Procedure these scripts implement: docs/runbooks/rig-testing.md.

source "$(dirname "$0")/lib.sh"

command -v script >/dev/null || die "util-linux script is required"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
bindir="$tmp/bin"
mkdir -p "$bindir"
real_timeout="$(command -v timeout)"

stub() {  # stub <name> <body>
    printf '#!/usr/bin/env bash\n%s\n' "$2" >"$bindir/$1"
    chmod +x "$bindir/$1"
}

# STUB_MONITOR: hold (redraw xruns=0 until killed) | xrun (hold, xruns=3) |
# fail (one xruns=0 frame, exit 1) | quit (one xruns=0 frame, exit 0).
stub ac '
case $1 in
    monitor)
        n=0
        [[ $STUB_MONITOR == xrun ]] && n=3
        printf "\033[H level -60 dBFS xruns=%s\n" "$n"
        case $STUB_MONITOR in
            fail) exit 1 ;;
            quit) exit 0 ;;
        esac
        while sleep 0.2; do printf "\033[H level -60 dBFS xruns=%s\n" "$n"; done ;;
esac
exit 0'
# STUB_TIMEOUT=lie: runs the command for 0.5 s only, then reports a timeout
# (124) as if the full duration had passed.
stub timeout "
if [[ \${STUB_TIMEOUT:-} == lie ]]; then
    $(printf %q "$real_timeout") 0.5 \"\${@:2}\"
    exit 124
fi
exec $(printf %q "$real_timeout") \"\$@\""
# STUB_DAEMON=up: `pgrep -xn ac-daemon` names a live process (this test's
# shell), so daemon_identity resolves. Plain `pgrep -x ac-daemon` (the
# REMOTE_USE_BUILD stop loop) always reports nothing running.
stub pgrep '
case "$*" in
    "-xn ac-daemon") [[ $STUB_DAEMON == up ]] && { echo "$STUB_DAEMON_PID"; exit 0; }; exit 1 ;;
    "-ax jackd") echo "1 jackd -d alsa"; exit 0 ;;
esac
exit 1'
stub pkill 'exit 1'
stub jack_lsp '
echo system:capture_1
[[ $STUB_DAEMON == up ]] && printf "ac-daemon:in_1\nac-daemon:in_2\n"
exit 0'
stub jack_samplerate 'echo 96000'
stub jack_bufsize 'echo 64'
stub journalctl 'echo "-- No entries --"'

run_soak() {  # run_soak <monitor> <daemon> [timeout]
    PATH="$bindir:$PATH" STUB_MONITOR="$1" STUB_DAEMON="$2" STUB_TIMEOUT="${3:-}" \
        STUB_DAEMON_PID=$$ DEST="" SECS=2 NCAP=2 \
        bash -c "set -eu
$REMOTE_USE_BUILD
$(cat "$(dirname "$0")/lib/xrun_soak_remote.sh")" </dev/null
}

fail() { echo "FAIL: $1"; exit 1; }

expect_fail() {  # expect_fail <label> <expected output fragment> <run_soak args...>
    local label=$1 want=$2 out rc
    shift 2
    out="$(run_soak "$@" 2>&1)" && rc=0 || rc=$?
    [[ $rc != 0 ]] || fail "$label: expected nonzero exit — output: $out"
    [[ $out == *"daemon-side counter: xruns=0"* || $1 == xrun ]] ||
        fail "$label: the stub's xruns=0 frame should have been captured, so only the new checks can fail it — output: $out"
    [[ $out == *"$want"* ]] || fail "$label: expected \"$want\" — output: $out"
    echo "$label: exit $rc"
}

out="$(run_soak hold up 2>&1)" && rc=0 || rc=$?
[[ $rc == 0 ]] || fail "held load: expected exit 0, got $rc — output: $out"
[[ $out == *"exit status 124"* && $out == *"daemon-side counter: xruns=0"* ]] ||
    fail "held load: record incomplete — output: $out"
echo "held load, no xruns: exit 0"

expect_fail "monitor exits 1 early" "ended with status 1, not by the" fail up
expect_fail "monitor exits 0 early" "ended with status 0, not by the" quit up
# Elapsed time is whole seconds around a ~0.5 s run, so it reads 0 or 1
# depending on the second boundary; match only the part that cannot vary and
# that no other check prints.
expect_fail "timeout 124 before SECS" "less than the requested 2 s" hold up lie
expect_fail "no daemon at checkpoint" "no ac-daemon or no ac-daemon JACK ports" hold down
expect_fail "held load with xruns" "daemon-side counter: xruns=3" xrun up

echo "xrun_soak_remote.sh: all cases as expected"
