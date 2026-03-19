#!/usr/bin/env bash
# ff400.sh — RME Fireface 400 (card 0) initialization to known defaults
# Run once after boot, or any time you suspect settings have drifted.
#
# Routing: each PCM stream N goes to hardware output N at unity (32768 = 0 dB
# on this card's 0–65536 / -90..+6 dB scale).  Analog inputs/ADAT inputs are
# muted in the DSP mixer (hardware loopback = 0).
#
# Usage:
#   ./ff400.sh          — apply defaults
#   ./ff400.sh show     — print current relevant settings
#   ./ff400.sh +4dbu    — set output/input levels to +4 dBu  (default)
#   ./ff400.sh -10dbv   — set output/input levels to -10 dBV
#   ./ff400.sh high     — set output/input levels to High

set -e
CARD=0

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
        printf "  %-26s %s\n" "mic-1/2-powering:"       "$(_bool 90)"
        # FF400 hardware output order (ALSA/JACK, 0-indexed) — confirmed empirically:
        #   0-7  : ADAT 1–8
        #   8-9  : SPDIF L/R
        #   10-17: Analog AN1–AN8  (rear line outputs)
        CHNAME=(ADAT1 ADAT2 ADAT3 ADAT4 ADAT5 ADAT6 ADAT7 ADAT8 SPDIF-L SPDIF-R AN1 AN2 AN3 AN4 AN5 AN6 AN7 AN8)
        echo ""
        echo "  stream-source-gain diagonal (JACK ch → hw output):"
        for i in $(seq 63 80); do
            idx=$((i - 63))
            diag=$(amixer -c $CARD cget numid=$i 2>/dev/null \
                   | grep ': values' \
                   | sed 's/.*values=//' \
                   | python3 -c "import sys; v=sys.stdin.read().strip().split(','); print(v[$idx])")
            printf "    ch%02d %-10s %s\n" $idx "${CHNAME[$idx]}" "$diag"
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

# ── Ensure snd-fireface-ctl service is running (bridges ALSA → FireWire hw) ──
systemctl --user restart snd-fireface-ctl.service
sleep 1
echo "  snd-fireface-ctl:   restarted"

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
amixer -c $CARD cset numid=91 off,off >/dev/null  # line-3/4-inst  → off
amixer -c $CARD cset numid=92 off,off >/dev/null  # line-3/4-pad   → off
amixer -c $CARD cset numid=90 off,off >/dev/null  # mic-1/2-powering (phantom) → off
echo "  line-3/4 inst/pad:  off"
echo "  phantom power:      off"

# ── Output volume (numid 8, 18 channels, unity = 32768 = 0 dB) ───────────────
amixer -c $CARD cset numid=8 \
    32768,32768,32768,32768,32768,32768,32768,32768,32768,\
    32768,32768,32768,32768,32768,32768,32768,32768,32768 >/dev/null
echo "  output-volume:      unity (32768) × 18"

# ── PCM stream → hardware output routing (identity, 32768 = 0 dB) ────────────
# numid 63..80 = mixer:stream-source-gain index 0..17
# Each row is 18 values; set position [N] = 32768, rest = 0
echo "  stream routing:     identity @ 0 dB (32768)"
for i in $(seq 0 17); do
    numid=$((63 + i))
    vals=$(python3 -c "v=[0]*18; v[$i]=32768; print(','.join(map(str,v)))")
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

# ── JACK port aliases ─────────────────────────────────────────────────────────
# Sets human-readable aliases on all 18 playback and capture ports.
# Aliases show up in QjackCtl, Carla, Catia etc. but jack_lsp still shows
# the real name.  Safe to run even if JACK is not running (fails silently).
CHNAME=(ADAT1 ADAT2 ADAT3 ADAT4 ADAT5 ADAT6 ADAT7 ADAT8 SPDIF-L SPDIF-R AN1 AN2 AN3 AN4 AN5 AN6 AN7 AN8)
if jack_lsp &>/dev/null; then
    echo "  JACK aliases:       setting..."
    for i in $(seq 0 17); do
        n=$((i + 1))
        jack_alias "system:playback_${n}"  "FF400:playback_${CHNAME[$i]}"  2>/dev/null || true
        jack_alias "system:capture_${n}"   "FF400:capture_${CHNAME[$i]}"   2>/dev/null || true
    done
    echo "  JACK aliases:       done"
else
    echo "  JACK aliases:       skipped (JACK not running)"
fi

echo ""
echo "Done.  Run  ./ff400.sh show  to verify."
