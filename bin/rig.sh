#!/usr/bin/env bash
# rig.sh <pr> [--fg]                 rig role against a PR's head (pipeline mode)
# rig.sh --lock "<who, what>"        take the rig lock; prints the token
# rig.sh --unlock <token>|--force    release it
# rig.sh --status                    show who holds it
#
# Pipeline mode (rig.md → pipeline mode): take the lock, build the PR head
# portable, ship it, run the rig role with the check QA named, and require a
# `<!-- agent: rig -->` record naming that head. The rig role emits only inside
# the operator's standing consent (AGENTS.md → rig sessions).
#
# Exit: 0 record posted · 3 rig busy (lock held) · 4 rig scripts or access
# missing · anything else: the session failed and posted no record at this head.
#
#   AC_RIG=pupu            rig profile name (scripts/rig/hosts/<rig>.env)
#   AC_RIG_SCRIPTS=<dir>   rig scripts (default: <checkout>/scripts/rig)
#   AC_RIG_LEASE_S=7200    lock lease; a lock past it may be broken

source "$(dirname "$0")/common.sh"

RIG="${AC_RIG:-pupu}"
export AC_HOME                       # scripts/rig reads it for stage and access paths
LEASE="${AC_RIG_LEASE_S:-7200}"
LOCK_DIR='ac-test/rig.lock'          # relative to the rig user's home
MOTD=/run/motd.d/ac-rig-lock         # tmpfs: a reboot clears the banner

access="$AC_HOME/rig-hosts/$RIG.access.env"
[[ -r $access ]] || { echo "rig access file missing: $access" >&2; exit 4; }
# shellcheck disable=SC1090
source "$access"
: "${RIG_HOST:?}" "${RIG_USER:?}" "${RIG_SSH_KEY:?}"

rig_ssh() {
  ssh -F /dev/null -i "$RIG_SSH_KEY" -o BatchMode=yes -o ConnectTimeout=10 \
    "$RIG_USER@$RIG_HOST" "$@"
}

# rig_script <args...> — run the heredoc on stdin remotely with these args.
# ssh joins its arguments into one string for the remote shell, so each one is
# quoted here; unquoted, a note with spaces shifts every later argument.
rig_script() {
  local q
  q="$(printf ' %q' "$@")"
  rig_ssh "bash -s --$q"
}

# The lock is a directory on the rig: mkdir is atomic, and every session —
# pipeline, manual, another dev host — reaches the rig, not each other.
# `holder` says who, `expires` bounds a crashed holder, `token` guards unlock.
# The banner is best-effort (needs passwordless sudo on the rig).
lock_take() {
  local note="$1"
  local token rc=0
  token="$(od -An -N6 -tx1 /dev/urandom | tr -d ' \n')"
  rig_script "$LOCK_DIR" "$LEASE" "$token" "$note" "$MOTD" <<'EOF' || rc=$?
set -u
dir="$HOME/$1" lease="$2" token="$3" note="$4" motd="$5"
now=$(date +%s)
mkdir -p "$(dirname "$dir")"
if ! mkdir "$dir" 2>/dev/null; then
  exp=$(cat "$dir/expires" 2>/dev/null || echo 0)
  if (( now <= exp )); then
    echo "rig busy: $(cat "$dir/holder" 2>/dev/null) (lease until $(date -u -d @"$exp" +%FT%TZ))" >&2
    exit 3
  fi
  echo "breaking expired lock: $(cat "$dir/holder" 2>/dev/null)" >&2
  rm -rf "$dir"; mkdir "$dir" || exit 3
fi
printf '%s\n' "$note" > "$dir/holder"
echo $(( now + lease )) > "$dir/expires"
printf '%s\n' "$token" > "$dir/token"
printf 'RIG LOCKED — %s\n  since %s, lease until %s. Do not emit or change audio state.\n  ~/%s\n' \
  "$note" "$(date -u +%FT%TZ)" "$(date -u -d @$(( now + lease )) +%FT%TZ)" "$1" \
  | sudo -n install -D -m 644 /dev/stdin "$motd" 2>/dev/null || true
EOF
  (( rc == 0 )) || return "$rc"
  printf '%s\n' "$token"
}

lock_release() {
  local token="$1"
  rig_script "$LOCK_DIR" "$token" "$MOTD" <<'EOF'
set -u
dir="$HOME/$1" token="$2" motd="$3"
[[ -d $dir ]] || { echo "rig not locked" >&2; exit 0; }
if [[ $token != --force && "$(cat "$dir/token" 2>/dev/null)" != "$token" ]]; then
  echo "lock is held by someone else: $(cat "$dir/holder" 2>/dev/null)" >&2
  exit 1
fi
rm -rf "$dir"
sudo -n rm -f "$motd" 2>/dev/null || true
EOF
}

lock_status() {
  rig_script "$LOCK_DIR" <<'EOF'
dir="$HOME/$1"
if [[ -d $dir ]]; then
  exp=$(cat "$dir/expires" 2>/dev/null || echo 0)
  state=live; (( $(date +%s) > exp )) && state=expired
  echo "locked ($state): $(cat "$dir/holder" 2>/dev/null), lease until $(date -u -d @"$exp" +%FT%TZ)"
else
  echo "unlocked"
fi
EOF
}

case "${1:-}" in
  --lock)   lock_take "${2:?usage: rig.sh --lock \"<who, what>\"}"; exit ;;
  --unlock) lock_release "${2:?usage: rig.sh --unlock <token>|--force}"; exit ;;
  --status) lock_status; exit ;;
  ""|-*)    echo "usage: rig.sh <pr> [--fg] | --lock <note> | --unlock <token> | --status" >&2; exit 2 ;;
