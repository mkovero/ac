#!/usr/bin/env bash
# run-loopback-ir.sh — EMITS. Run the staged `it_loopback_ir` through a real
# port pair on a rig and bring back its record block.
#
#   scripts/rig/run-loopback-ir.sh <rig> --level <dBFS> --consent "<text>"
#       [--rev <rev>|latest] [--route ref|speaker] [--duration 2.0]
#
# --route ref (default): the electrical reference loopback — no speaker.
# --route speaker: speaker out -> mic in. The test's position and SNR
#   assertions were derived for an electrical chain; an acoustic run can fail
#   them for acoustic reasons, so read the record block, not the verdict.
#
# The test spawns its own daemon under an isolated HOME whose config sets no
# drive_max_dbfs, so the daemon's default ceiling applies there, not the
# rig's. This script's --level check is what holds the rig ceiling.
#
# Procedure: docs/runbooks/rig-testing.md.

source "$(dirname "$0")/lib.sh"

load_rig "${1:-}"
shift || true
level="" consent="" rev=latest route=ref duration=2.0
while (($#)); do
    case $1 in
        --level) level=${2:?}; shift 2 ;;
        --consent) consent=${2:?}; shift 2 ;;
        --rev) rev=${2:?}; shift 2 ;;
        --route) route=${2:?}; shift 2 ;;
        --duration) duration=${2:?}; shift 2 ;;
        *) die "unknown argument $1" ;;
    esac
done
require_consent "$consent"
case $route in
    ref)
        out=$RIG_REF_OUT_PORT in=$RIG_REF_IN_PORT
        require_level "$level"
        ;;
    speaker)
        out=$RIG_SPEAKER_OUT_PORT in=$RIG_MIC_IN_PORT
        require_level "$level" "$(speaker_ceiling)"
        ;;
    *) die "--route must be ref or speaker" ;;
esac
require_port_order
rev="$(resolve_rev "$rev")"
dest="$(rig_dest "$rev")"
run="$(rig_run_dir "loopback-ir-$route")"

echo "### it_loopback_ir ($RIG_NAME, route $route)"
echo
echo "- consent: $consent"
echo "- stimulus: Farina sweep 50 Hz–16 kHz, ${duration} s, $level dBFS nominal, $out -> $in"
echo "- build: $dest"
echo "- artefacts on rig: $run"
echo

remote="$(cat <<REMOTE
set -u
$REMOTE_USE_BUILD
cd "\$DEST" && sha256sum --quiet -c SHA256SUMS || { echo "error: staged build fails its SHA256SUMS" >&2; exit 1; }
dp="\$(sed -n 's/^it_loopback_ir_daemon_path=//p' MANIFEST.txt)"
[[ "\$(readlink -f "\$dp")" == "\$DEST/ac-daemon" ]] || { echo "error: \$dp does not resolve to \$DEST/ac-daemon — rerun ship.sh" >&2; exit 1; }
mkdir -p "\$RUN" && cd "\$RUN"
AC_LOOPBACK_OUT="\$OUT" AC_LOOPBACK_IN="\$IN" AC_LOOPBACK_LEVEL_DBFS="\$LEVEL" AC_LOOPBACK_DURATION_S="\$DUR" \
    "\$DEST/it_loopback_ir" --ignored --nocapture --test-threads=1 >run.log 2>&1
status=\$?
echo '\`\`\`'
cat run.log
echo '\`\`\`'
echo
echo "it_loopback_ir exit status: \$status"
exit \$status
REMOTE
)"
rig_bash "DEST=$(printf %q "$dest") RUN=$(printf %q "$run") LEVEL=$(printf %q "$level") \
DUR=$(printf %q "$duration") OUT=$(printf %q "$out") IN=$(printf %q "$in")" "$remote"
