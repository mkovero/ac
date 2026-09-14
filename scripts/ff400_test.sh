#!/usr/bin/env bash
# ff400_test.sh — rig-free regression test for ff400.sh's JACK alias
# handling (issue #444). Stubs jack_lsp / jack_alias / amixer on PATH; no
# FF400 hardware or real JACK server needed.
#
#   bash scripts/ff400_test.sh

set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
FF400="$HERE/ff400.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

FAILED=0
fail() { echo "FAIL: $1"; FAILED=1; }

# ── stub tools ────────────────────────────────────────────────────────────
# STUBDIR/jack_lsp, /jack_alias mutate JACK_STATE, a directory with one file
# per port holding that port's current aliases (one per line, in the order
# they were set — file line 1 stands in for jackd's own alsa_pcm alias).
# amixer answers `cget numid=8` and `cget numid=63..80` with a values= list
# AMIXER_WIDTH long, everything else with a single value; `cset` always
# succeeds. All three log every invocation.

write_stubs() {
    local bindir="$1"
    mkdir -p "$bindir"

    cat > "$bindir/amixer" <<'EOF'
#!/usr/bin/env bash
echo "$*" >> "${AMIXER_LOG:?}"
case "$*" in
    *"cget numid="*)
        numid=$(echo "$*" | grep -oE 'numid=[0-9]+' | grep -oE '[0-9]+')
        if [[ "$numid" == 8 || ( "$numid" -ge 63 && "$numid" -le 80 ) ]]; then
            n="${AMIXER_WIDTH:-18}"
            vals=$(python3 -c "print(','.join(['0']*int(\"$n\")))")
            echo ": values=$vals"
        else
            echo ": values=0"
        fi
        exit 0
        ;;
    *"cset"*)
        exit 0
        ;;
esac
exit 0
EOF

    cat > "$bindir/jack_lsp" <<'EOF'
#!/usr/bin/env bash
[[ -e "${JACK_DOWN:?}" ]] && exit 1
state="${JACK_STATE:?}"
if [[ "${1:-}" == "-A" ]]; then
    for f in "$state"/*; do
        [[ -e "$f" ]] || continue
        echo "$(basename "$f")"
        while IFS= read -r a; do
            [[ -n "$a" ]] && echo "   $a"
        done < "$f"
    done
else
    for f in "$state"/*; do
        [[ -e "$f" ]] || continue
        basename "$f"
    done
fi
exit 0
EOF

    cat > "$bindir/jack_alias" <<'EOF'
#!/usr/bin/env bash
echo "$*" >> "${ALIAS_LOG:?}"
state="${JACK_STATE:?}"
if [[ "${1:-}" == "-u" ]]; then
    [[ -z "${FORCE_UNALIAS_FAIL:-}" ]] || exit 1
    port="$2"; alias_name="$3"
    f="$state/$port"
    [[ -e "$f" ]] || exit 1
    grep -qxF "$alias_name" "$f" || exit 1
    grep -vxF "$alias_name" "$f" > "$f.tmp" && mv "$f.tmp" "$f"
    exit 0
else
    port="$1"; alias_name="$2"
    echo "$alias_name" >> "$state/$port"
    exit 0
fi
EOF

    chmod +x "$bindir"/amixer "$bindir"/jack_lsp "$bindir"/jack_alias
}

STUBDIR="$WORK/bin"
write_stubs "$STUBDIR"
export PATH="$STUBDIR:$PATH"

STATE="$WORK/state"
export JACK_STATE="$STATE"
export JACK_DOWN="$WORK/jack_down"     # file exists => jack_lsp fails
export AMIXER_LOG="$WORK/amixer.log"
export ALIAS_LOG="$WORK/alias.log"

reset_state() {
    # $1 = capture/playback port count for this run
    rm -rf "$STATE"; mkdir -p "$STATE"
    rm -f "$JACK_DOWN"
    : > "$AMIXER_LOG"; : > "$ALIAS_LOG"
    unset FORCE_UNALIAS_FAIL
    export AMIXER_WIDTH="$1"
    for i in $(seq 1 "$1"); do
        echo "alsa_pcm:hw:Card:out$i" > "$STATE/system:capture_$i"
        echo "alsa_pcm:hw:Card:in$i"  > "$STATE/system:playback_$i"
    done
}

plant_ff400_alias() {
    echo "FF400:capture_ADAT1" >> "$STATE/system:capture_1"
}

# ── (a),(b),(c),(d): plant an old-table alias, run, check the cleanup ──────
reset_state 18
plant_ff400_alias
out="$(bash "$FF400" 2>&1)"; rc=$?
[[ $rc -eq 0 ]] || fail "(a) script exited $rc on a clean alias-clearing run: $out"
grep -qF "FF400:" "$STATE/system:capture_1" && fail "(a) FF400: alias on system:capture_1 survived the run"
grep -qxF "alsa_pcm:hw:Card:out1" "$STATE/system:capture_1" \
    || fail "(b) non-FF400: alias on system:capture_1 was removed"
grep -vE '^-u ' "$ALIAS_LOG" | grep -q . && fail "(c) jack_alias called without -u: $(cat "$ALIAS_LOG")"
grep -q 'cset numid=90' "$AMIXER_LOG" && fail "(d) phantom power (numid=90) was written"

# ── (e): same checks at 14 capture ports (96 kHz) and 18 (48 kHz) ─────────
for n in 14 18; do
    reset_state "$n"
    plant_ff400_alias
    out="$(bash "$FF400" 2>&1)"; rc=$?
    [[ $rc -eq 0 ]] || fail "(e) script exited $rc with $n ports: $out"
    grep -qF "FF400:" "$STATE/system:capture_1" && fail "(e) FF400: alias survived with $n ports"
    volwidth=$(grep -oE '^-c 0 cset numid=8 [0-9,]+' "$AMIXER_LOG" | tail -1 | awk '{print $NF}' | awk -F, '{print NF}')
    [[ "$volwidth" == "$n" ]] || fail "(e) output-volume width was $volwidth, want $n ports"
done

# ── (f): JACK unreachable — must not claim "no aliases" ────────────────────
reset_state 18
: > "$JACK_DOWN"
out="$(bash "$FF400" 2>&1)"; rc=$?
[[ $rc -eq 0 ]] || fail "(f) script exited $rc when JACK was unreachable: $out"
echo "$out" | grep -qF "could not reach a JACK server" \
    || fail "(f) unreachable JACK did not print the 'could not reach' line: $out"
echo "$out" | grep -qi "no alias" && fail "(f) unreachable JACK was reported as 'no aliases' instead of unreachable"
[[ -s "$ALIAS_LOG" ]] && fail "(f) jack_alias was called while JACK was unreachable"
rm -f "$JACK_DOWN"

# ── (g): an unalias failure must exit non-zero ─────────────────────────────
reset_state 18
plant_ff400_alias
export FORCE_UNALIAS_FAIL=1
out="$(bash "$FF400" 2>&1)"; rc=$?
unset FORCE_UNALIAS_FAIL
[[ $rc -ne 0 ]] || fail "(g) script exited 0 despite a failing unalias: $out"

if [[ $FAILED -ne 0 ]]; then
    echo "ff400.sh alias handling: FAILED"
    exit 1
fi
echo "ff400.sh alias handling: all cases as expected"
