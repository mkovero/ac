#!/usr/bin/env bash
# lib_test.sh — regression test for require_level/speaker_ceiling. No rig
# needed: pure arithmetic on the two coupled ceiling constants.
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
# This block used to carry three hardcoded copies of that split form (one
# per script) rather than calling resolve_dest — which meant reverting any
# one script's actual call site back to the nested, broken form left this
# test green, since it never read the scripts at all (PR #441 QA finding,
# sixth pass, confirmed by live reproduction: reverting xrun-soak.sh alone
# left all blocks passing). Calling resolve_dest directly closes that gap:
# a regression in the function itself, or in any caller that stops using
# it, is now the only way for preflight.sh/xrun-soak.sh/probe-outputs.sh to
# regress, and this is that function's own test.
AC_HOME="$(mktemp -d)"
mkdir -p "$AC_HOME/target-rig-stage"
if (resolve_dest nonexistent-rev) >/dev/null 2>&1; then
    echo "FAIL: resolve_dest should fail closed on a missing revision"
    exit 1
fi
rm -rf "$AC_HOME"
unset AC_HOME
echo "lib.sh resolve_dest: fails closed on a missing revision"
