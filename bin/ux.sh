#!/usr/bin/env bash
# ux.sh [issue|pr] [--fg]
#
# UX role. No argument → sweeps every open needs-ux issue.
# Read-only against the tree: it writes a design comment, not code.
# Replaces ux-run.sh, which was interactive-only and took no argument.

source "$(dirname "$0")/common.sh"

arg="${1:-}"
if [[ -n $arg && $arg != --* ]]; then
  shift
  tag="$arg"
  prompt="Review #$arg in $AC_REPO and write your design comment on it."
else
  ids=$(gh issue list -R "$AC_REPO" --state open --label needs-ux \
        --json number --jq 'map("#\(.number)")|join(" ")')
  [[ -n $ids && $ids != '""' ]] || { echo "nothing labelled needs-ux"; exit 0; }
  echo "reviewing: $ids" >&2
  tag="sweep"
  prompt="Review these needs-ux items in $AC_REPO, one at a time, in order: $ids"
fi

AC_TAG="$tag" run ux "$prompt" --read "$@"
