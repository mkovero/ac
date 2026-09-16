#!/usr/bin/env bash
# probe_remote.sh — the per-output emission loop probe-outputs.sh runs on
# the rig. A real file, not an inline heredoc, so probe_remote_test.sh can
# run this exact text locally against stub `ac`/`jack_rec` — no drift
# between what ships and what's tested (PR #441 QA finding: a failed `ac
# setup`/`ac generate` was not checked, so a lost ack could drive the
# previous output, or the daemon's unchanged one, above the requested
# ceiling with nothing to catch it).
#
# Expects on entry (set by probe-outputs.sh via rig_bash, or by
# probe_remote_test.sh's stub): DEST RUN LEVEL SECS FREQ OUTS NCAP LIB,
# and REMOTE_USE_BUILD (lib.sh) already run ahead of this file, defining
# daemon_identity() and putting a staged build on PATH when DEST is set.
#
# Not meant to be executed directly — probe-outputs.sh concatenates
# REMOTE_USE_BUILD with this file's content and passes the result to `bash
# -c` over ssh (or, in the test, to a local `bash -c` with stubs on PATH).

set -eu

die() {
    echo "error: $*" >&2
    exit 1
}

mkdir -p "$RUN" && cd "$RUN"
cfg="$HOME/.config/ac/config.json"
orig_out="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["output_channel"])' "$cfg")"
orig_in="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["input_channel"])' "$cfg")"
cleanup() {
    # set -e applies inside a trap too — every command here must survive a
    # nonzero exit on its own (`ac setup` restoring, or `pkill` matching
    # nothing) so a failure partway through cleanup can't skip pkill and
    # leave a daemon running.
    ac stop >/dev/null 2>&1 || true
    ac setup output "$orig_out" input "$orig_in" >/dev/null 2>&1 || true
    pkill -x ac-daemon 2>/dev/null || true
}
trap cleanup EXIT

current_output() {
    python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["output_channel"])' "$cfg"
}

ac setup output "${OUTS%% *}" >/dev/null
echo "- daemon executable: $(daemon_identity)"
caps=""
for i in $(seq 1 "$NCAP"); do caps="$caps system:capture_$i"; done
rec=$((SECS - 2))
# shellcheck disable=SC2086  # OUTS is a deliberately space-separated list (--outputs "0 1")
for ch in $OUTS; do
    ac setup output "$ch" >/dev/null
    # A lost ack exits only the client (check_ack), so a following generate
    # could otherwise ride on whatever output the daemon still had — verify
    # the daemon's own config agrees before anything emits.
    got="$(current_output)"
    [[ $got == "$ch" ]] ||
        die "requested output $ch, daemon config reads $got — refusing to emit"
    ac generate level "$LEVEL" "$LEVEL" "${FREQ}hz" "${SECS}s" >"gen_$ch.log" 2>&1 &
    genpid=$!
    sleep 0.7
    # shellcheck disable=SC2086  # caps is a deliberately space-separated list of port names
    jack_rec -f "probe_out$ch.wav" -d "$rec" -b 24 $caps >/dev/null
    # Plain `wait` with no argument always returns 0, regardless of the
    # background job's own exit status — capture the pid so a generate that
    # never started (e.g. its own ack rejected) is not read as a clean run.
    wait "$genpid"
    ac stop >/dev/null
    echo
    echo "#### ac output $ch — $(grep -o 'system:playback_[0-9]*' "gen_$ch.log" | head -1)"
    echo
    python3 "$LIB/chan_levels.py" "probe_out$ch.wav" "$FREQ" | tail -n +2
done
