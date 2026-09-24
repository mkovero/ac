#!/usr/bin/env bash
# carry-forward.sh — does a rig record measured at an earlier head still count
# at the head staged in <stage-B>? The rule it applies is stated once, in
# docs/runbooks/rig-testing.md → "Carry-forward" (#579); this file implements
# it and does not restate it.
#
#   scripts/rig/carry-forward.sh <pr> <stage-B> [<facts-out>]
#       Source: the newest measured rig record for <pr> under $AC_SESSION_DIR
#       (default $AC_HOME/session), read from the runner's machine block.
#       On a carry, prints the carried-forward PR comment on stdout and, when
#       <facts-out> is given, writes measured_at=, measured_record= and
#       comment= to it.
#   scripts/rig/carry-forward.sh --stages <stage-A> <stage-B>
#       Compare two staged builds directly; prints the digest table.
#
# Exit: 0 carries · 1 does not carry (reason on stderr) · 2 could not compare.
# Nothing defaults to "carries": every failure path is non-zero.

source "$(dirname "$0")/lib.sh"

# An unexpected failure anywhere, functions included, is "could not compare".
set -o errtrace
trap 'exit 2' ERR

no_carry() {
    echo "carry-forward: no carry — $*" >&2
    exit 1
}
cannot() {
    echo "carry-forward: could not compare — $*" >&2
    exit 2
}

# The fence the runner writes (bin/common.sh → append_rig_block). The block
# must be the last thing in the record.
BLOCK_FENCE='```rig-runner'

# block_of <record> — the body of the runner's machine block, or non-zero when
# the record does not end with one.
block_of() {
    awk -v fence="$BLOCK_FENCE" '
        $0 == fence { inb = 1; body = ""; closed = 0; tail = 0; next }
        inb && $0 == "```" { inb = 0; closed = 1; next }
        inb { body = body $0 "\n"; next }
        closed && NF { tail = 1 }
        END { if (!closed || inb || tail) exit 1; printf "%s", body }
    ' "$1"
}

# field <key> <block text> — the value of the first `key=` line.
field() { sed -n "s/^$1=//p" <<<"$2" | head -1; }

# lines <prefix> <block text> — every `prefix=` line's value, in order.
lines() { sed -n "s/^$1=//p" <<<"$2"; }

manifest_value() { sed -n "s/^$1=//p" "$2" | head -1; }

# verify_stage <stage> — the stage still matches its own SHA256SUMS, and its
# CARRYSUMS is what carry_sums computes from it now.
verify_stage() {
    local stage=$1 target
    for f in MANIFEST.txt SHA256SUMS CARRYSUMS; do
        [[ -s $stage/$f ]] || cannot "$stage has no $f (built before carry-forward, or the build failed)"
    done
    (cd "$stage" && sha256sum --quiet -c SHA256SUMS) >/dev/null 2>&1 ||
        cannot "$stage no longer matches its own SHA256SUMS"
    target="$(manifest_value cargo_target_dir "$stage/MANIFEST.txt")"
    [[ -n $target ]] || cannot "$stage/MANIFEST.txt has no cargo_target_dir"
    local now
    now="$(carry_sums "$stage" "$target")" || cannot "carry_sums failed on $stage"
    [[ $now == "$(cat "$stage/CARRYSUMS")" ]] || cannot "$stage/CARRYSUMS does not match its binaries"
}

names_of() { awk '{print $2}' "$1"; }

