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
