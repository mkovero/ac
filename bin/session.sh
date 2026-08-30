#!/usr/bin/env bash
# session.sh <jsonl> [view]
#
# Read a raw session transcript. Views:
#   summary  (default)  what ran, cost, files touched
#   tools               tool call trace with the argument that matters
#   text                the model's reasoning between tool calls
#   files                files edited or written, sorted unique
#   delegate            explorer/subagent calls and their briefs
#   final               last assistant message — the distilled output
#   errors              failed tool results and rate limit events
#   weight              context each tool put in front of the model, by result bytes
#   cost                one TSV line: file, turns, usd, seconds
#   types               event type histogram, for when a filter stops matching

set -euo pipefail
f="${1:?usage: session.sh <jsonl> [summary|tools|text|files|delegate|final|errors|weight|cost|types]}"
view="${2:-summary}"

# Collapse newlines: a heredoc in a Bash command otherwise becomes several
# rows, and every downstream cut/sort counts its lines as tool names.
tools() {
  jq -r 'select(.type=="assistant") | .message.content[]?
         | select(.type=="tool_use")
         | .name + "\t" +
           ((.input.file_path // .input.pattern // .input.command // .input.description // "")
            | tostring | gsub("[\r\n]+"; " ") | .[0:120])' "$f"
}

files() {
  jq -r 'select(.type=="assistant") | .message.content[]?
         | select(.type=="tool_use" and (.name=="Edit" or .name=="Write" or .name=="NotebookEdit"))
         | .input.file_path // empty' "$f" | sort -u
}

text() {
  jq -r 'select(.type=="assistant") | .message.content[]?
         | select(.type=="text") | .text' "$f"
}

# The final message is the session's actual output. Prefer the result event;
# fall back to the last assistant text, which is the same content, because
# not every version emits result into the stream.
final() {
  local r
  r=$(jq -r 'select(.type=="result") | .result // empty' "$f")
  [[ -n $r ]] && { printf '%s\n' "$r"; return; }
  jq -rs '[.[] | select(.type=="assistant") | .message.content[]?
           | select(.type=="text") | .text] | last // "(no assistant text found)"' "$f"
}

errors() {
  jq -r 'select(.type=="user") | .message.content[]?
         | select(.type=="tool_result" and (.is_error == true))
         | "ERROR " + (.content | tostring | .[0:300])' "$f"
  jq -r 'select(.type=="rate_limit_event")
         | "RATE LIMIT " + (. | tostring | .[0:200])' "$f"
}

# How much context each tool actually put in front of the model, by result
# size. Call counts are misleading on their own: one tool returning 40 kB of
# prose costs more than twenty Greps returning line numbers. This is the view
# that settles "is <tool> earning its place" — it is a measurement, not an
# impression, and it is the only one that separates a locator from a payload.
weight() {
  jq -rs '
    ( [ .[] | select(.type=="assistant") | .message.content[]?
        | select(.type=="tool_use") | {key: .id, value: .name} ]
      | from_entries ) as $name
    | [ .[] | select(.type=="user") | .message.content[]?
        | select(.type=="tool_result")
        | {name: ($name[.tool_use_id] // "unknown"),
           n:    (.content | tostring | length)} ]
    | group_by(.name)
    | map({name: .[0].name, calls: length, bytes: (map(.n) | add)})
    | sort_by(-.bytes)
    | (["BYTES","CALLS","TOOL"], (.[] | [.bytes, .calls, .name]))
    | @tsv' "$f"
}

# One line per session, for aggregating across a whole log directory:
#   for j in ~/src/ac-wt/log/*.jsonl; do bin/session.sh "$j" cost; done | sort -k3 -rn
cost() {
  jq -r --arg f "$(basename "$f")" '
    select(.type=="result")
    | [$f, (.num_turns // "?"), (.total_cost_usd // "?"),
       ((.duration_ms // 0) / 1000 | floor)]
    | @tsv' "$f"
}

case "$view" in
  tools)    tools ;;
  text)     text ;;
  files)    files ;;
  final)    final ;;
  errors)   errors ;;
  weight)   weight ;;
  cost)     cost ;;
  types)    jq -r '.type' "$f" | sort | uniq -c | sort -rn ;;
  delegate) jq -r 'select(.type=="assistant") | .message.content[]?
                   | select(.type=="tool_use" and (.name|test("Agent|Task";"i")))
                   | "\(.name): \(.input | tostring | .[0:400])"' "$f" ;;
  summary)
    echo "== tool counts =="
    tools | cut -f1 | sort | uniq -c | sort -rn
    echo
    echo "== files written =="
    files | sed 's/^/  /' || true
    echo
    echo "== delegation =="
    n=$(jq -r 'select(.type=="assistant") | .message.content[]?
               | select(.type=="tool_use" and (.name|test("Agent|Task";"i"))) | .name' "$f" | wc -l)
    echo "  subagent calls: $n"
    echo
    echo "== cost =="
    jq -r 'select(.type=="result")
           | "  turns=\(.num_turns // "?") cost=\(.total_cost_usd // "?") dur=\((.duration_ms // 0)/1000|floor)s"' "$f" \
      | grep . || echo "  no result event — session incomplete, or this version does not emit one"
    echo
    echo "== final =="
    final | head -40
    ;;
  *) echo "unknown view: $view" >&2; exit 2 ;;
esac
