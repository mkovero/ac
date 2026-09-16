#!/usr/bin/env bash
# build-portable.sh — build rig-portable `ac` binaries for the checked-out
# commit and stage them with a sha256 manifest.
#
# Run on the development host, from any worktree. Never build on a rig.
#
#   scripts/rig/build-portable.sh [--allow-dirty]
#
# Stages into $AC_HOME/target-rig-stage/<rev12>/:
#   ac  ac-daemon  ir_probe  transfer_probe  it_loopback_ir
#   MANIFEST.txt  SHA256SUMS  build.log  test-build.log
#
# Procedure: docs/runbooks/rig-testing.md.

source "$(dirname "$0")/lib.sh"

allow_dirty=0
case ${1:-} in
    --allow-dirty) allow_dirty=1 ;;
    "") ;;
    *) die "usage: $0 [--allow-dirty]" ;;
esac

top="$(git rev-parse --show-toplevel)"
rev_full="$(git -C "$top" rev-parse HEAD)"
rev="${rev_full:0:12}"
# Untracked files count too — Cargo builds from the working filesystem, not
# just the index, so an untracked build.rs or source file can change the
# compiled binary without a tracked diff to show for it. --untracked-files
# defaults to "normal" (one line per untracked file, or per untracked dir)
# and still respects .gitignore, so target/ stays out of the count.
dirty="$(git -C "$top" status --porcelain -- ac-rs | wc -l)"
if ((dirty > 0 && allow_dirty == 0)); then
    die "ac-rs/ has $dirty uncommitted change(s) — commit them, or pass --allow-dirty (recorded in the manifest)"
fi

stage="$(stage_root)/$rev"
artefacts=(ac ac-daemon ir_probe transfer_probe it_loopback_ir)
mkdir -p "$stage"
# A failed earlier run must not leave old binaries beside a new manifest.
(cd "$stage" && rm -f "${artefacts[@]}" MANIFEST.txt SHA256SUMS build.log test-build.log test-build.jsonl)

# One target dir per commit. A target dir shared across worktrees can report
# `Finished` without compiling and hand back another ref's binaries.
export CARGO_TARGET_DIR="${RIG_TARGET_DIR:-$(ac_home)/target-rig-$rev}"

# Portable CPU baseline — rigs reject target-cpu=native builds with SIGILL.
# RUSTFLAGS replaces ac-rs/.cargo/config.toml's rustflags wholesale, so the
# mold linker flag is restated here.
export RUSTFLAGS="-C target-cpu=x86-64 -C link-arg=-fuse-ld=mold"

cd "$top/ac-rs" || die "no ac-rs/ under $top"
note "building $rev (dirty files: $dirty) into $CARGO_TARGET_DIR"

# --bins as well as --examples: a target-selection flag alone builds only
# the targets it names, so `--examples` by itself silently skips `ac`.
if ! cargo build --release -p ac-cli -p ac-daemon --bins --examples >"$stage/build.log" 2>&1; then
    tail -30 "$stage/build.log" >&2
    die "cargo build failed — full log $stage/build.log"
fi
if ! cargo test --release -p ac-daemon --test it_loopback_ir --no-run \
    --message-format=json >"$stage/test-build.jsonl" 2>"$stage/test-build.log"; then
    tail -30 "$stage/test-build.log" >&2
    die "it_loopback_ir build failed — full log $stage/test-build.log"
fi

# build_compiled_this_run (lib.sh) covers any workspace crate, not just
# ac-daemon — a rebuild that only touches ac-cli or an ac-core change that
# never reaches the daemon's inputs is still a real rebuild, and
# compiled_this_run must not read "no" for it.
compiled="$(build_compiled_this_run "$stage/build.log" "$stage/test-build.log")"

itbin="$(python3 - "$stage/test-build.jsonl" <<'EOF'
import json, sys
exes = []
for line in open(sys.argv[1]):
    if not line.startswith("{"):
        continue
    e = json.loads(line).get("executable")
    if e and "/it_loopback_ir-" in e:
        exes.append(e)
print(exes[-1] if exes else "")
EOF
)"
[[ -n $itbin ]] || die "no it_loopback_ir executable in cargo's JSON output"

rel="$CARGO_TARGET_DIR/release"
for f in "$rel/ac" "$rel/ac-daemon" "$rel/examples/ir_probe" "$rel/examples/transfer_probe" "$itbin"; do
    [[ -x $f ]] || die "expected build output missing: $f"
done
cp -f "$rel/ac" "$rel/ac-daemon" "$rel/examples/ir_probe" "$rel/examples/transfer_probe" "$stage/"
cp -f "$itbin" "$stage/it_loopback_ir"

# it_loopback_ir spawns the daemon at its compile-time path
# (env!("CARGO_BIN_EXE_ac-daemon")). ship.sh recreates that path on the rig.
daemon_path="$rel/ac-daemon"
grep -aqF "$daemon_path" "$stage/it_loopback_ir" ||
    die "it_loopback_ir does not embed $daemon_path — cargo changed how it locates the daemon; fix ship.sh before shipping"

(cd "$stage" && sha256sum "${artefacts[@]}" >SHA256SUMS)

cat >"$stage/MANIFEST.txt" <<EOF
rev=$rev_full
dirty_files=$dirty
built_at_utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)
build_host=$(uname -n)
rustc=$(rustc -V)
rustflags=$RUSTFLAGS
cargo_target_dir=$CARGO_TARGET_DIR
compiled_this_run=$compiled
it_loopback_ir_daemon_path=$daemon_path
EOF

note "staged $stage"
cat "$stage/MANIFEST.txt"
cat "$stage/SHA256SUMS"
if [[ $compiled == no ]]; then
    note "no 'Compiling ac-*' line: this target dir already held $rev's build. Fine for a rerun of the same commit; if RIG_TARGET_DIR is shared across refs, distrust these hashes."
fi
