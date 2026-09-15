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
require_port_order
[[ $seconds =~ ^[0-9]+$ && $seconds -ge 3 && $seconds -le 10 ]] || die "--seconds must be an integer 3..10"

dest=""
# See lib.sh's resolve_dest for why resolve_rev's failure must reach this
# shell directly rather than through a nested `rig_dest "$(resolve_rev ...)"`
# (PR #441 QA finding, fifth pass). Matters more here: this is an EMIT
# script, so an unresolved rev falling through silently would drive real
# audio through whatever is installed instead of refusing.
if [[ $rev != installed ]]; then
    dest="$(resolve_dest "$rev")"
fi
run="$(rig_run_dir probe-outputs)"
push_helpers

echo "### wiring probe ($RIG_NAME)"
echo
echo "- consent: $consent"
echo "- stimulus: ${freq} Hz sine at $level dBFS nominal, ${seconds} s per output, outputs: $outputs"
echo "- build: ${dest:-installed /usr/local/bin}"
echo "- artefacts on rig: $run"
echo

# probe_remote.sh is a real file, not an inline heredoc, so the same text
# that runs here also runs under probe_outputs_remote_test.sh's stubs —
# see that file's header for why. set -e (upgraded from the sibling
# scripts' set -u) so a failed ac setup/generate/jack_rec stops the loop
# instead of falling through to the next command with nothing checking it.
remote="set -eu
$REMOTE_USE_BUILD
$(cat "$RIG_SCRIPTS/lib/probe_remote.sh")"
rig_bash "DEST=$(printf %q "$dest") RUN=$(printf %q "$run") LEVEL=$(printf %q "$level") \
SECS=$(printf %q "$seconds") FREQ=$(printf %q "$freq") OUTS=$(printf %q "$outputs") \
NCAP=$(printf %q "$RIG_ANALOG_CAPTURES") LIB=$(printf %q "$RIG_STAGE_BASE/lib")" "$remote"
