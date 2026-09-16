#!/usr/bin/env bash
# lib_test.sh — regression test for lib.sh's require_level/speaker_ceiling
# (the two coupled ceiling constants) and resolve_rev/resolve_dest, plus the
# three scripts that call resolve_dest, each run with ssh/scp stubbed. No rig
# needed.
#
#   bash scripts/rig/lib_test.sh
#
# Procedure these scripts implement: docs/runbooks/rig-testing.md.

source "$(dirname "$0")/lib.sh"

# shellcheck disable=SC2034  # read by die()/speaker_ceiling() in lib.sh
RIG_NAME=lib-test-rig
RIG_DRIVE_CEILING_DBFS=-40
# shellcheck disable=SC2034  # read by speaker_ceiling() in lib.sh
RIG_SPEAKER_CEILING_DBFS=-50
# shellcheck disable=SC2034  # read by rig_dest() in lib.sh. Set here (not
# left unset like the rest of load_rig's state) so the resolve_dest test
# below exercises the real bug: with this unset, rig_dest itself dies on
# `set -u` before a broken (unguarded) resolve_dest could silently succeed,
# which would let that test pass for the wrong reason.
RIG_STAGE_BASE=/rig-stage-lib-test

assert_refuses() {
    # require_level dies (hard `exit`) on refusal; a subshell keeps that
    # from killing this script, since `&&`/`||` alone do not — set -e
    # exempts the consequents of a list, but exit() is unconditional.
    if (require_level "$1" "$2") 2>/dev/null; then
        echo "FAIL: $1 should be refused at ceiling $2"
        exit 1
    fi
}
assert_allows() {
    (require_level "$1" "$2") 2>/dev/null || { echo "FAIL: $1 should be allowed at ceiling $2"; exit 1; }
}

assert_allows -40 "$RIG_DRIVE_CEILING_DBFS"    # at the standing ceiling
assert_refuses -39 "$RIG_DRIVE_CEILING_DBFS"   # 1 dB over
assert_allows -50 "$(speaker_ceiling)"         # at the speaker ceiling
assert_refuses -45 "$(speaker_ceiling)"        # between the two ceilings — the case #445/PR #441 cares about
assert_allows -60 "$(speaker_ceiling)"         # well under

echo "lib.sh require_level: all cases as expected"

# resolve_rev must fail closed on a bogus/missing revision — this is the
# guarantee preflight.sh's `rev != none` branch relies on (PR #441 QA
# finding: preflight.sh used to catch this failure in an `if` and silently
# skip the staged-build check, same as passing `none`).
AC_HOME="$(mktemp -d)"
mkdir -p "$AC_HOME/target-rig-stage"
if (resolve_rev nonexistent-rev) 2>/dev/null; then
    echo "FAIL: resolve_rev should refuse a revision with nothing staged for it"
    exit 1
fi
rm -rf "$AC_HOME"
unset AC_HOME
echo "lib.sh resolve_rev: fails closed on a missing revision"

# The block above tests resolve_rev in isolation, which never had the bug.
# preflight.sh's actual failure was in how it composed resolve_rev with
# rig_dest: `dest="$(rig_dest "$(resolve_rev "$rev_arg")")"` buries
# resolve_rev's exit 1 inside the inner command substitution, where only
# rig_dest's own (always-0, it just echoes) exit status reaches the
# assignment — set -e never sees the failure. xrun-soak.sh and
# probe-outputs.sh composed the same nested, broken form separately and
# were unfixed by the commit that first fixed preflight.sh (PR #441 QA
# finding, fifth pass). All three now call one function, lib.sh's
# resolve_dest, instead of each inlining the split form.
#
# This block tests resolve_dest on its own: a regression inside the
# function turns it red. It does not read or run any caller, so a caller
# that drops resolve_dest for the nested form is invisible here — the next
# block covers that.
AC_HOME="$(mktemp -d)"
mkdir -p "$AC_HOME/target-rig-stage"
if (resolve_dest nonexistent-rev) >/dev/null 2>&1; then
    echo "FAIL: resolve_dest should fail closed on a missing revision"
    exit 1
fi
rm -rf "$AC_HOME"
unset AC_HOME
echo "lib.sh resolve_dest: fails closed on a missing revision"

# Each caller of resolve_dest, run for real against a revision with nothing
# staged, with ssh/scp stubbed on PATH (PR #441 QA and codex-qa finding,
# 35ddb7a1: the block above never touched the callers, so reverting one of
# them to the nested `rig_dest "$(resolve_rev ...)"` form stayed green).
# resolve_rev prints its error in both the fixed and the nested form, so the
# test does not look for the message: it requires a nonzero exit and that no
# remote script reached the (stub) rig after resolution. The only remote
# script allowed is probe-outputs.sh's port-order check, which runs before
# it resolves the revision.
AC_HOME="$(mktemp -d)"
export AC_HOME
mkdir -p "$AC_HOME/target-rig-stage" "$AC_HOME/rig-hosts" "$AC_HOME/bin"
cat >"$AC_HOME/rig-hosts/pupu.access.env" <<'ENV'
RIG_HOST=lib-test.invalid
RIG_USER=lib-test
RIG_SSH_KEY=/dev/null
ENV
cat >"$AC_HOME/bin/ssh" <<'STUB'
#!/usr/bin/env bash
case "$*" in
    *port_order.py*) echo "port order: matches the profile" ;;
    *"bash -c"*) echo "remote script" >>"$AC_HOME/remote.log" ;;
esac
exit 0
STUB
printf '#!/bin/sh\nexit 0\n' >"$AC_HOME/bin/scp"
chmod +x "$AC_HOME/bin/ssh" "$AC_HOME/bin/scp"
here="$(dirname "$0")"
assert_caller_refuses() {  # assert_caller_refuses <script> <args...>
    local script=$1 rc
    shift
    rm -f "$AC_HOME/remote.log"
    PATH="$AC_HOME/bin:$PATH" bash "$here/$script" "$@" >/dev/null 2>&1 && rc=0 || rc=$?
    [[ $rc != 0 ]] || { echo "FAIL: $script should fail on a missing revision"; exit 1; }
    [[ ! -e $AC_HOME/remote.log ]] ||
        { echo "FAIL: $script sent a remote script to the rig after its revision failed to resolve"; exit 1; }
}
assert_caller_refuses preflight.sh pupu nonexistent-rev
assert_caller_refuses xrun-soak.sh pupu 10 --rev nonexistent-rev
assert_caller_refuses probe-outputs.sh pupu --level -60 --consent "lib_test.sh, stubbed ssh" --rev nonexistent-rev
rm -rf "$AC_HOME"
unset AC_HOME
echo "preflight.sh / xrun-soak.sh / probe-outputs.sh: stop before the rig on a missing revision"
