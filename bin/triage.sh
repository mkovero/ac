#!/usr/bin/env bash
# triage.sh [issue] [--fg]
#
# Triage role. No argument → sweeps every open issue nothing has routed yet.
# Read-only against the tree: triage writes a spec comment and labels, never
# code. GitHub tools stay available so it can post and label.

source "$(dirname "$0")/common.sh"

arg="${1:-}"
if [[ -n $arg && $arg != --* ]]; then
  shift
  tag="issue-$arg"
  prompt="Triage issue #$arg in $AC_REPO. Write the spec comment and apply labels."
else
  # `--label` is an OR over presence, so it cannot express absence. Search can.
  # The qualifier list is triage.md's own routing set: an issue carrying any of
  # them has already been routed by triage, architect, ux, or you.
  ids=$(gh_retry gh issue list -R "$AC_REPO" --state open --limit 100 \
        --search '-label:ready-to-implement -label:needs-design -label:needs-ux -label:needs-clarification -label:epic' \
        --json number --jq 'map("#\(.number)")|join(" ")')
  [[ -n $ids && $ids != '""' ]] || { echo "nothing unrouted"; exit 0; }
  echo "triaging: $ids" >&2
  tag="sweep"
  prompt="Triage these unrouted issues in $AC_REPO, one at a time, in order: $ids"
fi

AC_TAG="$tag" run triage "$prompt" --read "$@"
