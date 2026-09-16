#!/usr/bin/env bash
# acoustic_ir_remote_test.sh — regression test for
# scripts/rig/lib/acoustic_ir_remote.sh, the routing + ir_probe block
# acoustic-ir.sh runs on the rig. No rig, no JACK, no real ac-daemon: stub
# `ac` on PATH and a stub `ir_probe` in a fake staged build, and the exact
# same assembly (set -eu + REMOTE_USE_BUILD + the file's own text)
# acoustic-ir.sh sends over ssh, run locally with `bash -c` instead.
#
#   bash scripts/rig/acoustic_ir_remote_test.sh
#
# PR #441 codex-qa finding this covers (35ddb7a1): `ac setup`'s status was
# discarded and the route never read back, so ir_probe could emit through a
# previous run's output/input while the record named the requested ones.
# A failed setup and a lying one (exit 0, config not updated) must each stop
# before ir_probe runs and say so in the output; the ok path must still run.
# A lying setup must also be caught when the channels already match but a
# sticky output_port remains, since the daemon routes by that port first
# (PR #441 QA, 5982e3e5).
#
# Procedure these scripts implement: docs/runbooks/rig-testing.md.

source "$(dirname "$0")/lib.sh"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
bindir="$tmp/bin"
homedir="$tmp/home"
dest="$tmp/stage"
mkdir -p "$bindir" "$homedir/.config/ac" "$dest"

cat >"$bindir/ac" <<'EOF'
#!/usr/bin/env bash
# Stub `ac setup output <o> input <i>`. STUB_SETUP_MODE: ok (updates the
# config and clears the sticky ports, as the daemon's setup does) |
# fail (exit 1) | lie (exit 0, config left untouched).
cfg="$HOME/.config/ac/config.json"
if [[ $1 == setup && $2 == output && $4 == input ]]; then
    case "${STUB_SETUP_MODE:-ok}" in
        fail) echo "error: no reply from daemon" >&2; exit 1 ;;
        lie) exit 0 ;;
        *) python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
d["output_channel"] = int(sys.argv[2])
d["input_channel"] = int(sys.argv[3])
d["output_port"] = None
d["input_port"] = None
json.dump(d, open(sys.argv[1], "w"))
' "$cfg" "$3" "$5" ;;
    esac
fi
exit 0
EOF
chmod +x "$bindir/ac"

# REMOTE_USE_BUILD and cleanup pkill/pgrep ac-daemon by name — stubbed so a
# test run never touches a real daemon on the machine running it.
printf '#!/bin/sh\nexit 1\n' >"$bindir/pkill"
printf '#!/bin/sh\nexit 1\n' >"$bindir/pgrep"
chmod +x "$bindir/pkill" "$bindir/pgrep"

# ir_probe lives in the staged build (called as "$DEST/ir_probe") and leaves a
# marker, so "did anything emit" is a file check, not an output grep.
cat >"$dest/ir_probe" <<EOF
#!/usr/bin/env bash
: >"$tmp/ir_probe-ran"
echo "stub ir_probe: peak ok"
EOF
chmod +x "$dest/ir_probe"
(cd "$dest" && sha256sum ir_probe >SHA256SUMS)

# run_ir <setup mode> <starting config.json>
run_ir() {
    rm -rf "$tmp/run" "$tmp/ir_probe-ran"
    printf '%s\n' "$2" >"$homedir/.config/ac/config.json"
    HOME="$homedir" PATH="$bindir:$PATH" STUB_SETUP_MODE="$1" \
        DEST="$dest" RUN="$tmp/run" LEVEL=-60 DUR=2.0 F1=50 F2=16000 WIN=16384 TAU="" SPK=0 MIC=0 \
        bash -c "set -eu
$REMOTE_USE_BUILD
$(cat "$(dirname "$0")/lib/acoustic_ir_remote.sh")"
}

fail() { echo "FAIL: $1"; exit 1; }

# Requests 0/0 while the config starts at 1/1 (a previous run's route), so
# "lie" leaves a real disagreement for the read-back to catch. The key set
# matches what the daemon writes (ports serialise as null).
prev_route='{"output_channel": 1, "input_channel": 1, "output_port": null, "input_port": null}'
# Channels already match the request, but an earlier manual run pinned an
# output port — resolve_output routes by that port first.
sticky_port='{"output_channel": 0, "input_channel": 0, "output_port": "system:playback_9", "input_port": null}'

out="$(run_ir ok "$prev_route" 2>&1)" && rc=0 || rc=$?
[[ $rc == 0 ]] || fail "setup=ok: expected exit 0, got $rc — output: $out"
[[ -f $tmp/ir_probe-ran ]] || fail "setup=ok: ir_probe should have run"
[[ $out == *"config reads output 0 / input 0, no sticky port, as requested"* ]] || fail "setup=ok: route not recorded — output: $out"
[[ $out == *"stub ir_probe: peak ok"* ]] || fail "setup=ok: ir_probe output missing from the record"
echo "setup=ok: route read back, ir_probe runs, exit 0"

out="$(run_ir fail "$prev_route" 2>&1)" && rc=0 || rc=$?
[[ $rc != 0 ]] || fail "setup=fail: expected nonzero exit"
[[ -f $tmp/ir_probe-ran ]] && fail "setup=fail: ir_probe must not run"
[[ $out == *"routing: REFUSED"*"exited 1"* ]] || fail "setup=fail: refusal missing from the record — output: $out"
[[ $out == *"no reply from daemon"* ]] || fail "setup=fail: setup's own error missing from the record"
echo "setup=fail: refuses before ir_probe, exit $rc"

out="$(run_ir lie "$prev_route" 2>&1)" && rc=0 || rc=$?
[[ $rc != 0 ]] || fail "setup=lie: expected nonzero exit"
[[ -f $tmp/ir_probe-ran ]] && fail "setup=lie: ir_probe must not run"
[[ $out == *"routing: REFUSED — requested output 0 / input 0, config reads output 1 / input 1"* ]] ||
    fail "setup=lie: read-back refusal missing from the record — output: $out"
echo "setup=lie: caught by the config read-back, exit $rc"

out="$(run_ir ok "$sticky_port" 2>&1)" && rc=0 || rc=$?
[[ $rc == 0 ]] || fail "sticky port, setup=ok: an applied setup clears the port — expected exit 0, got $rc — output: $out"
[[ -f $tmp/ir_probe-ran ]] || fail "sticky port, setup=ok: ir_probe should have run"
echo "sticky output_port, setup=ok: cleared by setup, ir_probe runs, exit 0"

out="$(run_ir lie "$sticky_port" 2>&1)" && rc=0 || rc=$?
[[ $rc != 0 ]] || fail "sticky port, setup=lie: expected nonzero exit — output: $out"
[[ -f $tmp/ir_probe-ran ]] && fail "sticky port, setup=lie: ir_probe must not run"
[[ $out == *"routing: REFUSED"*"config still pins output_port system:playback_9 / input_port None"* ]] ||
    fail "sticky port, setup=lie: port refusal missing from the record — output: $out"
echo "sticky output_port with matching channels, setup=lie: refused, exit $rc"

echo "acoustic_ir_remote.sh: all cases as expected"
