#!/usr/bin/env bash
# review.sh <pr> [--fg]
#
# QA role. No Edit/Write against the tree — a reviewer that can fix what it
# finds will fix it, and the finding never reaches you as a finding. Delegation
# is safe: explorer is read-only too, so it is not a way around that.

source "$(dirname "$0")/common.sh"
n="${1:?usage: review.sh <pr> [--fg]}"; shift || true

run qa "Review PR #$n in $AC_REPO." --read "$@"
