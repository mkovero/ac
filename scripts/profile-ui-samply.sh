#!/usr/bin/env bash
# Profile ac-ui under samply with the synthetic backend + benchmark exit.
#
# Usage:
#   scripts/profile-ui-samply.sh [duration_sec]
#
# Env overrides:
#   VIEW=waterfall CHANNELS=4 BINS=1000 RATE=20 scripts/profile-ui-samply.sh 30
#   VIEW=spectrum  scripts/profile-ui-samply.sh 30
#
# Output: /tmp/ac-ui-profile-<timestamp>.json.gz
# View with: samply load <file>

set -euo pipefail

DURATION="${1:-20}"
VIEW="${VIEW:-waterfall}"           # spectrum | waterfall
CHANNELS="${CHANNELS:-4}"
BINS="${BINS:-1000}"                # synthetic producer bin count
RATE="${RATE:-20}"                  # synthetic producer fps

repo_root=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo_root"

if ! command -v samply >/dev/null; then
    echo "samply not found — install with: cargo install samply" >&2
    exit 1
fi

paranoid=$(cat /proc/sys/kernel/perf_event_paranoid 2>/dev/null || echo 2)
if [[ "$paranoid" -gt 1 ]]; then
    echo "perf_event_paranoid=$paranoid — samply needs <= 1 for non-root profiling." >&2
    exit 1
fi

if [[ -z "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]]; then
    echo "no DISPLAY or WAYLAND_DISPLAY — ac-ui needs a graphical session" >&2
    exit 1
fi

echo ">> building ac-ui (profile=profiling)..."
(cd ac-rs && cargo build --profile profiling -p ac-ui)

ui_bin="ac-rs/target/profiling/ac-ui"
out="/tmp/ac-ui-profile-$(date +%Y%m%d-%H%M%S).json.gz"

cleanup() {
    if [[ -n "${samply_pid:-}" ]] && kill -0 "$samply_pid" 2>/dev/null; then
        kill -TERM "$samply_pid" 2>/dev/null || true
        wait "$samply_pid" 2>/dev/null || true
    fi
    if [[ -n "${ui_pid:-}" ]] && kill -0 "$ui_pid" 2>/dev/null; then
        kill -TERM "$ui_pid" 2>/dev/null || true
        wait "$ui_pid" 2>/dev/null || true
    fi
}
trap cleanup EXIT INT TERM

echo ">> launching ac-ui (synthetic, view=$VIEW, channels=$CHANNELS, bins=$BINS @ ${RATE}Hz, benchmark=${DURATION}s)..."
"$ui_bin" --synthetic \
    --channels "$CHANNELS" \
    --bins     "$BINS" \
    --rate     "$RATE" \
    --view     "$VIEW" \
    --benchmark "$DURATION" \
    >/tmp/ac-ui-profile.log 2>&1 &
ui_pid=$!

# Give wgpu a moment to pick up adapter + create swapchain before we attach.
sleep 0.8
if ! kill -0 "$ui_pid" 2>/dev/null; then
    echo "!! ac-ui exited before we could attach — see /tmp/ac-ui-profile.log" >&2
    tail -40 /tmp/ac-ui-profile.log >&2
    exit 1
fi

echo ">> attaching samply to pid=$ui_pid → $out"
samply record --no-open -o "$out" --pid "$ui_pid" \
    >/tmp/ac-ui-samply.log 2>&1 &
samply_pid=$!
sleep 0.5

# Wait for ac-ui to finish its benchmark window (it exits on its own).
max_wait=$((DURATION + 20))
for _ in $(seq 1 "$max_wait"); do
    kill -0 "$ui_pid" 2>/dev/null || break
    sleep 1
done
unset ui_pid

echo ">> waiting for samply to finalize profile..."
for _ in $(seq 1 20); do
    kill -0 "$samply_pid" 2>/dev/null || break
    sleep 1
done
if kill -0 "$samply_pid" 2>/dev/null; then
    kill -TERM "$samply_pid" 2>/dev/null || true
    sleep 2
    kill -KILL "$samply_pid" 2>/dev/null || true
fi
wait "$samply_pid" 2>/dev/null || true
unset samply_pid

echo
if [[ -s "$out" ]]; then
    echo ">> profile written: $out ($(stat -c%s "$out") bytes)"
    echo ">> view: samply load $out"
else
    echo "!! profile missing or empty — check /tmp/ac-ui-profile.log and /tmp/ac-ui-samply.log" >&2
    exit 1
fi
