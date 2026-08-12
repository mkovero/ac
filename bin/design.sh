#!/usr/bin/env bash
# design.sh [issue] [--fg]
#
# Architect role. No argument → sweeps every open needs-design issue.
# Read-only against the tree: architect decides, developer implements.
# GitHub tools stay available so it can post its comment and move labels.

source "$(dirname "$0")/common.sh"

arg="${1:-}"
if [[ -n $arg && $arg != --* ]]; then
  shift
  tag="issue-$arg"
  prompt="Review issue #$arg in $AC_REPO and produce a design decision."
else
  ids=$(gh issue list -R "$AC_REPO" --state open --label needs-design \
        --json number --jq 'map("#\(.number)")|join(" ")')
  [[ -n $ids && $ids != '""' ]] || { echo "nothing labelled needs-design"; exit 0; }
  echo "reviewing: $ids" >&2
  tag="sweep"
  prompt="Review these needs-design issues in $AC_REPO, one at a time, in order: $ids"
fi

AC_TAG="$tag" run architect "$prompt" --read "$@"
