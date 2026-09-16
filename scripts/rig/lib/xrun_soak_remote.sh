#!/usr/bin/env bash
# xrun_soak_remote.sh — the soak block xrun-soak.sh runs on the rig. A real
# file, not an inline heredoc, so xrun_soak_remote_test.sh runs this exact
# text locally against stub `ac`/JACK tools (same pattern as probe_remote.sh).
#
# A soak passes only when the load was actually held (PR #441 codex-qa
# finding, 35ddb7a1): `ac monitor` must be ended by `timeout` itself (status
# 124, kept through `script -e`), the wall time must cover SECS, the
# mid-run checkpoint must see a daemon and its JACK ports, and both xrun
# counts must be zero. An `ac monitor` that exits early — zero or not — fails
# the soak even when its last frame read xruns=0.
#
# Expects on entry (set by xrun-soak.sh via rig_bash, or by the test):
# DEST SECS NCAP, and REMOTE_USE_BUILD (lib.sh) already run ahead of this
# file, defining daemon_identity().
#
# Not meant to be executed directly — xrun-soak.sh concatenates
# REMOTE_USE_BUILD with this file's content and passes the result to
# `bash -c` over ssh.

set -eu

trap 'ac stop >/dev/null 2>&1 || true; pkill -x ac-daemon 2>/dev/null || true' EXIT
ts="$(mktemp)"
t0=$(date +%s)
(
    sleep $((SECS / 2))
    ident="$(daemon_identity)"
    ports=$(jack_lsp 2>/dev/null | grep -c '^ac-daemon' || true)
    printf '%s\n%s\n' "$ident" "$ports" >"$ts.mid"
) &
monitor_status=0
script -qec "timeout $SECS ac monitor 0-$((NCAP - 1)) --tui" "$ts" >/dev/null 2>&1 || monitor_status=$?
elapsed=$(($(date +%s) - t0))
ac stop >/dev/null 2>&1 || true
wait
if jlog=$(journalctl --since "@$t0" --no-pager 2>&1); then
    log=$(grep -ciE 'jackd.*xrun' <<<"$jlog" || true)
else
    log=unreadable
fi
daemon=$(tr -d '\033' <"$ts" | grep -o 'xruns=[0-9]*' | tail -1 || true)
mid_ident=none mid_ports=0
if [[ -s $ts.mid ]]; then
    { read -r mid_ident; read -r mid_ports; } <"$ts.mid"
fi
echo "- mid-run: daemon $mid_ident, $mid_ports ac-daemon JACK ports"
echo "- ac monitor: exit status $monitor_status (124 = held until timeout), ${elapsed} s of ${SECS} s"
echo "- JACK: $(jack_samplerate) Hz, $(jack_bufsize) frames, jackd: $(pgrep -ax jackd | cut -d' ' -f2-)"
echo "- jackd xrun log lines: $log"
echo "- daemon-side counter: ${daemon:-not captured}"
rm -f "$ts" "$ts.mid"
ok=1
if [[ $monitor_status != 124 ]]; then
    echo "- FAIL: ac monitor ended with status $monitor_status, not by the ${SECS} s timeout — the load was not held"
    ok=0
fi
if ((elapsed < SECS)); then
    echo "- FAIL: soak lasted ${elapsed} s, less than the requested ${SECS} s"
    ok=0
fi
if [[ $mid_ident == none || $mid_ports == 0 ]]; then
    echo "- FAIL: no ac-daemon or no ac-daemon JACK ports at the mid-run checkpoint"
    ok=0
fi
[[ $ok == 1 && $log == 0 && ${daemon#xruns=} == 0 ]]
