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
#
# amixer's `cget numid=N` answers with a values= list whose width is fixed
# per control family, matching the upstream ctl-service model (architect,
# issue #444 re-entry): numid=8 → WIDTH_8 (default 18), numid 9..26 →
# WIDTH_ANALOG (default 8), numid 45..62 → WIDTH_ADAT (default 8), numid
# 63..80 → WIDTH_STREAM (default 18). These widths do NOT follow JACK port
# count — reset_state's port count and the mixer widths are independent
# knobs, on purpose (case (e)). FAIL_NUMID, if set, makes that one numid's
# cget answer with no `values=` line at all, simulating an unreadable
# control. `cset` always succeeds (real amixer never rejects a count
# mismatch — see architect's evidence — so this stub doesn't simulate one;
# case (h) checks the *sent* width against the reported one instead). All
# three log every invocation.
#
# jack_lsp's `-A` calls are counted in JACK_A_CALLS: when JACK_A_DOWN is
# set, the Nth `-A` call fails once N exceeds JACK_A_DOWN_AFTER (default 0,
# i.e. the very first `-A` call fails) — this simulates JACK going away
# *after* the plain reachability probe (`jack_lsp` with no args) already
# succeeded, which a masked pipe/process-substitution failure would miss
# (codex-qa, PR #450 re-review).

write_stubs() {
    local bindir="$1"
    mkdir -p "$bindir"

    cat > "$bindir/amixer" <<'EOF'
#!/usr/bin/env bash
echo "$*" >> "${AMIXER_LOG:?}"
case "$*" in
    *"cget numid="*)
        numid=$(echo "$*" | grep -oE 'numid=[0-9]+' | grep -oE '[0-9]+')
        if [[ -n "${FAIL_NUMID:-}" && "$numid" == "$FAIL_NUMID" ]]; then
            exit 0   # no values= line at all: simulates an unreadable control
        fi
        if [[ "$numid" == 8 ]]; then
            n="${WIDTH_8:-18}"
        elif [[ "$numid" -ge 9 && "$numid" -le 26 ]]; then
            n="${WIDTH_ANALOG:-8}"
        elif [[ "$numid" -ge 45 && "$numid" -le 62 ]]; then
            n="${WIDTH_ADAT:-8}"
        elif [[ "$numid" -ge 63 && "$numid" -le 80 ]]; then
            n="${WIDTH_STREAM:-18}"
        else
            n=1
        fi
        vals=$(python3 -c "print(','.join(['0']*int(\"$n\")))")
        echo ": values=$vals"
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
    if [[ -n "${JACK_A_DOWN:-}" ]]; then
        calls="${JACK_A_CALLS:?}"
        n=0
        [[ -e "$calls" ]] && n=$(cat "$calls")
        n=$((n + 1))
        echo "$n" > "$calls"
        [[ "$n" -gt "${JACK_A_DOWN_AFTER:-0}" ]] && exit 1
    fi
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
export JACK_A_CALLS="$WORK/jack_a_calls"   # jack_lsp -A call counter, see JACK_A_DOWN
export AMIXER_LOG="$WORK/amixer.log"
export ALIAS_LOG="$WORK/alias.log"

