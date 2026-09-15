#!/usr/bin/env bash
# lib.sh — shared by scripts/rig/*. Source, do not execute.
#
# Procedure these scripts implement: docs/runbooks/rig-testing.md.

set -euo pipefail

RIG_SCRIPTS="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

die() {
    echo "error: $*" >&2
    exit 1
}

note() { echo "== $*" >&2; }

# $AC_HOME, resolved the same way bin/common.sh does: beside the MAIN
# checkout, from anywhere — including inside a linked worktree.
ac_home() {
    if [[ -n ${AC_HOME:-} ]]; then
        echo "$AC_HOME"
        return
    fi
    local common
    common="$(git rev-parse --path-format=absolute --git-common-dir)"
    echo "$(dirname "$(dirname "$common")")/ac-wt"
}

# Where build-portable.sh stages binaries and ship.sh reads them from.
# Named target-* so $AC_HOME's .gitignore (`target-*/`) keeps binaries out of
# that repo.
stage_root() { echo "$(ac_home)/target-rig-stage"; }

# load_rig <name> — source scripts/rig/hosts/<name>.env, then the private
# $AC_HOME/rig-hosts/<name>.access.env (address, user, SSH key).
load_rig() {
    local name=${1:-}
    [[ -n $name ]] || die "rig name required — one of: $(ls "$RIG_SCRIPTS/hosts" | sed 's/\.env$//' | tr '\n' ' ')"
    local f="$RIG_SCRIPTS/hosts/$name.env"
    [[ -f $f ]] || die "no host profile $f"
    # shellcheck source=/dev/null
    source "$f"
    RIG_NAME=$name
    local access
    access="$(ac_home)/rig-hosts/$name.access.env"
    [[ -f $access ]] || die "no access file $access — it sets RIG_HOST, RIG_USER and RIG_SSH_KEY"
    # shellcheck source=/dev/null
    source "$access"
    : "${RIG_HOST:?$access must set RIG_HOST}" "${RIG_USER:?$access must set RIG_USER}" \
        "${RIG_SSH_KEY:?$access must set RIG_SSH_KEY}" "${RIG_STAGE_BASE:?$f must set RIG_STAGE_BASE}"
}

rig_ssh() {
    ssh -F /dev/null -o BatchMode=yes -o ConnectTimeout=10 -i "$RIG_SSH_KEY" \
        "$RIG_USER@$RIG_HOST" "$@"
}

rig_scp() {
    scp -F /dev/null -o BatchMode=yes -o ConnectTimeout=10 -i "$RIG_SSH_KEY" "$@"
}

# rig_bash "<VAR=value ...>" "<script>" — run a bash script on the rig with
# stdin closed. The script travels as a `bash -c` argument, not on stdin to
# `bash -s`: anything in it that reads stdin (`script`, `ac`'s key handling)
# would otherwise swallow the rest of the script and exit 0 having run
# nothing.
rig_bash() {
    ssh -n -F /dev/null -o BatchMode=yes -o ConnectTimeout=10 -i "$RIG_SSH_KEY" \
        "$RIG_USER@$RIG_HOST" "$1 bash -c $(printf %q "$2")"
}

# resolve_rev <rev|latest> — a directory name under stage_root.
resolve_rev() {
    local want=${1:-latest} root
    root="$(stage_root)"
    if [[ $want == latest ]]; then
        want="$(ls -1t "$root" 2>/dev/null | head -1)"
        [[ -n $want ]] || die "nothing staged under $root — run build-portable.sh first"
    fi
    [[ -f $root/$want/SHA256SUMS ]] || die "no staged build $root/$want (missing SHA256SUMS)"
    echo "$want"
}

# build_compiled_this_run <log-file>... — did any workspace crate (all
# package names start "ac-": ac-cli, ac-core, ac-daemon, ac-scene, ac-view)
# actually compile, per the given cargo build logs. Shared by
# build-portable.sh and build_portable_test.sh so the two can't drift apart
# (PR #441 QA: an earlier version of the test kept its own copy of this
# pattern, which would stay green if the production grep were narrowed back).
build_compiled_this_run() {
    grep -qE '^\s*Compiling ac-[a-z]+ ' "$@" && echo yes || echo no
}

