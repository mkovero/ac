#!/usr/bin/env bash
# ff400.sh — RME Fireface 400 (card 0) initialization to known defaults
# Run once after boot, or any time you suspect settings have drifted.
#
# Routing: each PCM stream N goes to hardware output N at unity (32768 = 0 dB
# on this card's 0–65536 / -90..+6 dB scale).  Analog inputs/ADAT inputs are
# muted in the DSP mixer (hardware loopback = 0).
#
# This script sets no JACK channel-name aliases and does not change phantom
# power. snd_fireface's ADAT/analog block order is not stable across boots
# (see issue #444), so a fixed alias table would sometimes label an analog
# port as ADAT or vice versa. It only clears any `FF400:`-prefixed alias
# left over from an earlier run of an older version of this script.
#
# Usage:
#   ./ff400.sh          — apply defaults
#   ./ff400.sh show     — print current relevant settings
#   ./ff400.sh +4dbu    — set output/input levels to +4 dBu  (default)
#   ./ff400.sh -10dbv   — set output/input levels to -10 dBV
#   ./ff400.sh high     — set output/input levels to High

set -e
CARD=0

# ── ALSA control width helpers ───────────────────────────────────────────────
# The FF400's array controls (output-volume, stream-source-gain) carry a
# values= list whose length follows the driver's live port count, which is
# not fixed: 18 at 48 kHz, 14 at 96 kHz on this card (issue #444). Read the
# width from the control itself instead of assuming a literal count.
_ctl_width() {
    amixer -c "$CARD" cget numid="$1" 2>/dev/null \
        | grep ': values=' | sed 's/.*values=//' | awk -F, '{print NF}'
}

# ── JACK alias cleanup ───────────────────────────────────────────────────────
# This script publishes no channel-name aliases: on snd_fireface the block
# order is not determined here (issue #444), and a wrong alias looks like
# driver output. It only removes any `FF400:`-prefixed alias a previous run
# of this script may have left on a capture/playback port; any other alias
# (e.g. jackd's own `alsa_pcm:...`) is left untouched.
clear_ff400_aliases() {
    if ! jack_lsp &>/dev/null; then
        echo "  JACK aliases:       could not reach a JACK server; did not check aliases"
        echo "  JACK aliases:       check: jack_lsp from the user jackd runs as; ss -xlp | grep jack"
        return
    fi

    local port="" line stale=0
    while IFS= read -r line; do
        case "$line" in
            "   "*)
                local alias="${line#   }"
                case "$port" in
                    system:capture_*|system:playback_*)
                        case "$alias" in
                            FF400:*)
                                if ! jack_alias -u "$port" "$alias" 2>/dev/null; then
                                    echo "  JACK aliases:       could not unalias ${alias} on ${port}" >&2
                                    stale=1
                                fi
                                ;;
                        esac
                        ;;
                esac
                ;;
            *)
                port="$line"
                ;;
        esac
    done < <(jack_lsp -A)

    local remaining
    remaining=$(jack_lsp -A | awk '
        /^   / { if ($0 ~ /^   FF400:/ && port ~ /^system:(capture|playback)_/) print port ": " $0; next }
        { port = $0 }
    ')
    if [[ -n "$remaining" ]]; then
        echo "  JACK aliases:       FF400: alias still present after clearing:" >&2
        echo "$remaining" >&2
        exit 1
    fi
    if [[ $stale -ne 0 ]]; then
        exit 1
    fi

    echo "  JACK aliases:       cleared any FF400: alias left by an earlier run"
    echo "  JACK aliases:       none set here; block order is not determined by this script — to find it:"
    echo "                        silent capture: unconnected ADAT/S/PDIF inputs read exact digital zero"
    echo "                        drive a tone, watch meter:stream-input / meter:analog-output"
    echo "                        scripts/rig/preflight.sh's port-order row, where scripts/rig/ exists"
}