reset_state() {
    # $1 = capture/playback port count for this run. Mixer control widths
    # are a separate knob (WIDTH_8/WIDTH_ANALOG/WIDTH_ADAT/WIDTH_STREAM,
    # FAIL_NUMID) — reset_state does not touch them, on purpose (case (e)).
    rm -rf "$STATE"; mkdir -p "$STATE"
    rm -f "$JACK_DOWN" "$JACK_A_CALLS"
    : > "$AMIXER_LOG"; : > "$ALIAS_LOG"
    unset FORCE_UNALIAS_FAIL JACK_A_DOWN JACK_A_DOWN_AFTER
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

# ── (e): mixer writes are unchanged across 14 and 18 JACK ports ───────────
# Control width must not follow JACK port count (architect, re-entry): with
# the mixer widths held fixed, the alias result and every cset the script
# issues must be identical whether JACK exposes 14 or 18 ports.
prev_csets=""
for n in 14 18; do
    reset_state "$n"
    plant_ff400_alias
    out="$(bash "$FF400" 2>&1)"; rc=$?
    [[ $rc -eq 0 ]] || fail "(e) script exited $rc with $n JACK ports: $out"
    grep -qF "FF400:" "$STATE/system:capture_1" && fail "(e) FF400: alias survived with $n JACK ports"
    csets="$(grep 'cset numid=' "$AMIXER_LOG")"
    if [[ -n "$prev_csets" ]]; then
        [[ "$csets" == "$prev_csets" ]] \
            || fail "(e) mixer writes changed between 14 and 18 JACK ports (control width must not follow port count)"
    fi
    prev_csets="$csets"
done

# ── (h): every cset's value count matches the width amixer reported ───────
# Must go red against an unrevised ff400.sh that still sends 18 values to
# an 8-wide analog-source-gain/adat-source-gain control (correctness issue
# 1, PR #450 qa review).
expected_width() {
    local numid="$1"
    if [[ "$numid" == 8 ]]; then echo "${WIDTH_8:-18}"
    elif [[ "$numid" -ge 9 && "$numid" -le 26 ]]; then echo "${WIDTH_ANALOG:-8}"
    elif [[ "$numid" -ge 45 && "$numid" -le 62 ]]; then echo "${WIDTH_ADAT:-8}"
    elif [[ "$numid" -ge 63 && "$numid" -le 80 ]]; then echo "${WIDTH_STREAM:-18}"
    fi
}
reset_state 18
plant_ff400_alias
out="$(bash "$FF400" 2>&1)"; rc=$?
[[ $rc -eq 0 ]] || fail "(h) script exited $rc with default control widths: $out"
while IFS= read -r line; do
    numid=$(echo "$line" | grep -oE 'numid=[0-9]+' | head -1 | grep -oE '[0-9]+')
    want="$(expected_width "$numid")"
    [[ -n "$want" ]] || continue
    got=$(echo "$line" | awk '{print $NF}' | awk -F, '{print NF}')
    [[ "$got" == "$want" ]] \
        || fail "(h) numid=$numid cset sent $got values, amixer reported width $want"
done < <(grep 'cset numid=' "$AMIXER_LOG")

# ── (i): an unreadable control width aborts before any cset ────────────────
reset_state 18
export FAIL_NUMID=70   # inside 63..80 (stream-source-gain)
out="$(bash "$FF400" 2>&1)"; rc=$?
unset FAIL_NUMID
[[ $rc -ne 0 ]] || fail "(i) script exited 0 despite an unreadable numid=70 width: $out"
echo "$out" | grep -q '\bnumid=70\b' || fail "(i) failure message did not name numid=70: $out"
grep -q 'cset' "$AMIXER_LOG" && fail "(i) a cset was issued despite an unreadable control width: $(cat "$AMIXER_LOG")"

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

# ── (j): `show` also clears a stale FF400: alias and prints guidance ───────
# Must go red against a `show` branch that exits before clear_ff400_aliases
# runs (codex-qa major finding, PR #450 re-review).
reset_state 18
plant_ff400_alias
out="$(bash "$FF400" show 2>&1)"; rc=$?
[[ $rc -eq 0 ]] || fail "(j) show exited $rc: $out"
grep -qF "FF400:" "$STATE/system:capture_1" && fail "(j) show did not clear the planted FF400: alias"
echo "$out" | grep -qi "none set here" \
    || fail "(j) show did not print the no-alias/where-to-look guidance: $out"

# ── (k): jack_lsp -A failing right after the reachability probe must not
# claim aliases were cleared or confirmed absent (codex-qa major finding:
# the initial enumeration ran inside a process substitution whose failure
# `set -e` never saw) ────────────────────────────────────────────────────
reset_state 18
plant_ff400_alias
export JACK_A_DOWN=1 JACK_A_DOWN_AFTER=0   # fail on the 1st -A call (enumeration)
out="$(bash "$FF400" 2>&1)"; rc=$?
unset JACK_A_DOWN JACK_A_DOWN_AFTER
[[ $rc -ne 0 ]] || fail "(k) script exited 0 despite jack_lsp -A failing during enumeration: $out"
echo "$out" | grep -qi "cleared any FF400" && fail "(k) claimed aliases cleared despite jack_lsp -A failing: $out"
echo "$out" | grep -qi "none set here" && fail "(k) claimed no aliases set despite jack_lsp -A failing: $out"

# ── (m): jack_lsp -A failing on the post-clear verify pass (enumeration
# itself succeeded) must not claim aliases were confirmed cleared (codex-qa
# major finding: the verify pipe into awk masked jack_lsp's exit status
# without pipefail) ─────────────────────────────────────────────────────
reset_state 18
plant_ff400_alias
export JACK_A_DOWN=1 JACK_A_DOWN_AFTER=1   # 1st -A (enumeration) ok, 2nd (verify) fails
out="$(bash "$FF400" 2>&1)"; rc=$?
unset JACK_A_DOWN JACK_A_DOWN_AFTER
[[ $rc -ne 0 ]] || fail "(m) script exited 0 despite jack_lsp -A failing during verification: $out"
echo "$out" | grep -qi "cleared any FF400" && fail "(m) claimed aliases cleared despite the verify jack_lsp -A failing: $out"

# ── (l): `show` must not abort on an unreadable stream-source-gain row ─────
# numid=64 is ch01 of the diagonal; FAIL_NUMID makes it answer with no
# `values=` line, which previously made an unguarded `$(...)` assignment
# trip `set -e` mid-loop (codex-qa minor finding, PR #450 re-review).
reset_state 18
export FAIL_NUMID=64
out="$(bash "$FF400" show 2>&1)"; rc=$?
unset FAIL_NUMID
[[ $rc -eq 0 ]] || fail "(l) show exited $rc on an unreadable numid=64 diagonal row: $out"
echo "$out" | grep -q 'ch01 (could not be read)' \
    || fail "(l) show did not report ch01 as unreadable: $out"
echo "$out" | grep -q 'ch02 ' \
    || fail "(l) show aborted before printing ch02 after an unreadable row: $out"

if [[ $FAILED -ne 0 ]]; then
    echo "ff400.sh alias handling: FAILED"
    exit 1
fi
echo "ff400.sh alias handling: all cases as expected"
