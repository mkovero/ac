#!/usr/bin/env bash
# noise-snapshot.sh — silent: record the measurement mic and print broadband
# and band levels, as a record block. Run at the start of every acoustic
# session; room noise varies a lot between sessions.
#
#   scripts/rig/noise-snapshot.sh <rig> [seconds]   (default 5)
#
# The WAV stays on the rig under <stage_base>/noise/.
# Procedure: docs/runbooks/rig-testing.md.

source "$(dirname "$0")/lib.sh"

load_rig "${1:-}"
secs=${2:-5}
[[ $secs =~ ^[0-9]+$ ]] || die "seconds must be an integer"

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
wav="$RIG_STAGE_BASE/noise/noise_$stamp.wav"

header="$(rig_ssh "mkdir -p '$RIG_STAGE_BASE/noise' && \
jack_rec -f '$wav' -d $secs -b 24 '$RIG_MIC_IN_PORT' >/dev/null 2>&1 && \
echo \"rate=\$(jack_samplerate) period=\$(jack_bufsize) \
mic_gain=\$(amixer -c '$RIG_ALSA_CARD' cget numid=$RIG_MIC_GAIN_NUMID | sed -n 's/.*: values=//p') \
phantom=\$(amixer -c '$RIG_ALSA_CARD' cget numid=$RIG_PHANTOM_NUMID | sed -n 's/.*: values=//p')\"")" ||
    die "recording failed on $RIG_NAME"

echo "### noise snapshot ($RIG_NAME, $stamp, ${secs} s, $RIG_MIC_IN_PORT)"
echo
echo "$header; file $wav"
echo
echo '```'
rig_ssh "python3 - '$wav' 0" <"$RIG_SCRIPTS/lib/bands.py"
echo '```'
