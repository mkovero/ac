#!/usr/bin/env bash
# stale_names.sh [--base <rev>] [--head <rev>] [<dir>]
#
# Names a diff removed, still described somewhere in the tree (#554).
#
# Four review rounds in a row (PRs #538, #547, #553) were blocked only because
# prose went on describing, in the present tense, a constant, variant, field
# or rule the same diff had deleted. fmt, clippy and the tests do not read
# prose, and a reviewer reads the diff, while the stale text sits outside it.
# This step is the mechanical half; the QA checklist is the other.
#
# Inputs, both searched in the whole <head> tree except docs/superseded/:
#
#   symbol    Every item (fn const static struct enum union trait type mod
#             macro_rules!) and every enum variant or named struct field
#             defined in the <base> version of a *.rs file that base..head
#             changes, deletes or renames away, minus every name still
#             defined in any *.rs file of <head>. A name that moved or is
#             defined elsewhere therefore drops out by construction.
#   declared  One literal per line from the file $AC_SUPERSEDED_NAMES — the
#             design's **superseded names** field (common.sh →
#             superseded_names_of). Symbols or phrases; case-sensitive, not
#             word-bounded, matched against whole paragraphs so a phrase
#             split by a line break is still found.
#
# Symbols match as whole words. A variant or field that is one all-lowercase
# word with no `_` (`window`) matches only inside backticks or after `::`/`.`,
# so ordinary prose does not collide with it.
#
# A paragraph is a maximal run of non-blank lines. In *.rs and *.sh only
# comment lines (// /// //! or #) join into paragraphs; a code line is its own.
#
# Exemption: a mention whose paragraph cites #$AC_ISSUE — the issue this change
# implements — is printed as `cited #N` and not reported. Text citing the
# change that removed a name was written for that change; stale text predates
# the issue and cannot cite it. With AC_ISSUE unset nothing is exempt (more
# red, never less).
#
# What passing does NOT mean: a false claim about something that still exists
# (#538's citation, #547's observability claim) has no removed name to find,
# and a declared list catches only the wording someone thought to write down.
#
# --head defaults to HEAD, --base to `git merge-base <head> origin/main`.
# Reads git objects only; the working tree is never consulted.
#
# Output: one `names` line in bin/gate.sh's layout, then the names checked and
# every mention, reported or cited. Exit 0 no reported mention, 1 at least
# one, 2 refused (bad rev, unreadable names file, AC_ISSUE not a number).

set -euo pipefail

usage() { sed -n '2,49p' "$0"; }
line() { printf '  %-7s %-16s %s\n' names "$1" "$2"; }
refuse() {
  echo "stale_names: refused — $*" >&2
  line REFUSED "$*"
  exit 2
}

base="" head=HEAD dir=.
while (($#)); do
  case "$1" in
    --base) base="${2:-}"; shift ;;
    --head) head="${2:-}"; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) refuse "unknown option $1" ;;
    *) dir="$1" ;;
  esac
  shift
done

top="$(git -C "$dir" rev-parse --show-toplevel 2>/dev/null)" || refuse "$dir is not in a git worktree"
cd "$top"

head_sha="$(git rev-parse -q --verify "$head^{commit}")" || refuse "--head $head is not a commit"
if [[ -z $base ]]; then
  base_sha="$(git merge-base "$head_sha" origin/main 2>/dev/null)" \
    || refuse "no merge-base of $head and origin/main; pass --base"
else
  base_sha="$(git rev-parse -q --verify "$base^{commit}")" || refuse "--base $base is not a commit"
fi

issue="${AC_ISSUE:-}"
[[ -z $issue || $issue =~ ^[0-9]+$ ]] || refuse "AC_ISSUE=$issue is not an issue number"