esac

pr="$1"; shift
[[ $pr =~ ^[0-9]+$ ]] || { echo "not a PR number: $pr" >&2; exit 2; }

scripts="${AC_RIG_SCRIPTS:-$ROOT/scripts/rig}"
if [[ ! -x $scripts/build-portable.sh || ! -x $scripts/ship.sh ]]; then
  echo "rig scripts not found at $scripts — they land with PR #441; set AC_RIG_SCRIPTS" >&2
  exit 4
fi

head="$(gh_retry gh pr view "$pr" -R "$AC_REPO" --json headRefOid --jq .headRefOid)"
issue="$(gh_retry gh pr view "$pr" -R "$AC_REPO" --json closingIssuesReferences \
         --jq '.closingIssuesReferences[0].number // empty')"
rev="${head:0:12}"

token="$(lock_take "pipeline rig.sh PR #$pr @ $rev ($(uname -n) pid $$)")" || exit $?
wt="$WT_BASE/rig-pr-$pr"
cleanup() {
  cd "$ROOT" || true
  [[ -d $wt ]] && git worktree remove --force "$wt" >/dev/null 2>&1
  lock_release "$token" || echo "could not release the rig lock — token $token" >&2
}
trap cleanup EXIT

require_space "$wt" || exit 1
git fetch -q origin "pull/$pr/head"
[[ $(git rev-parse FETCH_HEAD) == "$head" ]] || { echo "PR #$pr moved while preparing" >&2; exit 1; }
[[ -e $wt ]] && git worktree remove --force "$wt" >/dev/null 2>&1
git worktree add --detach "$wt" "$head" >/dev/null
link_support "$wt"

# Build and ship here, not in the session: both are mechanical, and a rig
# session that builds is how the dev VM got OOM-killed once.
echo "rig: building $rev" >&2
( cd "$wt" && "$scripts/build-portable.sh" ) || { echo "portable build failed" >&2; exit 1; }
echo "rig: shipping $rev to $RIG" >&2
"$scripts/ship.sh" "$RIG" "$rev" || { echo "ship failed" >&2; exit 1; }

before="$(newest_record "$pr" rig)"
cd "$wt"
# The session writes its record inside its own worktree, the one directory it
# can write to; the runner files it below. No --add-dir: under --fg, run()
# puts extra args right before the prompt, and --add-dir is variadic — it
# swallowed the prompt and the session never started (#489, 2026-09-16).
record_in="$wt/rig-record.md"
record_out="$AC_SESSION_DIR/$(date +%F)-rig-pr-$pr-$rev.md"
rm -f "$record_in"
AC_TAG="rig-pr-$pr" run rig "Pipeline mode (rig.md → pipeline mode) for PR #$pr in $AC_REPO.

- head: $head
- issue: #${issue:-none}
- rig: $RIG — facts in docs/rigs/$RIG.md, profile $scripts/hosts/$RIG.env,
  access in $access
- scripts: $scripts (use them; they enforce the profile's ceilings)
- staged build: $rev, already built at this head, shipped, and sha256-verified
  on the rig
- the rig lock is held for you (token $token). Do not release it.

The check to run is the 'rig verification required' field of the newest
<!-- agent: qa --> record on the PR, together with the issue's 'rig check'
(architect or triage comment). Run exactly that. Emit only inside the standing
consent in AGENTS.md: typed levels at or below -40 dBFS and the profile's lower
ceilings, bounded commands only. Host reboots and snd_fireface reloads are
within that consent when the check needs them: afterwards make JACK reachable
(restart jack-ac if clients cannot connect), restore the FF400 baseline with
the toggle writes in docs/rigs/$RIG.md, and confirm it by probe-outputs.sh at
-60 dBFS before any capture; record each event with its time. The rig lock
lives on disk and survives a reboot; the login banner does not. No interface
power cycles and no physical changes (nobody is in the room): if the check
needs one, record decline for that part and name the permission.

Headless: run every command in the foreground, with a timeout. Write the
full record to $record_in (the runner files it under \$AC_HOME/session and
commits it; do not commit it here), and post the PR comment exactly as rig.md
specifies, ending with the rig verdict line." "$@" || true

if [[ -s $record_in ]]; then
  mkdir -p "$AC_SESSION_DIR"
  cp "$record_in" "$record_out"
  if git -C "$AC_HOME" rev-parse --git-dir >/dev/null 2>&1; then
    git -C "$AC_HOME" add "$record_out" || true
    if ! git -C "$AC_HOME" diff --cached --quiet -- "$record_out"; then
      git -C "$AC_HOME" commit -q -m "rig: PR #$pr at $rev (pipeline)" -- "$record_out" \
        || echo "rig: could not commit $record_out in \$AC_HOME" >&2
    fi
  fi
  echo "rig: record filed at $record_out" >&2
else
  echo "rig: session wrote no record file at $record_in" >&2
fi

after="$(newest_record "$pr" rig)"
if [[ -z $after || $after == "$before" || $after != *"$head"* ]]; then
  echo "rig session posted no record naming $head" >&2
  exit 1
fi
verdict="$(grep -oE '\*\*rig verdict:\*\* *(pass|fail|decline)' <<<"$after" | tail -1 | awk '{print $NF}' || true)"
echo "rig: record posted, verdict ${verdict:-missing}" >&2
[[ -n $verdict ]] || exit 1
