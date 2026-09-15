#!/usr/bin/env bash
# acoustic-ir.sh — EMITS through the speaker. One headless `plot_ir` via the
# staged `ir_probe`: speaker out -> mic in, reporting peak, floor, SNR, offset
# from window centre and onset.
#
#   scripts/rig/acoustic-ir.sh <rig> --level <dBFS> --consent "<text>"
#       [--rev <rev>|latest] [--duration 2.0] [--f1 50] [--f2 16000]
#       [--window 16384] [--tau-ms <ms>] [--mic-position "<where>"]
#
# Uses a daemon spawned from the staged build (identity printed), routed with
# `ac setup` to the rig's speaker and mic indices; that daemon's config
# ceiling applies as well as this script's --level check, which holds the
# profile's speaker ceiling (RIG_SPEAKER_CEILING_DBFS). The ac config's
# channels are restored on exit.
#
# Procedure: docs/runbooks/rig-testing.md.

source "$(dirname "$0")/lib.sh"

load_rig "${1:-}"
shift || true
level="" consent="" rev=latest duration=2.0 f1=50 f2=16000 window=16384 tau="" pos="not stated"
while (($#)); do
    case $1 in
        --level) level=${2:?}; shift 2 ;;
        --consent) consent=${2:?}; shift 2 ;;
        --rev) rev=${2:?}; shift 2 ;;
        --duration) duration=${2:?}; shift 2 ;;
        --f1) f1=${2:?}; shift 2 ;;
        --f2) f2=${2:?}; shift 2 ;;
        --window) window=${2:?}; shift 2 ;;
        --tau-ms) tau=${2:?}; shift 2 ;;
        --mic-position) pos=${2:?}; shift 2 ;;
        *) die "unknown argument $1" ;;
    esac
done
require_consent "$consent"
require_level "$level" "$(speaker_ceiling)"
require_port_order
rev="$(resolve_rev "$rev")"
dest="$(rig_dest "$rev")"
run="$(rig_run_dir acoustic-ir)"

echo "### acoustic IR ($RIG_NAME)"
echo
echo "- consent: $consent"
echo "- stimulus: Farina sweep ${f1}–${f2} Hz, ${duration} s, $level dBFS nominal, $RIG_SPEAKER_OUT_PORT -> $RIG_MIC_IN_PORT"
echo "- mic position: $pos"
echo "- build: $dest"
echo "- artefacts on rig: $run"
echo

tau_arg=""
[[ -n $tau ]] && tau_arg="--tau-ms $tau"

remote="$(cat <<REMOTE
set -u
$REMOTE_USE_BUILD
cd "\$DEST" && sha256sum --quiet -c SHA256SUMS || { echo "error: staged build fails its SHA256SUMS" >&2; exit 1; }
mkdir -p "\$RUN" && cd "\$RUN"
cfg="\$HOME/.config/ac/config.json"
orig_out="\$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["output_channel"])' "\$cfg")"
orig_in="\$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["input_channel"])' "\$cfg")"
cleanup() {
    ac setup output "\$orig_out" input "\$orig_in" >/dev/null 2>&1
    pkill -x ac-daemon 2>/dev/null
}
trap cleanup EXIT
ac setup output "\$SPK" input "\$MIC" >setup.log 2>&1
echo "- daemon executable: \$(daemon_identity)"
echo "- drive_max_dbfs in that daemon's config: \$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("drive_max_dbfs"))' "\$cfg")"
"\$DEST/ir_probe" --level-dbfs "\$LEVEL" --duration "\$DUR" --f1 "\$F1" --f2 "\$F2" --window "\$WIN" \$TAU >run.log 2>&1
status=\$?
echo
echo '\`\`\`'
cat run.log
echo '\`\`\`'
echo
echo "ir_probe exit status: \$status"
exit \$status
REMOTE
)"
rig_bash "DEST=$(printf %q "$dest") RUN=$(printf %q "$run") LEVEL=$(printf %q "$level") \
DUR=$(printf %q "$duration") F1=$(printf %q "$f1") F2=$(printf %q "$f2") WIN=$(printf %q "$window") \
TAU=$(printf %q "$tau_arg") SPK=$(printf %q "$RIG_SPEAKER_OUT_INDEX") MIC=$(printf %q "$RIG_MIC_IN_INDEX")" "$remote"
