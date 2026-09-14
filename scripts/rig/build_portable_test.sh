#!/usr/bin/env bash
# build_portable_test.sh — regression test for build-portable.sh's
# compiled_this_run derivation. No rig, no cargo build: synthetic build.log
# fixtures piped through the same grep pattern the script uses.
#
#   bash scripts/rig/build_portable_test.sh
#
# Procedure these scripts implement: docs/runbooks/rig-testing.md.

set -euo pipefail

# Keep this pattern identical to build-portable.sh's own grep — the point of
# this test is to catch a future narrowing of it, not to re-derive it.
pattern='^\s*Compiling ac-[a-z]+ '

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

check() {
    local name=$1 want=$2 file=$3
    local got=no
    grep -qE "$pattern" "$file" && got=yes
    if [[ $got != "$want" ]]; then
        echo "FAIL: $name — want compiled=$want, got compiled=$got"
        exit 1
    fi
}

# A rebuild that touches only ac-cli (no ac-daemon line) is still a real
# rebuild — the defect PR #441's QA review found live on pupu.
cat >"$tmp/ac-cli-only.log" <<'LOG'
   Compiling ac-cli v0.2.0 (/x/ac-rs/crates/ac-cli)
    Finished release [optimized] target(s) in 4.10s
LOG
check "ac-cli-only rebuild" yes "$tmp/ac-cli-only.log"

# The original, still-covered case: ac-daemon itself compiled.
cat >"$tmp/ac-daemon.log" <<'LOG'
   Compiling ac-daemon v0.2.0 (/x/ac-rs/crates/ac-daemon)
    Finished release [optimized] target(s) in 4.10s
LOG
check "ac-daemon rebuild" yes "$tmp/ac-daemon.log"

# A genuine cache hit: nothing recompiled.
cat >"$tmp/cache-hit.log" <<'LOG'
    Finished release [optimized] target(s) in 0.02s
LOG
check "cache hit" no "$tmp/cache-hit.log"

echo "build-portable.sh compiled_this_run: all cases as expected"
