#!/usr/bin/env bash
# probe-outputs.sh — EMITS. Verify wiring: drive one output at a time with a
# bounded tone and report which analog capture sees it.
#
#   scripts/rig/probe-outputs.sh <rig> --level <dBFS> --consent "<text>"
#       [--rev <rev>|latest|installed] [--outputs "0 1"] [--seconds 3] [--freq 1000]
#
# Run after any cable work, before trusting a stated layout. Use a low level
# (−60 dBFS reads about −64 dBFS at a 1 m mic on pupu). Each tone is an
# `ac generate level`, which the daemon ends itself after --seconds.
# The ac config's output/input channels are restored on exit.
#
# Procedure: docs/runbooks/rig-testing.md.

source "$(dirname "$0")/lib.sh"

load_rig "${1:-}"
shift || true
level="" consent="" rev=latest seconds=3 freq=1000
outputs="$RIG_SPEAKER_OUT_INDEX $RIG_REF_OUT_INDEX"
while (($#)); do
    case $1 in
        --level) level=${2:?}; shift 2 ;;
        --consent) consent=${2:?}; shift 2 ;;
        --rev) rev=${2:?}; shift 2 ;;
        --outputs) outputs=${2:?}; shift 2 ;;
        --seconds) seconds=${2:?}; shift 2 ;;
        --freq) freq=${2:?}; shift 2 ;;
        *) die "unknown argument $1" ;;
    esac
done
require_consent "$consent"
ceiling=$RIG_DRIVE_CEILING_DBFS
for o in $outputs; do
    [[ $o == "$RIG_SPEAKER_OUT_INDEX" ]] && ceiling="$(speaker_ceiling)"
done
require_level "$level" "$ceiling"
[[ $seconds =~ ^[0-9]+$ && $seconds -ge 3 && $seconds -le 10 ]] || die "--seconds must be an integer 3..10"

dest=""
if [[ $rev != installed ]]; then dest="$(rig_dest "$(resolve_rev "$rev")")"; fi
run="$(rig_run_dir probe-outputs)"
push_helpers

echo "### wiring probe ($RIG_NAME)"
echo
echo "- consent: $consent"
echo "- stimulus: ${freq} Hz sine at $level dBFS nominal, ${seconds} s per output, outputs: $outputs"
echo "- build: ${dest:-installed /usr/local/bin}"
echo "- artefacts on rig: $run"
echo

remote="$(cat <<REMOTE
set -u
$REMOTE_USE_BUILD
mkdir -p "\$RUN" && cd "\$RUN"
cfg="\$HOME/.config/ac/config.json"
orig_out="\$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["output_channel"])' "\$cfg")"
orig_in="\$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["input_channel"])' "\$cfg")"
cleanup() {
    ac stop >/dev/null 2>&1
    ac setup output "\$orig_out" input "\$orig_in" >/dev/null 2>&1
    pkill -x ac-daemon 2>/dev/null
}
trap cleanup EXIT
ac setup output "\${OUTS%% *}" >/dev/null 2>&1
echo "- daemon executable: \$(daemon_identity)"
caps=""; for i in \$(seq 1 "\$NCAP"); do caps="\$caps system:capture_\$i"; done
rec=\$((SECS - 2))
for ch in \$OUTS; do
    ac setup output "\$ch" >/dev/null 2>&1
    ac generate level "\$LEVEL" "\$LEVEL" "\${FREQ}hz" "\${SECS}s" >"gen_\$ch.log" 2>&1 &
    sleep 0.7
    jack_rec -f "probe_out\$ch.wav" -d "\$rec" -b 24 \$caps >/dev/null 2>&1
    wait
    ac stop >/dev/null 2>&1
    echo
    echo "#### ac output \$ch — \$(grep -o 'system:playback_[0-9]*' "gen_\$ch.log" | head -1)"
    echo
    python3 "\$LIB/chan_levels.py" "probe_out\$ch.wav" "\$FREQ" | tail -n +2
done
REMOTE
)"
rig_bash "DEST=$(printf %q "$dest") RUN=$(printf %q "$run") LEVEL=$(printf %q "$level") \
SECS=$(printf %q "$seconds") FREQ=$(printf %q "$freq") OUTS=$(printf %q "$outputs") \
NCAP=$(printf %q "$RIG_ANALOG_CAPTURES") LIB=$(printf %q "$RIG_STAGE_BASE/lib")" "$remote"
