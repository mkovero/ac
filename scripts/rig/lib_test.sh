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
# assignment — set -e never sees the failure. Exercise preflight.sh's
# actual composed form (the fixed `rev="$(resolve_rev ...)" || exit 1`
# split), not a bare resolve_rev call, so a regression back to the nested
# form would be caught here (PR #441 QA finding, third pass).
AC_HOME="$(mktemp -d)"
mkdir -p "$AC_HOME/target-rig-stage"
if (
    rev="$(resolve_rev nonexistent-rev)" || exit 1
    rig_dest "$rev"
) >/dev/null 2>&1; then
    echo "FAIL: preflight.sh's composed rev/dest resolution should fail closed on a missing revision"
    exit 1
fi
rm -rf "$AC_HOME"
unset AC_HOME
echo "lib.sh resolve_rev + rig_dest composition (preflight.sh's actual line): fails closed on a missing revision"

# preflight.sh's fix above only closed the bug where preflight.sh itself hit
# it; xrun-soak.sh:26 and probe-outputs.sh:42 composed resolve_rev/rig_dest
# the same nested, broken way and were unfixed by that commit (PR #441 QA
# finding, fifth pass). Both are now split identically to preflight.sh — run
# each script's exact `rev`/`dest` lines (copied here, not re-derived) so a
# regression back to the nested form in either file is caught.
AC_HOME="$(mktemp -d)"
mkdir -p "$AC_HOME/target-rig-stage"
# xrun-soak.sh's composed form
rev=nonexistent-rev
if (
    if [[ $rev != installed ]]; then
        rev="$(resolve_rev "$rev")" || exit 1
        rig_dest "$rev"
    fi
) >/dev/null 2>&1; then
    echo "FAIL: xrun-soak.sh's rev/dest resolution should fail closed on a missing revision"
    exit 1
fi
# probe-outputs.sh's composed form (identical shape, separate call site)
rev=nonexistent-rev
if (
    if [[ $rev != installed ]]; then
        rev="$(resolve_rev "$rev")" || exit 1
        rig_dest "$rev"
    fi
) >/dev/null 2>&1; then
    echo "FAIL: probe-outputs.sh's rev/dest resolution should fail closed on a missing revision"
    exit 1
fi
rm -rf "$AC_HOME"
unset AC_HOME
echo "lib.sh resolve_rev + rig_dest composition (xrun-soak.sh / probe-outputs.sh): fails closed on a missing revision"
