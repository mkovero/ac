#!/usr/bin/env bash
# acoustic_ir_remote.sh — the routing + `ir_probe` block acoustic-ir.sh runs
# on the rig. A real file, not an inline heredoc, so acoustic_ir_remote_test.sh
# runs this exact text locally against stub `ac`/`ir_probe` (same pattern as
# probe_remote.sh).
#
# Fails closed on routing (PR #441 codex-qa finding, 35ddb7a1): a failed
# `ac setup`, or one that exits 0 while the persisted config does not hold the
# requested output/input, stops the run before `ir_probe` emits, and says so
# in the record. The daemon saves the config before it acknowledges `setup`,
# so the file is the state `ir_probe`'s daemon routes by.
#
# The daemon routes by a sticky `output_port` / `input_port` before the
# channel index (resolve_output / resolve_input in
# ac-daemon/src/handlers/mod.rs), and only an applied `setup` clears them
# (handlers/admin.rs). So the read-back also requires both ports to be null:
# a lost setup over a config whose channels already match would otherwise
# pass while the run emits through a stale pinned port (PR #441 QA, 5982e3e5).
#
# Expects on entry (set by acoustic-ir.sh via rig_bash, or by the test):
# DEST RUN LEVEL DUR F1 F2 WIN TAU SPK MIC, and REMOTE_USE_BUILD (lib.sh)
# already run ahead of this file, defining daemon_identity().
#
# Not meant to be executed directly — acoustic-ir.sh concatenates
# REMOTE_USE_BUILD with this file's content and passes the result to
# `bash -c` over ssh.

set -eu

cd "$DEST" && sha256sum --quiet -c SHA256SUMS || { echo "error: staged build fails its SHA256SUMS" >&2; exit 1; }
mkdir -p "$RUN" && cd "$RUN"
cfg="$HOME/.config/ac/config.json"
cfg_key() {
    python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get(sys.argv[2]))' "$cfg" "$1"
}
orig_out="$(cfg_key output_channel)"
orig_in="$(cfg_key input_channel)"
cleanup() {
    # set -e applies inside a trap too — each command must survive on its own.
    ac setup output "$orig_out" input "$orig_in" >/dev/null 2>&1 || true
    pkill -x ac-daemon 2>/dev/null || true
}
trap cleanup EXIT

refuse_routing() {
    echo "- routing: REFUSED — $1. Nothing was emitted."
    echo
    echo '```'
    cat setup.log
    echo '```'
    exit 1
}

setup_status=0
ac setup output "$SPK" input "$MIC" >setup.log 2>&1 || setup_status=$?
[[ $setup_status == 0 ]] || refuse_routing "\`ac setup output $SPK input $MIC\` exited $setup_status"
# Read back immediately before emission: a lost or wrong acknowledgement can
# exit 0 while the persisted route is still a previous run's.
got_out="$(cfg_key output_channel)"
got_in="$(cfg_key input_channel)"
got_out_port="$(cfg_key output_port)"
got_in_port="$(cfg_key input_port)"
[[ $got_out == "$SPK" && $got_in == "$MIC" ]] ||
    refuse_routing "requested output $SPK / input $MIC, config reads output $got_out / input $got_in"
[[ $got_out_port == None && $got_in_port == None ]] ||
    refuse_routing "requested output $SPK / input $MIC, but config still pins output_port $got_out_port / input_port $got_in_port, which the daemon routes by first"
echo "- routing: config reads output $got_out / input $got_in, no sticky port, as requested"
echo "- daemon executable: $(daemon_identity)"
status=0
# shellcheck disable=SC2086  # TAU is either empty or "--tau-ms <ms>"
"$DEST/ir_probe" --level-dbfs "$LEVEL" --duration "$DUR" --f1 "$F1" --f2 "$F2" --window "$WIN" $TAU >run.log 2>&1 || status=$?
echo
echo '```'
cat run.log
echo '```'
echo
echo "ir_probe exit status: $status"
exit "$status"
