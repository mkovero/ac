#!/usr/bin/env bash
# RC-11: pre-tag smoke matrix runner.
# Loops every default view × {1, 2, 8} synthetic channel counts through
# `ac-ui --synthetic --benchmark 1.5 --no-persist --view <name>` and
# prints a pass/fail summary. Use before tagging an RC to confirm the
# UI doesn't regress on any view.
#
# Run against a fresh `cargo build -p ac-ui` (or release):
#   cd ac-rs && cargo build -p ac-ui
#   ../scripts/rc-smoke.sh                      # uses target/debug/ac-ui
#   ../scripts/rc-smoke.sh release              # uses target/release/ac-ui
#
# Each spawn runs for 1.5 s of benchmark time, so the full matrix takes
# roughly 12 (views) × 3 (channel counts) × ~2 s = ~70 s.

set -uo pipefail

PROFILE="${1:-debug}"
cd "$(dirname "$0")/../ac-rs"
BIN="target/${PROFILE}/ac-ui"

if [[ ! -x "$BIN" ]]; then
    echo "error: $BIN not found — did you 'cargo build -p ac-ui${PROFILE:+ --release}'?" >&2
    exit 2
fi

VIEWS=(
    spectrum_ember goniometer iotransfer
    bode_mag coherence bode_phase group_delay
    nyquist ir
    spectrum waterfall scope
)
CHANNELS=(1 2 8)

pass=0
fail=0
fail_log=$(mktemp)

for v in "${VIEWS[@]}"; do
    for c in "${CHANNELS[@]}"; do
        printf "  %-15s × %d ch  ... " "$v" "$c"
        out=$("$BIN" --synthetic --no-persist --benchmark 1.5 \
              --view "$v" --channels "$c" 2>&1)
        rc=$?
        if [[ $rc -eq 0 && "$out" == *"ac-ui benchmark:"* ]]; then
            frames=$(echo "$out" | awk '/ac-ui benchmark:/ {print $(NF-1); exit}')
            printf "ok (%s frames)\n" "$frames"
            ((pass++))
        else
            printf "FAIL (rc=%d)\n" "$rc"
            echo "=== $v / $c (rc=$rc) ===" >> "$fail_log"
            echo "$out" >> "$fail_log"
            ((fail++))
        fi
    done
done

echo
printf "result: %d pass, %d fail\n" "$pass" "$fail"
if [[ $fail -gt 0 ]]; then
    echo "failure log:" >&2
    cat "$fail_log" >&2
    rm -f "$fail_log"
    exit 1
fi
rm -f "$fail_log"
