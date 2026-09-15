#!/usr/bin/env bash
# probe_outputs_remote_test.sh — regression test for
# scripts/rig/lib/probe_remote.sh, the emission loop probe-outputs.sh runs
# on the rig. No rig, no JACK, no real ac-daemon: stub `ac`/`jack_rec` on
# PATH, and the exact same assembly (set -eu + REMOTE_USE_BUILD +
# probe_remote.sh's own text) probe-outputs.sh sends over ssh, run locally
# with `bash -c` instead.
#
#   bash scripts/rig/probe_outputs_remote_test.sh
#
# PR #441 QA (codex) finding this covers: a failed `ac setup output` or
# `ac generate` was not checked, so a lost ack could let emission proceed
# through whatever output the daemon still had configured, and a failed
# generate did not make the wrapper exit nonzero. Three failure shapes below
# each prove the loop stops before anything is captured; a fourth proves the
# ok/ok path still runs end to end.
#
# Procedure these scripts implement: docs/runbooks/rig-testing.md.

source "$(dirname "$0")/lib.sh"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
bindir="$tmp/bin"
homedir="$tmp/home"
libdir="$tmp/lib"
mkdir -p "$bindir" "$homedir/.config/ac" "$libdir"

cat >"$bindir/ac" <<'EOF'
#!/usr/bin/env bash
# Stub `ac`. STUB_SETUP_MODE: ok (updates config) | fail (exit 1) |
# lie (exit 0, config left untouched — a lost ack that the daemon still
# applied, or didn't). STUB_GENERATE_MODE: ok | fail (exit 1, as check_ack's
# process::exit(1) does when the daemon rejects the command).
cfg="$HOME/.config/ac/config.json"
if [[ $1 == setup && $2 == output ]]; then
    ch=$3
    case "${STUB_SETUP_MODE:-ok}" in
        fail) exit 1 ;;
        lie) exit 0 ;;
        *) python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
d["output_channel"] = int(sys.argv[2])
json.dump(d, open(sys.argv[1], "w"))
' "$cfg" "$ch" ;;
    esac
elif [[ $1 == generate ]]; then
    case "${STUB_GENERATE_MODE:-ok}" in
        fail) exit 1 ;;
        *) sleep 0.2 ;;
    esac
fi
exit 0
EOF
chmod +x "$bindir/ac"

cat >"$bindir/jack_rec" <<'EOF'
#!/usr/bin/env bash
# Stub jack_rec: ignore -d (no real capture, keep the test fast), create
# the target file so its existence can be asserted, then exit.
f=""
while (($#)); do
    case $1 in
        -f) f=$2; shift 2 ;;
        *) shift ;;
    esac
done
[[ -n $f ]] && : >"$f"
exit "${STUB_JACK_REC_EXIT:-0}"
EOF
chmod +x "$bindir/jack_rec"

cat >"$libdir/chan_levels.py" <<'EOF'
print("header (stub)")
print("stub-chan-levels-ran")
EOF

reset_cfg() {
    cat >"$homedir/.config/ac/config.json" <<'JSON'
{"output_channel": 0, "input_channel": 0}
JSON
}

run_probe() {  # run_probe <outputs>
    local outs=$1 run="$tmp/run"
    rm -rf "$run"
    mkdir -p "$run"
    (
        HOME="$homedir" PATH="$bindir:$PATH" \
        DEST="" RUN="$run" LEVEL=-60 SECS=3 FREQ=1000 OUTS="$outs" NCAP=2 LIB="$libdir" \
        STUB_SETUP_MODE="${STUB_SETUP_MODE:-ok}" STUB_GENERATE_MODE="${STUB_GENERATE_MODE:-ok}" \
        bash -c "set -eu
$REMOTE_USE_BUILD
$(cat "$(dirname "$0")/lib/probe_remote.sh")"
    )
}

fail() { echo "FAIL: $1"; exit 1; }

# --- happy path: both outputs emit, both get captured and analyzed ---
reset_cfg
STUB_SETUP_MODE=ok STUB_GENERATE_MODE=ok
out="$(run_probe "0 1" 2>&1)" && rc=0 || rc=$?
[[ $rc == 0 ]] || fail "ok/ok: expected exit 0, got $rc — output:\n$out"
[[ -f $tmp/run/probe_out0.wav && -f $tmp/run/probe_out1.wav ]] ||
    fail "ok/ok: expected both outputs captured"
[[ $out == *stub-chan-levels-ran* ]] || fail "ok/ok: expected analysis to run"
echo "ok/ok: emits and analyzes both outputs, exit 0"

# --- ac setup fails outright: must stop before anything emits ---
reset_cfg
STUB_SETUP_MODE=fail STUB_GENERATE_MODE=ok
out="$(run_probe "0" 2>&1)" && rc=0 || rc=$?
[[ $rc != 0 ]] || fail "setup=fail: expected nonzero exit"
[[ -f $tmp/run/probe_out0.wav ]] && fail "setup=fail: nothing should have been captured"
echo "setup=fail: refuses before emission, exit $rc"

# --- ac setup ack lost/wrong: daemon config disagrees with the request ---
# Requests output 1 while "lie" leaves the config at its reset_cfg default
# of 0, so current_output() must actually disagree with what was asked for
# (using output 0 here would pass by coincidence, not by the check working).
reset_cfg
STUB_SETUP_MODE=lie STUB_GENERATE_MODE=ok
out="$(run_probe "1" 2>&1)" && rc=0 || rc=$?
[[ $rc != 0 ]] || fail "setup=lie: expected nonzero exit"
[[ -f $tmp/run/probe_out1.wav ]] && fail "setup=lie: nothing should have been captured"
[[ -f $tmp/run/gen_1.log ]] && fail "setup=lie: ac generate should never have been reached"
[[ $out == *"refusing to emit"* ]] || fail "setup=lie: expected the config-mismatch refusal message"
echo "setup=lie: caught by the daemon-config check, exit $rc"

# --- ac generate's own ack rejected: wrapper must not read this as clean ---
reset_cfg
STUB_SETUP_MODE=ok STUB_GENERATE_MODE=fail
out="$(run_probe "0" 2>&1)" && rc=0 || rc=$?
[[ $rc != 0 ]] || fail "generate=fail: expected nonzero exit"
[[ $out == *"#### ac output 0"* ]] && fail "generate=fail: must not report a clean result for this channel"
echo "generate=fail: exit $rc, no channel reported as clean"

echo "probe_remote.sh: all cases as expected"