# compare <label-A> <manifest-A> <sums-A> <carry-A> <label-B> <manifest-B>
#         <sums-B> <carry-B> <table-out> — rule items 3 and 4. Writes the
# digest table to <table-out> whatever the outcome; returns via no_carry.
compare() {
    local la=$1 ma=$2 sa=$3 ca=$4 lb=$5 mb=$6 sb=$7 cb=$8 out=$9
    local da db ra rb
    da="$(manifest_value dirty_files "$ma")"
    db="$(manifest_value dirty_files "$mb")"
    ra="$(manifest_value carry_rule "$ma")"
    rb="$(manifest_value carry_rule "$mb")"

    {
        echo "| artefact | digest at $la | digest at $lb | |"
        echo "|---|---|---|---|"
    } >"$out"
    local mismatch=0 name a b r rewritten
    while read -r a name; do
        b="$(awk -v n="$name" '$2 == n {print $1}' "$cb")"
        if [[ -n $b && $a == "$b" ]]; then
            echo "| \`$name\` | \`$a\` | \`$b\` | match |" >>"$out"
        else
            echo "| \`$name\` | \`$a\` | \`${b:-missing}\` | **differs** |" >>"$out"
            mismatch=1
        fi
    done <"$ca"

    [[ $da == 0 ]] || no_carry "dirty_files=${da:-missing} at $la, want 0"
    [[ $db == 0 ]] || no_carry "dirty_files=${db:-missing} at $lb, want 0"
    [[ -n $ra && -n $rb ]] || no_carry "carry_rule missing (at $la: '${ra}', at $lb: '${rb}') — a build from before #579"
    [[ $ra == "$rb" ]] || no_carry "carry_rule $ra at $la, $rb at $lb"

    local i lab s c
    for i in a b; do
        if [[ $i == a ]]; then lab=$la s=$sa c=$ca; else lab=$lb s=$sb c=$cb; fi
        [[ -s $s && -s $c ]] || no_carry "empty SHA256SUMS or CARRYSUMS at $lab"
        [[ $(names_of "$s") == "$(names_of "$c")" ]] ||
            no_carry "CARRYSUMS and SHA256SUMS list different artefacts at $lab"
        while read -r _ name; do
            rewritten=0
            for r in "${CARRY_REWRITTEN[@]}"; do [[ $name == "$r" ]] && rewritten=1; done
            if ((!rewritten)); then
                [[ $(grep -E "  $name\$" "$s") == "$(grep -E "  $name\$" "$c")" ]] ||
                    no_carry "\`$name\`'s CARRYSUMS line is not its SHA256SUMS line at $lab"
            fi
        done <"$s"
    done
    [[ $(names_of "$ca") == "$(names_of "$cb")" ]] || no_carry "the artefact sets differ between $la and $lb"
    ((mismatch == 0)) || no_carry "a shipped artefact's digest differs between $la and $lb:
$(cat "$out")"
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

if [[ ${1:-} == --stages ]]; then
    [[ $# == 3 ]] || die "usage: $0 --stages <stage-A> <stage-B>"
    a=$2 b=$3
    verify_stage "$a"
    verify_stage "$b"
    ha="$(manifest_value rev "$a/MANIFEST.txt")"
    hb="$(manifest_value rev "$b/MANIFEST.txt")"
    compare "${ha:0:12}" "$a/MANIFEST.txt" "$a/SHA256SUMS" "$a/CARRYSUMS" \
        "${hb:0:12}" "$b/MANIFEST.txt" "$b/SHA256SUMS" "$b/CARRYSUMS" "$tmp/table"
    cat "$tmp/table"
    echo "carries: $ha → $hb"
    exit 0
fi

[[ $# == 2 || $# == 3 ]] || die "usage: $0 <pr> <stage-B> [<facts-out>] | --stages <stage-A> <stage-B>"
pr=$1 stage=$2 facts=${3:-}
[[ $pr =~ ^[0-9]+$ ]] || die "not a PR number: $pr"
session="${AC_SESSION_DIR:-$(ac_home)/session}"

verify_stage "$stage"
head_b="$(manifest_value rev "$stage/MANIFEST.txt")"
[[ $head_b =~ ^[0-9a-f]{40}$ ]] || cannot "$stage/MANIFEST.txt has no full rev"

# Rule 1 — source: the newest measured record, in filing-stamp order (date and
# UTC time from the name; the rev between them must not sort).
re="^([0-9]{4}-[0-9]{2}-[0-9]{2})-rig-pr-$pr-[0-9a-f]{12}-([0-9]{6})Z\.md\$"
ordered="$(
    for f in "$session"/*-rig-pr-"$pr"-*.md; do
        [[ -f $f ]] || continue
        b="$(basename "$f")"
        if [[ $b =~ $re ]]; then
            printf '%s%s %s\n' "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}" "$f"
        fi
    done | LC_ALL=C sort -r | cut -d' ' -f2-
)"
[[ -n $ordered ]] || no_carry "no filed rig record for PR #$pr under $session"

src="" blk=""
while IFS= read -r f; do
    blk="$(block_of "$f")" || no_carry "the newest measured record $f has no runner machine block (records filed before #579 never carry)"
    kind="$(field kind "$blk")"
    case $kind in
        carried) continue ;;
        measured) src=$f; break ;;
        *) no_carry "$f's machine block has kind='$kind'" ;;
    esac
done <<<"$ordered"
[[ -n $src ]] || no_carry "PR #$pr has only carried-forward records; a carried record is never a source"

head_a="$(field head "$blk")"
verdict="$(field verdict "$blk")"
comment="$(field comment "$blk")"
[[ $head_a =~ ^[0-9a-f]{40}$ ]] || no_carry "$src's machine block has no full head"
[[ $head_a != "$head_b" ]] || no_carry "the newest measured record is already at $head_b — a new session was asked for at this head"
# Rule 2 — verdict.
[[ $verdict == pass ]] || no_carry "the measured record at $head_a has verdict '${verdict:-missing}', only pass carries"

lines manifest "$blk" >"$tmp/manifest-a"
lines sha256sum "$blk" >"$tmp/sums-a"
lines carrysum "$blk" >"$tmp/carry-a"
[[ $(manifest_value rev "$tmp/manifest-a") == "$head_a" ]] ||
    no_carry "$src's recorded MANIFEST.txt is for $(manifest_value rev "$tmp/manifest-a"), not its head $head_a"

# Rules 3 and 4.
compare "A (${head_a:0:12})" "$tmp/manifest-a" "$tmp/sums-a" "$tmp/carry-a" \
    "B (${head_b:0:12})" "$stage/MANIFEST.txt" "$stage/SHA256SUMS" "$stage/CARRYSUMS" "$tmp/table"

if [[ -n $facts ]]; then
    printf 'measured_at=%s\nmeasured_record=%s\ncomment=%s\n' "$head_a" "$src" "$comment" >"$facts"
fi

cat <<EOF
<!-- agent: rig -->
## rig — PR #$pr at $head_b

**Carried forward, no session ran at this head.** The shipped binaries built at
this head are identical, by the digests below, to the ones a measured rig
session ran at an earlier head. Rule: \`docs/runbooks/rig-testing.md\` →
"Carry-forward" (#579). This record was written by the runner, not a session.

- measured at (A): \`$head_a\` — [rig record]($comment), filed as \`$src\`
- current head (B): \`$head_b\`
- measured verdict at A: \`pass\`
- \`dirty_files=0\` and \`carry_rule=$(manifest_value carry_rule "$stage/MANIFEST.txt")\` at both heads

$(cat "$tmp/table")

What this record covers is exactly what the record at A covers: judge the
named check against that record's content.

**rig verdict:** pass
EOF
