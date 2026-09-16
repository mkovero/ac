#!/usr/bin/env bash
# gate.sh [--force] [<dir>]
#
# The workspace gate, run once per tree:
#
#   cargo fmt --check
#   cargo clippy --workspace --all-targets -- -D warnings
#   cargo test --workspace   (as cargo nextest run + cargo test --doc, see below)
#
# The result is recorded under $AC_GATE_DIR/<key>/, where the key is the
# commit's tree hash plus the toolchain. Any later call for the same tree
# (developer before push, the runner before QA, QA itself, Codex QA) prints the
# record and exits with its status instead of running cargo again.
#
# Why: transcripts from 2026-08/09 showed cargo taking 35–43% of developer and
# qa wall time, most of it `cargo test --workspace` run two or three times per
# commit — once per role, and again after `| tail` hid the exit status.
#
# Key on the whole tree, not ac-rs/: tests read ../../ZMQ.md and the stddocs
# symlink. A docs-only commit therefore re-runs the gate; a rebase that leaves
# the tree identical does not.
#
# Refuses a dirty tree (exit 2): the record would describe HEAD while cargo
# built something else. Commit first.
#
# Tests run under cargo-nextest when it is installed: one process per test,
# all cores, about half the wall time of `cargo test --workspace` (122 s → 53 s
# on the dev VM, 2026-09-16). nextest does not run doctests, so
# `cargo test --doc --workspace` runs after it. Without nextest the step is
# plain `cargo test --workspace`; the record names which ran.
# ac-rs/.config/nextest.toml holds the serial groups the port-allocating
# ac-cli integration tests need.
#
# Exit: 0 all three pass, 1 any failed, 2 refused.
# Output: one line per step, then the failing lines of any red step, then the
# log paths. Read the logs by path — do not re-run to see more output.

source "$(dirname "$0")/common.sh"

force="" dir="."
for a in "$@"; do
  case "$a" in
    --force) force=1 ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) dir="$a" ;;
  esac
done

wt="$(git -C "$dir" rev-parse --show-toplevel 2>/dev/null)" \
  || { echo "gate: $dir is not in a git worktree" >&2; exit 2; }
cd "$wt/ac-rs" || { echo "gate: $wt has no ac-rs/" >&2; exit 2; }

# Tracked changes anywhere (tests read files outside ac-rs/), plus untracked
# files under ac-rs/ (a new module file changes the build). Untracked files
# elsewhere — review drafts, the gitignored .cargo/ and stddocs link — do not.
dirty="$(git status --porcelain --untracked-files=no -- "$wt"; git status --porcelain -- .)"
if [[ -n $dirty ]]; then
  echo "gate: refused — uncommitted changes; the record would not describe HEAD:" >&2
  printf '%s\n' "$dirty" | head -20 >&2
  exit 2
fi

sha="$(git rev-parse HEAD)"
tree="$(git rev-parse 'HEAD^{tree}')"
toolchain="$(rustc -V 2>/dev/null)"
key="$tree-$(printf '%s' "$toolchain" | sha256sum | cut -c1-12)"
rec="$AC_GATE_DIR/$key"

export CARGO_TARGET_DIR
CARGO_TARGET_DIR="$(prepare_target "$wt")" || exit 2

field() { sed -n "s/^$1=//p" "$2/result"; }

report() {
  local r="$1" s code first
  first="$(field sha "$r")"
  echo "gate: HEAD $sha, tree ${tree:0:12}, $toolchain, tests via $(field runner "$r")"
  [[ $first == "$sha" ]] || echo "      reused: recorded at commit $first, which has the identical tree"
  for s in fmt clippy test; do
    code="$(field "$s" "$r")"
    if [[ $code == 0 ]]; then code=PASS; else code="FAIL (exit $code)"; fi
    printf '  %-7s %-16s %5ss  %s\n' "$s" "$code" "$(field "${s}_s" "$r")" "$r/$s.log"
  done
  for s in fmt clippy test; do
    [[ $(field "$s" "$r") == 0 ]] && continue
    echo "--- $s failing lines (full log above; read it, do not re-run):"
    case "$s" in
      fmt)    grep -E '^Diff in ' "$r/$s.log" | head -30 || true ;;
      clippy) grep -E -A6 '^(error|warning)(\[|:)' "$r/$s.log" | head -60 || true ;;
      test)   grep -E '^ *(FAIL|SIGSEGV|SIGABRT|TIMEOUT) \[|^test .* FAILED$|^---- |panicked at|^error(\[|:)|test result: FAILED' "$r/$s.log" | head -60 || true ;;
    esac
  done
  echo "  record: $r/result"
  [[ $(field pass "$r") == 1 ]]
}

mkdir -p "$AC_GATE_DIR"
exec 9>"$AC_GATE_DIR/$key.lock"
if ! flock -n 9; then
  echo "gate: another run holds tree ${tree:0:12} — waiting for it" >&2
  flock 9
fi

if [[ -f $rec/result && -z $force ]]; then
  if report "$rec"; then exit 0; else exit 1; fi
fi

tmp="$(mktemp -d "$AC_GATE_DIR/.run-XXXXXX")"
declare -A rc secs
step() {
  local name="$1"; shift
  local t0=$SECONDS
  echo "gate: $name — $*" >&2
  if "$@" >"$tmp/$name.log" 2>&1; then rc[$name]=0; else rc[$name]=$?; fi
  secs[$name]=$(( SECONDS - t0 ))
}
step fmt    cargo fmt --check
step clippy cargo clippy --workspace --all-targets -- -D warnings
if cargo nextest --version >/dev/null 2>&1; then
  runner=nextest
  run_tests() {
    cargo nextest run --workspace --no-fail-fast --hide-progress-bar || return
    cargo test --doc --workspace
  }
else
  runner="cargo test (cargo-nextest not installed)"
  run_tests() { cargo test --workspace; }
fi
step test   run_tests

pass=0
(( rc[fmt] == 0 && rc[clippy] == 0 && rc[test] == 0 )) && pass=1
{
  echo "sha=$sha"
  echo "tree=$tree"
  echo "toolchain=$toolchain"
  echo "worktree=$wt"
  echo "target=$CARGO_TARGET_DIR"
  echo "runner=$runner"
  echo "finished=$(date -Iseconds)"
  for s in fmt clippy test; do echo "$s=${rc[$s]}"; echo "${s}_s=${secs[$s]}"; done
  echo "pass=$pass"
} > "$tmp/result"

rm -rf "$rec"
mv "$tmp" "$rec"
if report "$rec"; then exit 0; else exit 1; fi
