#!/usr/bin/env bash
# ff400.sh — RME Fireface 400 initialization to known defaults
# Run once after boot, or any time you suspect settings have drifted.
#
# Routing: each PCM stream N goes to hardware output N at unity (32768 = 0 dB
# on this card's 0–65536 / -90..+6 dB scale).  Analog, S/PDIF and ADAT inputs
# are muted in the DSP mixer (hardware loopback = 0).
#
# The card is found by its ALSA id (Fireface400), never by index: the index
# moves across boots (issue #453). Every write is a toggle write — a
# different value first, then the target — because ALSA drops a write equal
# to the cached value without notifying snd-fireface-ctl-service, so after a
# JACK or service restart a same-value write never reaches the device while
# `amixer cget` reads it back as set (issue #454). `show` prints that ALSA
# cache, not device state; prove routing and levels by capture.
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
#   ./ff400.sh high     — set output levels to High, input level to Low

set -e

# ── Card by name (issue #453) ────────────────────────────────────────────────
FF400_ID=Fireface400
# Where ALSA cards are listed; overridden only by scripts/ff400_test.sh.
SOUND_SYSFS="${FF400_SOUND_SYSFS:-/sys/class/sound}"
find_card() {
    local dir found=()
    for dir in "$SOUND_SYSFS"/card*; do
        [[ -r "$dir/id" && "$(<"$dir/id")" == "$FF400_ID" ]] && found+=("${dir##*/card}")
    done
    if [[ ${#found[@]} -ne 1 ]]; then
        echo "  card:                ${#found[@]} ALSA cards with id $FF400_ID; nothing written" >&2
        echo "  card:                check: /proc/asound/cards; FF400 power and FireWire cable; snd_fireface loaded" >&2
        exit 1
    fi
    CARD="${found[0]}"
}
find_card

# ── ALSA control width helpers ───────────────────────────────────────────────
# The FF400's array controls (output-volume, analog-source-gain,
# spdif-source-gain, adat-source-gain, stream-source-gain) each carry a values= list whose
# length is fixed by the userspace snd-firewire-ctl-services model
# (runtime/fireface/src/former_ctls.rs, protocols/fireface/src/former/ff400.rs
# upstream) — it does NOT follow sample rate or the JACK port count from
# issue #444 (that's a separate, kernel-side quantity: pcm_capture_channels
# in sound/firewire/fireface/ff.c). Read the width from the control itself
# rather than trusting a literal, since the ctl-service version actually
# installed has not been checked against upstream.
_ctl_width() {
    amixer -c "$CARD" cget numid="$1" 2>/dev/null \
        | grep ': values=' | sed 's/.*values=//' | awk -F, '{print NF}'
}

# Read and validate every array control's width before any mixer write
# (issue #444). A control whose name or index is not the one this script
# expects at that numid, a missing or non-numeric width, or — for stream-source-gain
# — a width too narrow for the row's own index, means the numid layout this
# script assumes does not match the running ctl-service: exit non-zero,
# name the numid, and write nothing.
declare -A CTL_WIDTH
check_ctl_width() {
    local numid="$1" name="$2" index="$3" width header actual actual_index
    header=$(amixer -c "$CARD" cget numid="$numid" 2>/dev/null | grep -m1 "^numid=$numid,") || true
    actual=$(sed -n "s/.*,name='\([^']*\)'.*/\1/p" <<< "$header")
    actual_index=$(sed -n "s/.*',index=\([0-9]*\).*/\1/p" <<< "$header")
    actual_index="${actual_index:-0}"
    if [[ "$actual" != "$name" || "$actual_index" != "$index" ]]; then
        echo "  mixer widths:        numid=$numid is '${actual:-unreadable}' index $actual_index, expected '$name' index $index; nothing written" >&2
        exit 1
    fi
    width=$(_ctl_width "$numid")
    if [[ -z "$width" || ! "$width" =~ ^[0-9]+$ || "$width" -eq 0 ]]; then
        echo "  mixer widths:        could not read value count for numid=$numid ($name); nothing written" >&2
        exit 1
    fi
    CTL_WIDTH[$numid]="$width"
}
check_all_ctl_widths() {
    local numid i
    check_ctl_width 8 "output-volume" 0
    for i in $(seq 0 17); do
        check_ctl_width $((9 + i)) "mixer:analog-source-gain" "$i"
        check_ctl_width $((27 + i)) "mixer:spdif-source-gain" "$i"
        check_ctl_width $((45 + i)) "mixer:adat-source-gain" "$i"
    done
    for i in $(seq 0 17); do
        numid=$((63 + i))
        check_ctl_width "$numid" "mixer:stream-source-gain" "$i"
        if [[ "${CTL_WIDTH[$numid]}" -le "$i" ]]; then
            echo "  mixer widths:        numid=$numid (stream-source-gain) reports width ${CTL_WIDTH[$numid]}, too narrow for index $i; nothing written" >&2
            exit 1
        fi
    done
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

    # jack_lsp -A's exit status must be checked explicitly, both here and
    # below: a process substitution's failure isn't seen by `set -e`, and a
    # pipe into awk without pipefail hides the exit status of jack_lsp behind
    # awk's own success. Either masked failure would let this function print
    # "cleared" / "none set here" while never having actually looked.
    local lsp_out
    if ! lsp_out=$(jack_lsp -A); then
        echo "  JACK aliases:       jack_lsp -A failed after the reachability check; did not clear or confirm any alias" >&2
        exit 1
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
    done <<< "$lsp_out"

    local verify_out
    if ! verify_out=$(jack_lsp -A); then
        echo "  JACK aliases:       jack_lsp -A failed during verification; did not confirm any alias was cleared" >&2
        exit 1
    fi
    local remaining
    remaining=$(awk '
        /^   / { if ($0 ~ /^   FF400:/ && port ~ /^system:(capture|playback)_/) print port ": " $0; next }
        { port = $0 }
    ' <<< "$verify_out")
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

# ── Toggle write (issue #454) ────────────────────────────────────────────────
# cset_toggle NUMID OTHER TARGET: write OTHER, then TARGET. OTHER must differ
# from TARGET in every element, so the second write is a change ALSA passes on
# to the ctl-service whatever the cache held.
cset_toggle() {
    amixer -c "$CARD" cset numid="$1" "$2" >/dev/null
    amixer -c "$CARD" cset numid="$1" "$3" >/dev/null
}
# repeat_csv N VALUE: VALUE repeated N times, comma-separated.
repeat_csv() {
    local out="$2" k
    for ((k = 1; k < $1; k++)); do out+=",$2"; done
    echo "$out"
}

# ── Level mode ────────────────────────────────────────────────────────────────
# line-output-level / headphone-output-level: Item #0 'High'  #1 '-10dBV'  #2 '+4dBu'
# line-input-level:                          Item #0 'Low'   #1 '-10dBV'  #2 '+4dBu'
MODE="${1:-+4dbu}"
case "${MODE,,}" in
    show)
        echo "=== Fireface 400 (card $CARD) current settings ==="
        echo "  (ALSA's cached values, not device state — after a JACK or ctl-service"
        echo "   restart they can differ; prove routing and levels by capture)"
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
                   | python3 -c "import sys; v=sys.stdin.read().strip().split(','); print(v[$idx])" 2>/dev/null) || true
            if [[ -n "$diag" ]]; then
                printf "    ch%02d %s\n" $idx "$diag"
            else
                printf "    ch%02d %s\n" $idx "(could not be read)"
            fi
        done
        echo ""
        clear_ff400_aliases
        exit 0
        ;;
    +4dbu|+4)   LEVEL_IDX=2 ; LEVEL_NAME="+4 dBu"  ; IN_LEVEL_NAME="+4 dBu"  ;;
    -10dbv|-10) LEVEL_IDX=1 ; LEVEL_NAME="-10 dBV" ; IN_LEVEL_NAME="-10 dBV" ;;
    high)       LEVEL_IDX=0 ; LEVEL_NAME="High"     ; IN_LEVEL_NAME="Low"     ;;
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

