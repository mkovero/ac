#!/usr/bin/env bash
# ac-gc.sh [--yes] [--debug]
#
# Reclaim disk from worktrees, target dirs and transcripts. Dry run unless
# --yes. Never touches a worktree with uncommitted or unpushed work.

# Deliberately NOT `set -e`: this walks over things that may be missing, half
# removed, or not in git at all, and an abort partway through is worse than a
# line of noise. Errors are checked where they matter.
set -uo pipefail

source "$(dirname "$0")/common.sh" || exit 1
set +e

KEEP_DAYS="${AC_KEEP_DAYS:-30}"
GO=""; DEBUG=""
for a in "$@"; do
  case "$a" in
    --yes)   GO=1 ;;
    --debug) DEBUG=1; set -x ;;
  esac
done

act()  { if [[ -n $GO ]]; then eval "$1"; else echo "    would: $1"; fi; }
size() { du -sh "$1" 2>/dev/null | cut -f1; }

echo "== worktrees"
mapfile -t WTS < <(git worktree list --porcelain 2>/dev/null | sed -n 's/^worktree //p')
echo "   (${#WTS[@]} found)"

for wt in "${WTS[@]}"; do
  [[ "$wt" == "$ROOT" ]] && { echo "  MAIN $wt"; continue; }
  if [[ ! -d $wt ]]; then
    echo "  DEAD $wt (directory gone)"
    continue
  fi

  br="$(git -C "$wt" branch --show-current 2>/dev/null)"
  dirty="$(git -C "$wt" status --porcelain 2>/dev/null | wc -l)"
  unpushed=0
  if [[ -n $br ]]; then
    unpushed="$(git -C "$wt" rev-list --count "origin/$br..$br" 2>/dev/null)"
    [[ -z $unpushed ]] && unpushed=0
  fi

  if (( dirty > 0 )) || (( unpushed > 0 )); then
    echo "  KEEP $(size "$wt")  $wt  [$br] $dirty uncommitted, $unpushed unpushed"
    continue
  fi

  state=""
  if [[ -n $br ]]; then
    state="$(gh_retry gh pr list -R "$AC_REPO" --state all --head "$br" \
             --json state --jq '.[0].state // empty' 2>/dev/null)"
  fi

  case "$state" in
    MERGED|CLOSED)
      echo "  GONE $(size "$wt")  $wt  [$br] PR $state"
      act "git worktree remove --force '$wt'" ;;
    OPEN)
      echo "  KEEP $(size "$wt")  $wt  [$br] PR open" ;;
    *)
      echo "  STALE $(size "$wt")  $wt  [$br] no PR, nothing local"
      act "git worktree remove --force '$wt'" ;;
  esac
done
act "git worktree prune"

echo
echo "== shared target dir"
if [[ -d $AC_TARGET ]]; then
  echo "  $(size "$AC_TARGET")  $AC_TARGET"
  echo "  (shared by every worktree — 'cargo clean' it, do not delete per branch)"
else
  echo "  (none yet)"
fi

echo
echo "== leftover per-branch target dirs"
# From the old layout, where each branch had its own. Removable once the branch
# is gone; nothing references them now.
found=0
for d in "$AC_HOME"/target/*/; do
  [[ -d $d ]] || continue
  b="$(basename "$d")"
  [[ $b == debug || $b == release || $b == tmp || $b == .rustc_info.json ]] && continue
  if [[ -d "$d/debug" || -d "$d/tmp" ]]; then
    found=1
    echo "  GONE $(size "$d")  $b (old per-branch layout)"
    act "rm -rf '$d'"
  fi
done
(( found )) || echo "  (none)"

echo
echo "== stray target dirs"
# Scan only this project's worktree base and its own checkout. dirname of
# WT_BASE is the whole src directory — that walks every unrelated repo there.
# CARGO_TARGET_DIR should keep these empty; anything here predates it or came
# from a cargo run that did not inherit it. Reported, never auto-removed: one
# could be a build you are part way through.
STRAY=()
for base in "$WT_BASE" "$ROOT"; do
  [[ -d $base ]] || continue
  mapfile -t -O "${#STRAY[@]}" STRAY < <(
    find "$(realpath "$base")" -maxdepth 3 -type d -name target 2>/dev/null)
done
if (( ${#STRAY[@]} )); then
  for t in "${STRAY[@]}"; do
    if [[ $t == "$(realpath "$ROOT")"/* ]]; then
      echo "  $(size "$t")  $t  (main checkout — expected if you build by hand)"
    else
      echo "  $(size "$t")  $t"
    fi
  done
  echo "  (not removed automatically — rm -rf them yourself if idle)"
else
  echo "  (none)"
fi

echo
echo "== transcripts older than ${KEEP_DAYS}d"
mapfile -t OLD < <(find "$AC_LOG_DIR" -name '*.jsonl' -mtime "+$KEEP_DAYS" 2>/dev/null)
if (( ${#OLD[@]} )); then
  for f in "${OLD[@]}"; do
    echo "  GONE $(size "$f")  $(basename "$f")"
    act "rm -f '$f'"
  done
else
  echo "  (none)"
fi

echo
echo "== total under $AC_HOME"
du -sh "$AC_HOME" 2>/dev/null || echo "  (none yet)"
echo
df -h "$AC_HOME" 2>/dev/null | tail -1 || df -h "$HOME" | tail -1
if [[ -z $GO ]]; then echo; echo "dry run. rerun with --yes to act."; fi
