#!/usr/bin/env bash
# xrun-soak.sh — silent: hold a capture-only `ac monitor` on the rig's analog
# inputs for N seconds and count xruns, as a record block.
#
#   scripts/rig/xrun-soak.sh <rig> [seconds] [--rev <rev>|latest|installed]
#
# An idle JACK shows no xruns; a load is the point. Does not restart or
# reconfigure JACK. Exit 1 if any xrun, and exit 1 unless the load was held:
# `ac monitor` ended by the timeout after the full N seconds, with the daemon
# and its JACK ports present at the mid-run checkpoint.
#
# Procedure: docs/runbooks/rig-testing.md.

source "$(dirname "$0")/lib.sh"

load_rig "${1:-}"
shift || true
secs=60 rev=latest
while (($#)); do
    case $1 in
        --rev) rev=${2:?}; shift 2 ;;
        [0-9]*) secs=$1; shift ;;
        *) die "unknown argument $1" ;;
    esac
done
[[ $secs =~ ^[0-9]+$ && $secs -ge 10 ]] || die "seconds must be an integer ≥ 10"
dest=""
# See lib.sh's resolve_dest for why resolve_rev's failure must reach this
# shell directly rather than through a nested `rig_dest "$(resolve_rev ...)"`
# (PR #441 QA finding, fifth pass).
if [[ $rev != installed ]]; then
    dest="$(resolve_dest "$rev")"
fi

echo "### xrun soak ($RIG_NAME, ${secs} s, capture-only ac monitor on $RIG_ANALOG_CAPTURES inputs)"
echo
echo "- build: ${dest:-installed /usr/local/bin}"

# xrun_soak_remote.sh is a real file, not an inline heredoc, so the same
# text that runs here also runs under xrun_soak_remote_test.sh's stubs.
remote="set -eu
$REMOTE_USE_BUILD
$(cat "$RIG_SCRIPTS/lib/xrun_soak_remote.sh")"
rig_bash "DEST=$(printf %q "$dest") SECS=$(printf %q "$secs") NCAP=$(printf %q "$RIG_ANALOG_CAPTURES")" "$remote"