# ── Validate every array control's width before any mixer write ─────────────
check_all_ctl_widths

# ── Ensure snd-fireface-ctl service is running (bridges ALSA → FireWire hw) ──
#systemctl --user restart snd-fireface-ctl.service
#sleep 1
#echo "  snd-fireface-ctl:   restarted"

# ── Output / input reference levels ──────────────────────────────────────────
OTHER_IDX=$(((LEVEL_IDX + 1) % 3))
cset_toggle 93 $OTHER_IDX $LEVEL_IDX  # line-output-level
cset_toggle 94 $OTHER_IDX $LEVEL_IDX  # headphone-output-level
cset_toggle 89 $OTHER_IDX $LEVEL_IDX  # line-input-level
echo "  output level:       $LEVEL_NAME"
echo "  headphone level:    $LEVEL_NAME"
echo "  input level:        $IN_LEVEL_NAME"

# ── Gain controls ─────────────────────────────────────────────────────────────
cset_toggle 81 1,1 0,0  # mic-input-gain  → 0 dB
cset_toggle 82 1,1 0,0  # line-input-gain → 0 dB
echo "  mic-input-gain:     0 dB"
echo "  line-input-gain:    0 dB"

# ── Input mode ────────────────────────────────────────────────────────────────
# Phantom power (numid=90, mic-1/2-powering) is not touched here: it belongs
# to the rig profile, not to this generic init (issue #444).
cset_toggle 91 on,on off,off  # line-3/4-inst  → off
cset_toggle 92 on,on off,off  # line-3/4-pad   → off
echo "  line-3/4 inst/pad:  off"