declare -a declared=()
if [[ -n ${AC_SUPERSEDED_NAMES:-} ]]; then
  [[ -f $AC_SUPERSEDED_NAMES && -r $AC_SUPERSEDED_NAMES ]] \
    || refuse "AC_SUPERSEDED_NAMES=$AC_SUPERSEDED_NAMES is not a readable file"
  while IFS= read -r l || [[ -n $l ]]; do
    l="${l#"${l%%[![:space:]]*}"}"; l="${l%"${l##*[![:space:]]}"}"
    [[ -z $l || $l == none ]] && continue
    declared+=("$l")
  done < "$AC_SUPERSEDED_NAMES"
fi

if [[ $base_sha == "$head_sha" ]]; then
  line PASS "(no diff)"
  exit 0
fi

# --- definitions ---------------------------------------------------------------
# One Rust file on stdin → `name<TAB>item|member`, one per definition. A brace
# tracker, not a parser: raw strings and braces in block comments can fool it.
# A missed head-side definition shows up as a false report (visible, red); a
# spurious one hides a mention — the accepted residual.
# shellcheck disable=SC2016
EXTRACT='
function ident(s,   v) {
  if (!match(s, /^(r#)?[A-Za-z_][A-Za-z0-9_]*/)) return ""
  v = substr(s, RSTART, RLENGTH); sub(/^r#/, "", v); return v
}
function out(name, cls) { if (name != "" && name != "_") print name "\t" cls }
function noattr(t) {
  while (t ~ /^#!?\[/) { if (!sub(/^#!?\[[^]]*\][ \t]*/, "", t)) break }
  return t
}
{
  if ($0 ~ /^[ \t]*\/\//) next
  s = $0
  gsub(/\\\\/, "", s); gsub(/\\"/, "", s); gsub(/\\'"'"'/, "", s)
  gsub(/"[^"]*"/, "\"\"", s)
  gsub(/'"'"'[^'"'"']'"'"'/, "'"''"'", s)
  sub(/\/\/.*/, "", s)

  if (body && depth == body) {
    t = s; sub(/^[ \t]+/, "", t); t = noattr(t)
    if (bodykind == "enum") { if (t ~ /^[A-Z]/) out(ident(t), "member") }
    else {
      sub(/^pub(\([^)]*\))?[ \t]+/, "", t)
      if (t ~ /^(r#)?[A-Za-z_][A-Za-z0-9_]*[ \t]*:([^:]|$)/) out(ident(t), "member")
    }
  }

  t = s; sub(/^[ \t]+/, "", t); t = noattr(t)
  for (;;) {
    if (sub(/^pub(\([^)]*\))?[ \t]+/, "", t)) continue
    if (sub(/^(async|unsafe|default)[ \t]+/, "", t)) continue
    if (sub(/^extern[ \t]+(""[ \t]+)?/, "", t)) continue
    if (t ~ /^const[ \t]+(fn|async|unsafe|extern)[ \t]/) { sub(/^const[ \t]+/, "", t); continue }
    break
  }
  if (match(t, /^(fn|const|static|struct|enum|union|trait|type|mod)[ \t]+/)) {
    kind = substr(t, 1, RLENGTH); gsub(/[ \t]/, "", kind)
    r = substr(t, RLENGTH + 1); sub(/^mut[ \t]+/, "", r)
    out(ident(r), "item")
    if (kind == "struct" || kind == "enum" || kind == "union") pending = (kind == "enum" ? "enum" : "struct")
  } else if (match(t, /^macro_rules![ \t]*/)) {
    out(ident(substr(t, RLENGTH + 1)), "item")
  }

  if (s !~ /[{};]/) next
  n = length(s)
  for (i = 1; i <= n; i++) {
    c = substr(s, i, 1)
    if (c == "{") { depth++; if (pending != "") { body = depth; bodykind = pending; pending = "" } }
    else if (c == "}") { depth--; if (body && depth < body) body = 0 }
    else if (c == ";") pending = ""
  }
}'

defs_of() { git show "$1:$2" 2>/dev/null | awk "$EXTRACT"; }

declare -A base_cls=() head_has=()
declare -a head_files=()
while IFS=$'\t' read -r st a b; do
  while IFS=$'\t' read -r nm cls; do
    [[ ${base_cls[$nm]:-} == item ]] || base_cls[$nm]="$cls"
  done < <(defs_of "$base_sha" "$a")
  case "$st" in
    M*) head_files+=("$a") ;;
    R*) head_files+=("$b") ;;
  esac
done < <(git diff --name-status -M --diff-filter=DMR "$base_sha" "$head_sha" -- '*.rs')

# Still defined in the head version of the changed files …
for f in "${head_files[@]+"${head_files[@]}"}"; do
  while IFS=$'\t' read -r nm _; do head_has[$nm]=1; done < <(defs_of "$head_sha" "$f")
done
declare -a cand=()
for nm in "${!base_cls[@]}"; do [[ -n ${head_has[$nm]:-} ]] || cand+=("$nm"); done
# … or anywhere else in the head tree. Only files that mention a candidate can
# define it, so extract just those.
if ((${#cand[@]})); then
  declare -a pats=()
  for nm in "${cand[@]}"; do pats+=(-e "$nm"); done
  while IFS= read -r f; do
    f="${f#"$head_sha":}"
    while IFS=$'\t' read -r nm _; do head_has[$nm]=1; done < <(defs_of "$head_sha" "$f")
  done < <(git grep -I -l -w -F "${pats[@]}" "$head_sha" -- '*.rs' || true)
fi
declare -a removed=()
for nm in "${cand[@]+"${cand[@]}"}"; do [[ -n ${head_has[$nm]:-} ]] || removed+=("$nm"); done
if ((${#removed[@]})); then mapfile -t removed < <(printf '%s\n' "${removed[@]}" | LC_ALL=C sort); fi

# --- mentions ------------------------------------------------------------------
# One file on stdin, the names in SN_NAMES as `word|restricted|declared<TAB>name`
# lines → `line<TAB>name<TAB>symbol|declared<TAB>cited|reported`.
# shellcheck disable=SC2016
SCAN='
function isid(c) { return c ~ /^[A-Za-z0-9_]$/ }
function wordin(s, w,   p, j, at) {
  p = 0
  while ((j = index(substr(s, p + 1), w)) > 0) {
    at = p + j
    if (!isid(substr(s, at - 1, 1)) && !isid(substr(s, at + length(w), 1))) return 1
    p = at
  }
  return 0
}
function restricted(s, w,   seg, m, x) {
  m = split(s, seg, "`")
  for (x = 2; x < m; x += 2) if (wordin(seg[x], w)) return 1
  return wordin2(s, "::" w) || wordin2(s, "." w)
}
function wordin2(s, w,   p, j, at) {  # w already carries its left context
  p = 0
  while ((j = index(substr(s, p + 1), w)) > 0) {
    at = p + j
    if (!isid(substr(s, at + length(w), 1))) return 1
    p = at
  }
  return 0
}
function squash(s) { gsub(/[ \t]+/, " ", s); sub(/^ /, "", s); sub(/ $/, "", s); return s }
function emit(i, k) {
  if ((i SUBSEP k) in done) return
  done[i, k] = 1
  print i "\t" N[k] "\t" (K[k] == "declared" ? "declared" : "symbol") "\t" (cited ? "cited" : "reported")
}
BEGIN {
  m = split(ENVIRON["SN_NAMES"], raw, "\n")
  for (i = 1; i <= m; i++) {
    if (raw[i] == "") continue
    split(raw[i], f, "\t"); nn++; K[nn] = f[1]; N[nn] = f[2]; P[nn] = squash(f[2])
  }
  code = ""
  if (path ~ /\.rs$/) code = "rs"; else if (path ~ /\.sh$/) code = "sh"
}
{ L[NR] = $0 }
END {
  for (i = 1; i <= NR; i++) {
    t = L[i]
    if (code == "rs" && match(t, /^[ \t]*\/\/[\/!]?/)) { T[i] = substr(t, RLENGTH + 1); Y[i] = "c" }
    else if (code == "sh" && match(t, /^[ \t]*#/))    { T[i] = substr(t, RLENGTH + 1); Y[i] = "c" }
    else { T[i] = t; Y[i] = (code == "" ? "c" : "x") }
    if (T[i] ~ /^[ \t]*$/) Y[i] = "b"
  }
  i = 1
  while (i <= NR) {
    if (Y[i] == "b") { i++; continue }
    a = i
    if (Y[i] == "c") while (i + 1 <= NR && Y[i + 1] == "c") i++
    b = i; i++
    joined = ""
    for (j = a; j <= b; j++) {
      s = squash(T[j])
      off[j] = length(joined) + (joined == "" ? 1 : 2)
      joined = joined (joined == "" ? "" : " ") s
    }
    cited = (issue != "" && joined ~ ("#" issue "([^0-9]|$)"))
    for (k = 1; k <= nn; k++) {
      if (K[k] == "declared") {
        if (P[k] == "") continue
        p = 0
        while ((j = index(substr(joined, p + 1), P[k])) > 0) {
          at = p + j; ln = a
          for (x = a; x <= b; x++) if (off[x] <= at) ln = x
          emit(ln, k); p = at
        }
      } else {
        for (x = a; x <= b; x++)
          if (K[k] == "word" ? wordin(L[x], N[k]) : restricted(L[x], N[k])) emit(x, k)
      }
    }
  }
}'

names=""
declare -a pats=()
for nm in "${removed[@]+"${removed[@]}"}"; do
  if [[ ${base_cls[$nm]} == member && $nm =~ ^[a-z][a-z0-9]*$ ]]; then kind=restricted; else kind=word; fi
  names+="$kind"$'\t'"$nm"$'\n'
  pats+=(-e "$nm")
done
for d in "${declared[@]+"${declared[@]}"}"; do
  names+="declared"$'\t'"$d"$'\n'
  # The longest word of a phrase is on one line wherever the phrase breaks.
  longest=""
  for w in $d; do (( ${#w} > ${#longest} )) && longest="$w"; done
  pats+=(-e "$longest")
done

declare -a hits=()
if ((${#pats[@]})); then
  while IFS= read -r f; do
    f="${f#"$head_sha":}"
    while IFS=$'\t' read -r ln nm input verdict; do
      hits+=("$f"$'\t'"$ln"$'\t'"$nm"$'\t'"$input"$'\t'"$verdict")
    done < <(git show "$head_sha:$f" | SN_NAMES="$names" awk -v path="$f" -v issue="$issue" "$SCAN")
  done < <(git grep -I -l -F "${pats[@]}" "$head_sha" -- . ':(exclude)docs/superseded/' || true)
fi

reported=0 cited=0
for h in "${hits[@]+"${hits[@]}"}"; do
  if [[ ${h##*$'\t'} == cited ]]; then cited=$((cited + 1)); else reported=$((reported + 1)); fi
done

status=PASS; ((reported == 0)) || status=FAIL
if [[ -n $issue ]]; then summary="$reported reported, $cited cited #$issue"
else summary="$reported reported (AC_ISSUE unset: nothing exempt)"; fi
line "$status" "$summary; base ${base_sha:0:12}, ${#removed[@]} removed symbol(s), ${#declared[@]} declared"

if ((${#removed[@]})); then
  printf '    removed: %s\n' "$(printf '%s ' "${removed[@]:0:30}")$( ((${#removed[@]} > 30)) && echo "… (+$(( ${#removed[@]} - 30 )))" )"
fi
if ((${#hits[@]})); then
  echo "--- names mentions (read each; a reported line describes a removed name):"
  printf '%s\n' "${hits[@]}" | LC_ALL=C sort -t$'\t' -k1,1 -k2,2n | while IFS=$'\t' read -r f ln nm input verdict; do
    if [[ $verdict == cited ]]; then
      printf '  %s:%s  %s  (%s, cited #%s)\n' "$f" "$ln" "$nm" "$input" "$issue"
    else
      printf '  %s:%s  %s  (%s)\n' "$f" "$ln" "$nm" "$input"
    fi
  done
fi
((reported == 0))
