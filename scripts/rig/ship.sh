#!/usr/bin/env bash
# ship.sh — copy a staged build to a rig and prove by sha256 that the rig
# holds exactly what was built.
#
#   scripts/rig/ship.sh <rig> [rev|latest] [--install]
#
# - copies binaries + MANIFEST.txt + SHA256SUMS to <stage_base>/<rev>-x86_64/
# - verifies sha256 on the rig against the manifest (fails loudly)
# - points it_loopback_ir's compile-time daemon path at the shipped daemon
# - --install: stops any ac-daemon, installs ac + ac-daemon to
#   /usr/local/bin, and verifies those by sha256 too
#
# Emits no audio. Procedure: docs/runbooks/rig-testing.md.

source "$(dirname "$0")/lib.sh"

install=0
args=()
for a in "$@"; do
    case $a in
        --install) install=1 ;;
        *) args+=("$a") ;;
    esac
done
load_rig "${args[0]:-}"
rev="$(resolve_rev "${args[1]:-latest}")"
stage="$(stage_root)/$rev"
dest="$(rig_dest "$rev")"
files=(ac ac-daemon ir_probe transfer_probe it_loopback_ir MANIFEST.txt SHA256SUMS)

(cd "$stage" && sha256sum --quiet -c SHA256SUMS) ||
    die "local stage $stage no longer matches its own SHA256SUMS"

note "shipping $rev to $RIG_NAME:$dest"
rig_ssh "mkdir -p '$dest'"
rig_scp "${files[@]/#/$stage/}" "$RIG_USER@$RIG_HOST:$dest/"

note "verifying sha256 on $RIG_NAME"
rig_ssh "cd '$dest' && sha256sum -c SHA256SUMS" ||
    die "sha256 mismatch on $RIG_NAME — do not run anything from $dest"

daemon_path="$(sed -n 's/^it_loopback_ir_daemon_path=//p' "$stage/MANIFEST.txt")"
[[ -n $daemon_path ]] || die "MANIFEST.txt has no it_loopback_ir_daemon_path"
rig_ssh "mkdir -p '$(dirname "$daemon_path")' && ln -sfn '$dest/ac-daemon' '$daemon_path'"
linked="$(rig_ssh "readlink -f '$daemon_path'")"
[[ $linked == "$dest/ac-daemon" ]] ||
    die "it_loopback_ir daemon path $daemon_path resolves to $linked, not $dest/ac-daemon"

if ((install)); then
    note "installing to /usr/local/bin on $RIG_NAME (stopping ac-daemon first)"
    rig_ssh "pkill -x ac-daemon || true; for i in 1 2 3 4 5; do pgrep -x ac-daemon >/dev/null || break; sleep 1; done; ! pgrep -x ac-daemon >/dev/null" ||
        die "ac-daemon still running on $RIG_NAME — refusing to install over it"
    rig_ssh "sudo -n install -m 755 '$dest/ac' '$dest/ac-daemon' /usr/local/bin/"
    want="$(grep -E '  (ac|ac-daemon)$' "$stage/SHA256SUMS" | awk '{print $1}' | sort)"
    got="$(rig_ssh "sha256sum /usr/local/bin/ac /usr/local/bin/ac-daemon" | awk '{print $1}' | sort)"
    [[ $want == "$got" ]] || die "installed /usr/local/bin binaries do not match $rev's SHA256SUMS"
fi

cat <<EOF

### build under test ($RIG_NAME)

- rev: $(sed -n 's/^rev=//p' "$stage/MANIFEST.txt") (dirty files: $(sed -n 's/^dirty_files=//p' "$stage/MANIFEST.txt"))
- rustflags: $(sed -n 's/^rustflags=//p' "$stage/MANIFEST.txt")
- staged on rig: $dest — sha256 verified on the rig against SHA256SUMS
- it_loopback_ir daemon path: $daemon_path -> $linked
- installed to /usr/local/bin: $([[ $install == 1 ]] && echo "yes, sha256 verified" || echo "no")

\`\`\`
$(cat "$stage/SHA256SUMS")
\`\`\`
EOF