# ── Level mode ────────────────────────────────────────────────────────────────
# line-output-level / headphone-output-level / line-input-level
#   Item #0 'High'   Item #1 '-10dBV'   Item #2 '+4dBu'
MODE="${1:-+4dbu}"
case "${MODE,,}" in
    show)
        echo "=== Fireface 400 (card $CARD) current settings ==="
        _enum() {
            local numid=$1
            local idx; idx=$(amixer -c $CARD cget numid=$numid 2>/dev/null | grep ': values=' | sed 's/.*values=//')
            amixer -c $CARD cget numid=$numid 2>/dev/null \
                | grep "Item #${idx} " | sed "s/.*Item #${idx} '//;s/'$//"
        }
        _int() { amixer -c $CARD cget numid=$1 2>/dev/null | grep ': values=' | sed 's/.*values=//'; }
        _bool() { amixer -c $CARD cget numid=$1 2>/dev/null | grep ': values=' | sed 's/.*values=//'; }
        printf "  %-26s %s\n" "line-output-level:"      "$(_enum 93)"
        printf "  %-26s %s\n" "headphone-output-level:" "$(_enum 94)"
        printf "  %-26s %s\n" "line-input-level:"       "$(_enum 89)"
        printf "  %-26s %s\n" "mic-input-gain:"         "$(_int 81) dB"
        printf "  %-26s %s\n" "line-input-gain:"        "$(_int 82) dB"
        printf "  %-26s %s\n" "line-3/4-inst:"          "$(_bool 91)"
        printf "  %-26s %s\n" "line-3/4-pad:"           "$(_bool 92)"
        printf "  %-26s %s\n" "mic-1/2-powering (driver cache, not set by this script):" "$(_bool 90)"
        echo ""
        echo "  stream-source-gain diagonal (JACK ch → hw output), by index only:"
        echo "  (this script does not know which name belongs to which index — see JACK aliases below)"
        for i in $(seq 63 80); do
            idx=$((i - 63))
            diag=$(amixer -c $CARD cget numid=$i 2>/dev/null \
                   | grep ': values' \
                   | sed 's/.*values=//' \
                   | python3 -c "import sys; v=sys.stdin.read().strip().split(','); print(v[$idx])" 2>/dev/null)
            [[ -n "$diag" ]] || continue
            printf "    ch%02d %s\n" $idx "$diag"
        done
        exit 0
        ;;
    +4dbu|+4)   LEVEL_IDX=2 ; LEVEL_NAME="+4 dBu"  ;;
    -10dbv|-10) LEVEL_IDX=1 ; LEVEL_NAME="-10 dBV" ;;
    high)       LEVEL_IDX=0 ; LEVEL_NAME="High"     ;;
    *)
        echo "Usage: $0 [show | +4dbu | -10dbv | high]"
        exit 1
        ;;
esac

echo "=== Fireface 400 init  (card $CARD, level mode: $LEVEL_NAME) ==="

# ── Clear any FF400: JACK alias left by an earlier run ───────────────────────
# Runs first: with set -e, a failing mixer write below must not leave a wrong
# alias in place (issue #444).
clear_ff400_aliases

# ── Ensure snd-fireface-ctl service is running (bridges ALSA → FireWire hw) ──
#systemctl --user restart snd-fireface-ctl.service
#sleep 1
#echo "  snd-fireface-ctl:   restarted"

# ── Output / input reference levels ──────────────────────────────────────────
amixer -c $CARD cset numid=93 $LEVEL_IDX >/dev/null  # line-output-level
amixer -c $CARD cset numid=94 $LEVEL_IDX >/dev/null  # headphone-output-level
amixer -c $CARD cset numid=89 $LEVEL_IDX >/dev/null  # line-input-level
echo "  output level:       $LEVEL_NAME"
echo "  headphone level:    $LEVEL_NAME"
echo "  input level:        $LEVEL_NAME"

# ── Gain controls ─────────────────────────────────────────────────────────────
amixer -c $CARD cset numid=81 0,0  >/dev/null  # mic-input-gain  → 0 dB
amixer -c $CARD cset numid=82 0,0  >/dev/null  # line-input-gain → 0 dB
echo "  mic-input-gain:     0 dB"
echo "  line-input-gain:    0 dB"

# ── Input mode ────────────────────────────────────────────────────────────────
# Phantom power (numid=90, mic-1/2-powering) is not touched here: it belongs
# to the rig profile, not to this generic init (issue #444).
amixer -c $CARD cset numid=91 off,off >/dev/null  # line-3/4-inst  → off
amixer -c $CARD cset numid=92 off,off >/dev/null  # line-3/4-pad   → off
echo "  line-3/4 inst/pad:  off"

# ── Output volume (numid 8, unity = 32768 = 0 dB) ────────────────────────────
vol_width=$(_ctl_width 8)
vol_vals=$(python3 -c "print(','.join(['32768'] * $vol_width))")
amixer -c $CARD cset numid=8 "$vol_vals" >/dev/null
echo "  output-volume:      unity (32768) × $vol_width"

# ── PCM stream → hardware output routing (identity, 32768 = 0 dB) ────────────
# numid 63..80 = mixer:stream-source-gain index 0..17
# Each row's value-list width follows the control's live width, not a
# literal count (issue #444); index i is only set when it falls inside that
# width.
echo "  stream routing:     identity @ 0 dB (32768)"
for i in $(seq 0 17); do
    numid=$((63 + i))
    width=$(_ctl_width "$numid")
    [[ -n "$width" && "$width" -gt 0 ]] || continue
    vals=$(python3 -c "
n=$width
i=$i
v=[0]*n
if i < n:
    v[i]=32768
print(','.join(map(str,v)))
")
    amixer -c $CARD cset numid=$numid "$vals" >/dev/null
done

# ── Analog and ADAT hardware inputs muted in DSP mixer ───────────────────────
# (analog-source-gain and adat-source-gain all 0 — no hardware loopback)
# numid 9..26  = mixer:analog-source-gain index 0..17
# numid 45..62 = mixer:adat-source-gain   index 0..17
echo "  analog/adat loopback: muted"
for numid in $(seq 9 26) $(seq 45 62); do
    amixer -c $CARD cset numid=$numid "0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0" >/dev/null
done

echo ""
echo "Done.  Run  ./ff400.sh show  to verify."
