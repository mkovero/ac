#!/usr/bin/env bash
# triage.sh [issue | --scope] [--fg]
#
# Triage role. No argument → sweeps every open issue nothing has routed yet.
# Read-only against the tree: triage writes a spec comment and labels, never
# code. GitHub tools stay available so it can post and label.
#
# --scope backfills the scope label ONLY, and is deliberately not the same pass
# as the sweep above. The sweep selects unrouted issues, so it can never reach
# an issue that is already `ready-to-implement` or `needs-design` — which is
# most of the backlog the scope label was added after. Running the ordinary
# per-issue triage over those instead would re-derive a spec that already
# exists, on issues that are already specced and in some cases already have an
# open PR. `triage.md` says a missing scope label is worth one line of comment,
# not a re-triage, so this mode carries a prompt that says exactly that.

source "$(dirname "$0")/common.sh"

SCOPE_LABELS="tier-1 tier-2 scene view scope-none"

arg="${1:-}"
if [[ $arg == --scope ]]; then
  shift
  # Same negation trick as the sweep: --label is an OR over presence and cannot
  # express absence, so the search qualifier list has to carry it.
  q=""; for l in $SCOPE_LABELS; do q="$q -label:$l"; done
  # Routed issues only — `label:a,b` is an OR, unlike repeated --label. These
  # are the ones heading for a PR, so they are the ones whose missing label
  # would cost a QA session a standards check it did not need. A backlog issue
  # gets its label when triage promotes it, which is the ordinary path and
  # needs no backfill; sweeping those too would triple the work for issues
  # nobody is about to review. Pass --scope-all to take every unlabelled issue.
  [[ ${1:-} == --scope-all ]] && { shift; scope_filter=""; } \
                              || scope_filter=" label:ready-to-implement,needs-design,needs-ux"
  ids=$(gh_retry gh issue list -R "$AC_REPO" --state open --limit 100 \
        --search "$q$scope_filter" --json number --jq 'map("#\(.number)")|join(" ")')
  [[ -n $ids && $ids != '""' ]] || { echo "no unlabelled issue in scope"; exit 0; }
  echo "scope backfill: $ids" >&2
  tag="scope"
  prompt="Backfill the scope label on these issues in $AC_REPO, one at a time,
in order: $ids

For each: read the issue, decide which ONE scope label fits
($(echo $SCOPE_LABELS | tr ' ' '/')), apply it, and post a single-line comment
saying which label and why in one clause.

Do NOT write or edit a spec comment. Do NOT change any other label. Do NOT
reassess whether the issue is actionable or correctly routed — these issues are
already triaged and several have open PRs. The scope label is the only output.
If an issue genuinely does not let you tell tier-1 from the rest, apply tier-1:
that is the documented bias, and the cost is a standards check nobody needed."
elif [[ -n $arg && $arg != --* ]]; then
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
