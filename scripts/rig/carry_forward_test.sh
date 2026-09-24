#!/usr/bin/env bash
# carry_forward_test.sh — the carry-forward rule (#579) on synthetic fixtures.
# No rig and no cargo build: stages are built from small ELF files compiled
# with cc, their CARRYSUMS written by lib.sh's carry_sums, records filed with
# bin/common.sh's append_rig_block, and every verdict comes from running
# carry-forward.sh itself. Each case names the branch it is the red case for.
#
#   bash scripts/rig/carry_forward_test.sh
#
# Rule: docs/runbooks/rig-testing.md → "Carry-forward".

source "$(dirname "$0")/lib.sh"
set +e

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
cf="$here/carry-forward.sh"
command -v cc >/dev/null || die "cc is needed to build the ELF fixtures"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
export AC_SESSION_DIR="$tmp/session"
fail=0
ok() { echo "ok   $1"; }
bad() {
    echo "FAIL $1"
    fail=1
}

# Heads and target dirs as rig.sh produces them: per-commit, equal length.
A=210a7ff15d58aaaaaaaaaaaaaaaaaaaaaaaaaaaa
B=f64b26d113ecbbbbbbbbbbbbbbbbbbbbbbbbbbbb
C=0c0c0c0c0c0ccccccccccccccccccccccccccccc

# elf <out> <target_dir> <extra> — an ELF with a build-id that embeds the
# daemon path under <target_dir>, the way it_loopback_ir does, plus a string
# <extra> that stands for any real code or data change.
elf() {
    printf '#include <stdio.h>\nconst char d[] = "%s/release/ac-daemon";\nconst char x[] = "%s";\nint main(void) { puts(d); puts(x); return 0; }\n' \
        "$2" "$3" >"$1.c"
    cc -O0 -Wl,--build-id=sha1 -o "$1" "$1.c" || die "cc failed on $1.c"
}

# stage <dir> <head> <variant> [key=value ...] — a staged build. <variant>
# changes `ac`'s bytes; ELF=<extra> changes it_loopback_ir's code; DIRTY,
# RULE (empty = no carry_rule line) and DROP=<artefact> change the manifest
# or the artefact set.
stage() {
    local dir=$1 head=$2 variant=$3
    shift 3
    local ELF=same DIRTY=0 RULE=$CARRY_RULE DROP=""
    local kv
    for kv in "$@"; do local "${kv%%=*}=${kv#*=}"; done
    local target="/x/target-rig-${head:0:12}"
    mkdir -p "$dir"
    local names=() n
    for n in ac ac-daemon ir_probe transfer_probe; do
        [[ $n == "$DROP" ]] && continue
        printf 'binary %s %s\n' "$n" "$([[ $n == ac ]] && echo "$variant" || echo same)" >"$dir/$n"
        names+=("$n")
    done
    elf "$dir/it_loopback_ir" "$target" "$ELF"
    names+=(it_loopback_ir)
    (cd "$dir" && sha256sum "${names[@]}" >SHA256SUMS)
    carry_sums "$dir" "$target" >"$dir/CARRYSUMS" || die "carry_sums failed on $dir"
    {
        echo "rev=$head"
        echo "dirty_files=$DIRTY"
        echo "cargo_target_dir=$target"
        [[ -n $RULE ]] && echo "carry_rule=$RULE"
    } >"$dir/MANIFEST.txt"
}

# record <name> <kind> <head> <verdict> [<stage>] — file a rig record under
# $AC_SESSION_DIR with the runner's own block writer.
record() {
    local name=$1 kind=$2 head=$3 verdict=$4 st=${5:-}
    mkdir -p "$AC_SESSION_DIR"
    printf 'session notes\n\n**rig verdict:** %s\n' "$verdict" >"$AC_SESSION_DIR/$name"
    (
        cd "$repo" && source bin/common.sh
        if [[ $kind == measured ]]; then
            append_rig_block "$AC_SESSION_DIR/$name" measured "$head" "$verdict" "https://example/c/$head" "$st"
        else
            append_rig_block "$AC_SESSION_DIR/$name" carried "$head" "$verdict" "https://example/c/$head" "$A" "x.md"
        fi
    ) || die "append_rig_block failed for $name"
}

# expect <label> <want-rc> <pr> <stage-B> — run the production script.
expect() {
    local label=$1 want=$2
    shift 2
    local rc=0
    "$cf" "$@" >"$tmp/out" 2>"$tmp/err" || rc=$?
    if [[ $rc == "$want" ]]; then ok "$label (exit $rc$([[ -s $tmp/err ]] && printf -- ": %s" "$(head -1 "$tmp/err" | cut -c1-110)"))"; else
        bad "$label — want exit $want, got $rc: $(tail -3 "$tmp/err")"
    fi
}

fresh() { rm -rf "$AC_SESSION_DIR"; }
d1=2026-09-23 # one filing date for every record below

stage "$tmp/A" "$A" v1
stage "$tmp/B" "$B" v1
stage "$tmp/Bx" "$B" v1 ELF=changed
stage "$tmp/Bac" "$B" v2

