#!/usr/bin/env bash
# Attach samply to a running ac-ui process (the one launched by `ac monitor`).
# Use this when the synthetic + benchmark path in `profile-ui-samply.sh`
# doesn't reproduce a CPU regression you can only see against the live daemon.
#
# Usage:
#   # Terminal A (your normal use):
#   ac monitor                               # leave it running, observe 100% CPU
#
#   # Terminal B (one-shot profile capture):
#   scripts/profile-ui-attach.sh [duration_sec]   # default 20 s
#
# Env overrides:
#   PID=12345 scripts/profile-ui-attach.sh 30   # explicit pid (skip auto-detect)
#
# Output: /tmp/ac-ui-profile-<timestamp>.json.gz
# View with: samply load <file>
#
# Prereqs (one-time):
#   sudo sh -c 'echo 1 > /proc/sys/kernel/perf_event_paranoid'
#   cd ac-rs && cargo build --profile profiling -p ac-ui
#   # Replace the dev build so `ac monitor` picks up the symbol-rich binary:
#   cp target/profiling/ac-ui target/debug/ac-ui

set -uo pipefail

DURATION="${1:-20}"

if [[ -z "${PID:-}" ]]; then
    PID=$(pgrep -x ac-ui | head -1)
fi
if [[ -z "$PID" ]]; then
    echo "no ac-ui process found — start \`ac monitor\` first or pass PID=<pid>" >&2
    exit 1
fi
if ! kill -0 "$PID" 2>/dev/null; then
    echo "pid $PID not running" >&2
    exit 1
fi

paranoid=$(cat /proc/sys/kernel/perf_event_paranoid 2>/dev/null || echo 2)
if [[ "$paranoid" -gt 1 ]]; then
    cat >&2 <<EOF
perf_event_paranoid=$paranoid — samply needs <= 1 for non-root profiling.
Fix:    sudo sh -c 'echo 1 > /proc/sys/kernel/perf_event_paranoid'
EOF
    exit 1
fi

if ! command -v samply >/dev/null; then
    echo "samply not found — install with: cargo install samply" >&2
    exit 1
fi

bin=$(readlink -f "/proc/$PID/exe" 2>/dev/null || echo "<unknown>")
echo ">> attaching to ac-ui pid=$PID  (binary: $bin)"
echo ">> sampling for ${DURATION}s — go drive the UI now (move mouse, switch views, anything)"

out="/tmp/ac-ui-profile-$(date +%Y%m%d-%H%M%S).json.gz"

# Run samply in the foreground so the terminal makes timing obvious. samply
# returns when the duration limit is reached OR when the target exits.
samply record --no-open --save-only -o "$out" -d "$DURATION" --pid "$PID"
status=$?

if [[ -s "$out" ]]; then
    echo
    echo ">> profile written: $out ($(stat -c%s "$out") bytes)"
    echo ">> share for analysis:"
    echo "     scp $out <analysis-host>:/tmp/"
    echo "   or view locally:"
    echo "     samply load $out"
else
    echo "!! profile missing or empty (samply exit=$status)" >&2
    exit 1
fi
