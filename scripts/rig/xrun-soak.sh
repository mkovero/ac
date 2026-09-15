#!/usr/bin/env bash
# xrun-soak.sh — silent: hold a capture-only `ac monitor` on the rig's analog
# inputs for N seconds and count xruns, as a record block.
#
#   scripts/rig/xrun-soak.sh <rig> [seconds] [--rev <rev>|latest|installed]
#
# An idle JACK shows no xruns; a load is the point. Does not restart or
# reconfigure JACK. Exit 1 if any xrun.
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
# See preflight.sh for why this must be split rather than nested: resolve_rev's
# exit 1 would otherwise only kill the inner command substitution, and
# rig_dest's own (always-0) exit status is what `set -e` would see (PR #441
# QA finding, fifth pass).
if [[ $rev != installed ]]; then
    rev="$(resolve_rev "$rev")" || exit 1
    dest="$(rig_dest "$rev")"
fi

echo "### xrun soak ($RIG_NAME, ${secs} s, capture-only ac monitor on $RIG_ANALOG_CAPTURES inputs)"
echo
echo "- build: ${dest:-installed /usr/local/bin}"

remote="$(cat <<REMOTE
set -u
$REMOTE_USE_BUILD
trap 'ac stop >/dev/null 2>&1; pkill -x ac-daemon 2>/dev/null' EXIT
ts="\$(mktemp)"
t0=\$(date +%s)
( sleep \$((SECS / 2))
  echo "- mid-run: daemon \$(daemon_identity), \$(jack_lsp | grep -c '^ac-daemon') ac-daemon JACK ports" >"\$ts.mid" ) &
script -qc "timeout \$SECS ac monitor 0-\$((NCAP - 1)) --tui" "\$ts" >/dev/null 2>&1
ac stop >/dev/null 2>&1
wait
if jlog=\$(journalctl --since "@\$t0" --no-pager 2>&1); then
    log=\$(grep -ciE 'jackd.*xrun' <<<"\$jlog")
else
    log=unreadable
fi
daemon=\$(tr -d '\033' <"\$ts" | grep -o 'xruns=[0-9]*' | tail -1)
cat "\$ts.mid"
echo "- JACK: \$(jack_samplerate) Hz, \$(jack_bufsize) frames, jackd: \$(pgrep -ax jackd | cut -d' ' -f2-)"
echo "- jackd xrun log lines: \$log"
echo "- daemon-side counter: \${daemon:-not captured}"
rm -f "\$ts" "\$ts.mid"
[[ \$log == 0 && \${daemon#xruns=} == 0 ]]
REMOTE
)"
rig_bash "DEST=$(printf %q "$dest") SECS=$(printf %q "$secs") NCAP=$(printf %q "$RIG_ANALOG_CAPTURES")" "$remote"