# --- the ELF fixture itself ---------------------------------------------------
# Control, against the rejected implementation: plain sha256 of it_loopback_ir
# differs between A and B (the target dir and the build-id), so "the same
# hashing ship.sh verifies" alone could never carry this pair.
ia="$(sha256sum <"$tmp/A/it_loopback_ir")" ib="$(sha256sum <"$tmp/B/it_loopback_ir")"
[[ $ia != "$ib" ]] && ok "control: plain sha256 of it_loopback_ir differs across target dirs" ||
    bad "control: the fixtures' it_loopback_ir are identical, the target-dir case is not tested"
# The build-id differs too, so zeroing it is load-bearing, not decoration.
bid() { readelf -n "$1" 2>/dev/null | sed -n 's/.*Build ID: //p'; }
[[ -n $(bid "$tmp/A/it_loopback_ir") && $(bid "$tmp/A/it_loopback_ir") != $(bid "$tmp/B/it_loopback_ir") ]] &&
    ok "control: the fixtures' build-ids differ" || bad "control: the fixtures' build-ids do not differ"
ra="$(python3 -c 'import sys; d=open(sys.argv[1],"rb").read().replace(sys.argv[2].encode(), b"@"); import hashlib; print(hashlib.sha256(d).hexdigest())' "$tmp/A/it_loopback_ir" "/x/target-rig-${A:0:12}")"
rb="$(python3 -c 'import sys; d=open(sys.argv[1],"rb").read().replace(sys.argv[2].encode(), b"@"); import hashlib; print(hashlib.sha256(d).hexdigest())' "$tmp/B/it_loopback_ir" "/x/target-rig-${B:0:12}")"
[[ $ra != "$rb" ]] && ok "control: replacing the target dir without zeroing the build-id still differs" ||
    bad "control: string replacement alone already matches"
[[ $(carry_digest "$tmp/A/it_loopback_ir" "/x/target-rig-${A:0:12}") == $(carry_digest "$tmp/B/it_loopback_ir" "/x/target-rig-${B:0:12}") ]] &&
    ok "carry_digest: only the target dir differs → equal digests" || bad "carry_digest: a target-dir-only difference is not equal"
[[ $(carry_digest "$tmp/A/it_loopback_ir" "/x/target-rig-${A:0:12}") != $(carry_digest "$tmp/Bx/it_loopback_ir" "/x/target-rig-${B:0:12}") ]] &&
    ok "carry_digest: one more changed byte → different digests" || bad "carry_digest: a code change is hidden"
printf 'not an elf\n' >"$tmp/notelf"
carry_digest "$tmp/notelf" /x >/dev/null 2>&1 && bad "carry_digest accepted a non-ELF file" ||
    ok "carry_digest refuses a non-ELF file"

# --- carries ------------------------------------------------------------------
fresh
record "$d1-rig-pr-7-${A:0:12}-100000Z.md" measured "$A" pass "$tmp/A"
expect "A pass, B differs only in the target dir → carries" 0 7 "$tmp/B" "$tmp/facts"
grep -qx '\*\*rig verdict:\*\* pass' "$tmp/out" && grep -q "$A" "$tmp/out" && grep -q "$B" "$tmp/out" &&
    [[ $(grep -c '| match |' "$tmp/out") == 5 && $(head -1 "$tmp/out") == '<!-- agent: rig -->' ]] &&
    ok "the carried comment names A and B, five matching rows, one pass verdict" ||
    bad "the carried comment is incomplete: $(cat "$tmp/out")"
[[ $(sed -n 's/^measured_at=//p' "$tmp/facts") == "$A" ]] && ok "facts name the measured head" ||
    bad "facts do not name A"

# --- red cases, one per branch ------------------------------------------------
expect "one more byte in it_loopback_ir → no carry" 1 7 "$tmp/Bx"
expect "one digest differs (ac) → no carry" 1 7 "$tmp/Bac"

stage "$tmp/Bdirty" "$B" v1 DIRTY=2
expect "dirty_files≠0 at B → no carry" 1 7 "$tmp/Bdirty"
stage "$tmp/Brule" "$B" v1 RULE=999
expect "carry_rule differs → no carry" 1 7 "$tmp/Brule"
stage "$tmp/Bnorule" "$B" v1 RULE=
expect "carry_rule missing → no carry" 1 7 "$tmp/Bnorule"
stage "$tmp/Bdrop" "$B" v1 DROP=transfer_probe
expect "artefact sets differ → no carry" 1 7 "$tmp/Bdrop"

stage "$tmp/Asame" "$A" v1
expect "the measured record is already at this head → no carry" 1 7 "$tmp/Asame"

for v in fail decline-site decline; do
    fresh
    record "$d1-rig-pr-7-${A:0:12}-100000Z.md" measured "$A" "$v" "$tmp/A"
    expect "measured verdict $v → no carry" 1 7 "$tmp/B"
done

fresh
expect "no record at all → no carry" 1 7 "$tmp/B"

