#!/usr/bin/env bash
# preflight.sh — silent state check of a rig, printed as a record block.
#
#   scripts/rig/preflight.sh <rig> [rev|none]
#
# Checks JACK (service, rate, period, required flags), where the analog
# capture block sits (the FF400's port order moves), the interface's ALSA
# baseline, the ac config (ceiling, channel map), what is running, installed
# binary hashes, and — given a rev — the staged build's hashes on the rig.
# Exit 1 if any FAIL line. Emits no audio; wiring is NOT checked here
# (that needs emission: probe-outputs.sh).
#
# Procedure: docs/runbooks/rig-testing.md.

source "$(dirname "$0")/lib.sh"

load_rig "${1:-}"
push_helpers
rev_arg=${2:-latest}
dest=""
if [[ $rev_arg != none ]]; then
    if rev="$(resolve_rev "$rev_arg" 2>/dev/null)"; then
        dest="$(rig_dest "$rev")"
    fi
fi

remote="$(cat <<'REMOTE'
set -u
fails=0
row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3"; [[ $1 == FAIL ]] && fails=$((fails + 1)); true; }

echo "### preflight ($RIGNAME, $(hostname), $(date -u +%Y-%m-%dT%H:%M:%SZ))"
echo
echo "| result | check | value |"
echo "|---|---|---|"

if systemctl is-active --quiet jack-ac.service; then row PASS "jack-ac.service" active; else row FAIL "jack-ac.service" "$(systemctl is-active jack-ac.service)"; fi
sr="$(jack_samplerate 2>/dev/null || echo none)"
bs="$(jack_bufsize 2>/dev/null || echo none)"
[[ $sr == "$RATE" ]] && row PASS "JACK rate" "$sr" || row FAIL "JACK rate" "$sr (want $RATE)"
[[ $bs == "$PERIOD" ]] && row PASS "JACK period" "$bs" || row FAIL "JACK period" "$bs (want $PERIOD)"
cmd="$(pgrep -ax jackd | head -1 | cut -d' ' -f2-)"
for f in $FLAGS; do
    [[ " $cmd " == *" $f "* ]] && row PASS "jackd flag $f" present || row FAIL "jackd flag $f" "missing — cmdline: $cmd"
done

pf="$(mktemp --suffix=.wav)"
ports=""; for i in $(seq 1 "$(jack_lsp | grep -c '^system:capture_')"); do ports="$ports system:capture_$i"; done
jack_rec -f "$pf" -d 1 -b 24 $ports >/dev/null 2>&1
po="$(python3 "$LIB/port_order.py" "$pf" "$FIRST" "$NA")"
[[ $? == 0 ]] && row PASS "JACK port order" "$(echo "$po" | head -1)" || row FAIL "JACK port order" "$(echo "$po" | tr '\n' ' ')"
rm -f "$pf"

for kv in $ALSA; do
    id=${kv%%=*}; want=${kv#*=}
    got="$(amixer -c "$CARD" cget numid="$id" 2>/dev/null | sed -n 's/.*: values=//p')"
    name="$(amixer -c "$CARD" cget numid="$id" 2>/dev/null | sed -n "s/.*name='\([^']*\)'.*/\1/p")"
    [[ $got == "$want" ]] && row PASS "ALSA $id ${name}" "$got" || row FAIL "ALSA $id ${name}" "${got:-unreadable} (want $want)"
done

cfgf="$HOME/.config/ac/config.json"
if [[ -f $cfgf ]]; then
    ceil_got="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("drive_max_dbfs"))' "$cfgf")"
    if python3 -c 'import sys; sys.exit(0 if sys.argv[1] != "None" and float(sys.argv[1]) <= float(sys.argv[2]) else 1)' "$ceil_got" "$CEIL"; then
        row PASS "ac drive_max_dbfs" "$ceil_got"
    else
        row FAIL "ac drive_max_dbfs" "$ceil_got (must be ≤ $CEIL)"
    fi
    for kv in $CFG; do
        k=${kv%%=*}; want=${kv#*=}
        got="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get(sys.argv[2]))' "$cfgf" "$k")"
        [[ $got == "$want" ]] && row PASS "ac $k" "$got" || row FAIL "ac $k" "$got (want $want)"
    done
else
    row FAIL "ac config" "$cfgf missing"
fi

d="$(pgrep -ax ac-daemon | tr '\n' ';')"
row INFO "ac-daemon running" "${d:-none}"
row INFO "/usr/local/bin/ac sha256" "$(sha256sum /usr/local/bin/ac 2>/dev/null | cut -c1-16)…"
row INFO "/usr/local/bin/ac-daemon sha256" "$(sha256sum /usr/local/bin/ac-daemon 2>/dev/null | cut -c1-16)…"

if [[ -n $DEST ]]; then
    if [[ -f $DEST/SHA256SUMS ]] && (cd "$DEST" && sha256sum --quiet -c SHA256SUMS >/dev/null 2>&1); then
        row PASS "staged build" "$DEST matches SHA256SUMS"
    else
        row FAIL "staged build" "$DEST missing or hash mismatch — run ship.sh"
    fi
    dp="$(sed -n 's/^it_loopback_ir_daemon_path=//p' "$DEST/MANIFEST.txt" 2>/dev/null)"
    if [[ -n $dp && "$(readlink -f "$dp")" == "$DEST/ac-daemon" ]]; then
        row PASS "it_loopback_ir daemon path" "$dp"
    else
        row FAIL "it_loopback_ir daemon path" "${dp:-unknown} does not resolve to $DEST/ac-daemon"
    fi
fi

xr="$(journalctl --since '-10min' --no-pager 2>/dev/null | grep -ciE 'jackd.*xrun')"
[[ $xr == 0 ]] && row PASS "jackd xrun log lines, last 10 min" 0 || row FAIL "jackd xrun log lines, last 10 min" "$xr"

echo
[[ $fails == 0 ]] && echo "preflight: all checks passed" || echo "preflight: $fails FAIL"
exit $((fails > 0))
REMOTE
)"
rig_bash "RATE=$(printf %q "$RIG_JACK_RATE") PERIOD=$(printf %q "$RIG_JACK_PERIOD") \
FLAGS=$(printf %q "$RIG_JACK_REQUIRED_FLAGS") CARD=$(printf %q "$RIG_ALSA_CARD") \
CEIL=$(printf %q "$RIG_DRIVE_CEILING_DBFS") ALSA=$(printf %q "${RIG_ALSA_EXPECT[*]}") \
CFG=$(printf %q "${RIG_AC_CONFIG_EXPECT[*]}") DEST=$(printf %q "$dest") \
RIGNAME=$(printf %q "$RIG_NAME") FIRST=$(printf %q "$RIG_ANALOG_CAPTURE_FIRST") \
NA=$(printf %q "$RIG_ANALOG_CAPTURES") LIB=$(printf %q "$RIG_STAGE_BASE/lib")" "$remote"