# Remote directory a staged revision is shipped to.
rig_dest() { echo "$RIG_STAGE_BASE/$1-x86_64"; }

# rig_run_dir <label> — a fresh remote directory for one run's artefacts.
rig_run_dir() { echo "$RIG_STAGE_BASE/runs/$(date -u +%Y%m%dT%H%M%SZ)-$1"; }

# push_helpers — copy scripts/rig/lib/*.py to <stage_base>/lib on the rig.
push_helpers() {
    rig_ssh "mkdir -p '$RIG_STAGE_BASE/lib'"
    rig_scp "$RIG_SCRIPTS"/lib/*.py "$RIG_USER@$RIG_HOST:$RIG_STAGE_BASE/lib/" >/dev/null
}

# rig_port_order — silent: record 1 s of every system capture port and check
# the analog block sits at RIG_ANALOG_CAPTURE_FIRST (see lib/port_order.py).
# Prints the finding; returns non-zero when the order does not match.
rig_port_order() {
    push_helpers
    # shellcheck disable=SC2016  # expanded on the rig
    rig_bash "FIRST=$(printf %q "$RIG_ANALOG_CAPTURE_FIRST") NA=$(printf %q "$RIG_ANALOG_CAPTURES") \
LIB=$(printf %q "$RIG_STAGE_BASE/lib")" '
n=$(jack_lsp | grep -c "^system:capture_")
f=$(mktemp --suffix=.wav)
ports=""; for i in $(seq 1 "$n"); do ports="$ports system:capture_$i"; done
jack_rec -f "$f" -d 1 -b 24 $ports >/dev/null 2>&1
python3 "$LIB/port_order.py" "$f" "$FIRST" "$NA"
s=$?
rm -f "$f"
exit $s'
}

# require_port_order — die unless the analog block is where the profile says.
require_port_order() {
    local out
    if ! out="$(rig_port_order)"; then
        echo "$out" >&2
        die "$RIG_NAME's JACK port order does not match its profile — the ports this script would drive are not the ones the profile names. Nothing was emitted."
    fi
    echo "- $out" | tail -1
}

# require_level <dbfs> [ceiling] — refuse anything above the given ceiling
# (default: the rig's standing ceiling).
require_level() {
    local level=${1:-} ceiling=${2:-$RIG_DRIVE_CEILING_DBFS}
    [[ -n $level ]] || die "--level <dBFS> is required for anything that emits"
    python3 -c 'import sys; sys.exit(0 if float(sys.argv[1]) <= float(sys.argv[2]) else 1)' \
        "$level" "$ceiling" ||
        die "level $level dBFS is above $RIG_NAME's ceiling of $ceiling dBFS for this path"
}

# speaker_ceiling — the ceiling for anything that drives the speaker output:
# RIG_SPEAKER_CEILING_DBFS when the profile sets one, else the standing one.
speaker_ceiling() { echo "${RIG_SPEAKER_CEILING_DBFS:-$RIG_DRIVE_CEILING_DBFS}"; }

# require_consent <text> — emission needs the operator's per-run consent,
# stated in words that go into the output and from there into the record.
require_consent() {
    [[ -n ${1:-} ]] || die "--consent \"<who consented to what, when>\" is required for anything that emits (see .agents/rig.md hard constraints)"
}

# Remote bash fragment: use the staged binaries in $DEST (or the installed
# ones when $DEST is empty), make sure no other ac-daemon is running, and
# define `daemon_identity`, which prints the running daemon's executable.
# `ac` looks up ac-daemon on PATH before its own directory, so without the
# PATH prefix a staged `ac` silently auto-spawns the installed daemon.
# shellcheck disable=SC2016,SC2034  # expanded into remote heredocs by the sourcing scripts
REMOTE_USE_BUILD='
if [[ -n $DEST ]]; then export PATH="$DEST:$PATH"; fi
pkill -x ac-daemon 2>/dev/null || true
for _i in 1 2 3 4 5; do pgrep -x ac-daemon >/dev/null || break; sleep 1; done
if pgrep -x ac-daemon >/dev/null; then echo "error: an ac-daemon would not stop" >&2; exit 1; fi
daemon_identity() {
    local p; p="$(pgrep -xn ac-daemon)" || { echo "none"; return; }
    readlink -f "/proc/$p/exe"
}
'