# Block missing: the newest measured record has none. An older record with a
# block must not stand in for it.
fresh
record "$d1-rig-pr-7-${A:0:12}-100000Z.md" measured "$A" pass "$tmp/A"
printf 'a record from before #579\n**rig verdict:** pass\n' >"$AC_SESSION_DIR/$d1-rig-pr-7-${A:0:12}-110000Z.md"
expect "newest record has no machine block → no carry" 1 7 "$tmp/B"

# Block not last: text after the closing fence means the runner did not write
# the end of the file.
fresh
record "$d1-rig-pr-7-${A:0:12}-100000Z.md" measured "$A" pass "$tmp/A"
echo "edited afterwards" >>"$AC_SESSION_DIR/$d1-rig-pr-7-${A:0:12}-100000Z.md"
expect "text after the machine block → no carry" 1 7 "$tmp/B"

# Source is carried: A (measured, pass) → B (carried) → C. C is compared with
# A. If the carried record were the source it has no digests, so this could
# only exit 0 by reading A.
stage "$tmp/C" "$C" v1
fresh
record "$d1-rig-pr-7-${A:0:12}-100000Z.md" measured "$A" pass "$tmp/A"
record "$d1-rig-pr-7-${B:0:12}-110000Z.md" carried "$B" pass
expect "A → B (carried) → C: C carries against A" 0 7 "$tmp/C"
grep -q "measured at (A): \`$A\`" "$tmp/out" && ok "C's comment names A as the measured head, not B" ||
    bad "C's comment does not name A"
stage "$tmp/Cx" "$C" v1 ELF=changed
expect "A → B (carried) → C, C's binary differs from A → no carry" 1 7 "$tmp/Cx"

fresh
record "$d1-rig-pr-7-${B:0:12}-110000Z.md" carried "$B" pass
expect "only carried records → no carry" 1 7 "$tmp/C"

# Stamp order, not name order: the rev sits between date and time in the name.
# Red on a plain name sort, which would pick ffff…'s older pass.
fresh
record "$d1-rig-pr-7-ffffffffffff-100000Z.md" measured "$A" pass "$tmp/A"
record "$d1-rig-pr-7-aaaaaaaaaaaa-110000Z.md" measured "$A" fail "$tmp/A"
expect "a newer fail sorts before an older pass by stamp, not by rev → no carry" 1 7 "$tmp/B"

# A's side comes only from the filed block; B's stage is re-verified by
# verify_stage, so these guards can only fire on a hand-edited or corrupted
# block at A. Each case asserts which refusal fired, not only the exit code:
# B's digests still match A's CARRYSUMS, so a plain table mismatch cannot be
# what stops them.
# expect_err <text> <label> — the last expect's stderr names <text>.
expect_err() {
    grep -qF -- "$1" "$tmp/err" && ok "$2" || bad "$2 — stderr: $(tail -2 "$tmp/err")"
}
arec="$AC_SESSION_DIR/$d1-rig-pr-7-${A:0:12}-100000Z.md"
zeros="$(printf '0%.0s' {1..64})"

fresh
record "$d1-rig-pr-7-${A:0:12}-100000Z.md" measured "$A" pass "$tmp/A"
sed -i "s/^sha256sum=[0-9a-f]*  ac\$/sha256sum=$zeros  ac/" "$arec"
expect "A's CARRYSUMS line for ac is not its SHA256SUMS line → no carry" 1 7 "$tmp/B"
expect_err "\`ac\`'s CARRYSUMS line is not its SHA256SUMS line at A" "  … refused by the line-equality guard at A"

fresh
record "$d1-rig-pr-7-${A:0:12}-100000Z.md" measured "$A" pass "$tmp/A"
sed -i '/^sha256sum=.*  transfer_probe$/d' "$arec"
expect "A's CARRYSUMS and SHA256SUMS list different artefacts → no carry" 1 7 "$tmp/B"
expect_err "CARRYSUMS and SHA256SUMS list different artefacts at A" "  … refused by the per-head artefact-set guard at A"

fresh
record "$d1-rig-pr-7-${A:0:12}-100000Z.md" measured "$A" pass "$tmp/A"
sed -i "s/^manifest=rev=.*/manifest=rev=$C/" "$arec"
expect "A's recorded manifest is for another head → no carry" 1 7 "$tmp/B"
expect_err "recorded MANIFEST.txt is for $C, not its head $A" "  … refused by the manifest-rev guard"

# A stage that no longer matches its sums cannot be compared.
fresh
record "$d1-rig-pr-7-${A:0:12}-100000Z.md" measured "$A" pass "$tmp/A"
cp -r "$tmp/B" "$tmp/Btamper" && echo x >>"$tmp/Btamper/ac"
expect "B's binaries no longer match its SHA256SUMS → could not compare" 2 7 "$tmp/Btamper"
cp -r "$tmp/B" "$tmp/Bold" && rm "$tmp/Bold/CARRYSUMS"
expect "B built before #579 (no CARRYSUMS) → could not compare" 2 7 "$tmp/Bold"

# --- --stages -------------------------------------------------------------------
expect "--stages A B → carries" 0 --stages "$tmp/A" "$tmp/B"
expect "--stages A Bx → no carry" 1 --stages "$tmp/A" "$tmp/Bx"

((fail == 0)) && echo "carry-forward: all cases as expected"
exit $fail