# ── Output volume (numid 8, unity = 32768 = 0 dB) ────────────────────────────
cset_toggle 8 "$(repeat_csv "${CTL_WIDTH[8]}" 0)" "$(repeat_csv "${CTL_WIDTH[8]}" 32768)"
echo "  output-volume:      unity (32768) × ${CTL_WIDTH[8]}"

# ── PCM stream → hardware output routing (identity, 32768 = 0 dB) ────────────
# numid 63..80 = mixer:stream-source-gain index 0..17. Width is fixed by the
# ctl-service model (see _ctl_width above), validated for every index by
# check_all_ctl_widths before this loop runs, so index i is always in range.
echo "  stream routing:     identity @ 0 dB (32768)"
for i in $(seq 0 17); do
    numid=$((63 + i))
    width="${CTL_WIDTH[$numid]}"
    vals=$(python3 -c "
n=$width
i=$i
v=[0]*n
v[i]=32768
print(','.join(map(str,v)))
")
    cset_toggle "$numid" "$(repeat_csv "$width" 1)" "$vals"
done

# ── Analog, S/PDIF and ADAT hardware inputs muted in DSP mixer ──────────────
# (all source gains 0 — no hardware loopback; issue #452 added S/PDIF)
# numid 9..26  = mixer:analog-source-gain index 0..17
# numid 27..44 = mixer:spdif-source-gain  index 0..17
# numid 45..62 = mixer:adat-source-gain   index 0..17
# Each row's width is fixed by the ctl-service model (8 analog, 2 S/PDIF,
# 8 ADAT on the installed service — see _ctl_width above) and validated by
# check_all_ctl_widths before this loop. The toggle's other value is 1
# (about −90 dB), so the brief intermediate state is still effectively muted.
for numid in $(seq 9 62); do
    width="${CTL_WIDTH[$numid]}"
    cset_toggle "$numid" "$(repeat_csv "$width" 1)" "$(repeat_csv "$width" 0)"
done
echo "  analog/spdif/adat loopback: muted"

echo ""
echo "Done.  ./ff400.sh show prints what ALSA holds; prove routing and levels by capture."
