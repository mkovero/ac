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

provider() {
  jq -e 'select(.type=="thread.started")' "$f" >/dev/null 2>&1 \
    && echo codex || echo claude
}

# Collapse newlines: a heredoc in a Bash command otherwise becomes several
# rows, and every downstream cut/sort counts its lines as tool names.
tools() {
  if [[ $(provider) == codex ]]; then
    jq -r 'select((.type=="item.started" and .item.type=="command_execution")
                   or (.type=="item.completed" and .item.type=="file_change"))
           | .item
           | if .type=="command_execution" then
               "Bash\t" + ((.command // "") | gsub("[\\r\\n]+"; " ") | .[0:120])
             elif .type=="file_change" then
               "Edit\t" + ((.changes // []) | tostring | .[0:120])
             else empty end' "$f"
    return
  fi
  jq -r 'select(.type=="assistant") | .message.content[]?
         | select(.type=="tool_use")
         | .name + "\t" +
           ((.input.file_path // .input.pattern // .input.command // .input.description // "")
            | tostring | gsub("[\r\n]+"; " ") | .[0:120])' "$f"
}

files() {
  if [[ $(provider) == codex ]]; then
    jq -r 'select(.type=="item.completed" and .item.type=="file_change")
           | .item.changes[]?.path // empty' "$f" | sort -u
    return
  fi
  jq -r 'select(.type=="assistant") | .message.content[]?
         | select(.type=="tool_use" and (.name=="Edit" or .name=="Write" or .name=="NotebookEdit"))
         | .input.file_path // empty' "$f" | sort -u
}

text() {
  if [[ $(provider) == codex ]]; then
    jq -r 'select(.type=="item.completed" and .item.type=="agent_message")
           | .item.text // empty' "$f"
    return
  fi
  jq -r 'select(.type=="assistant") | .message.content[]?
         | select(.type=="text") | .text' "$f"
}

# The final message is the session's actual output. Prefer the result event;
# fall back to the last assistant text, which is the same content, because
# not every version emits result into the stream.
final() {
  local r
  if [[ $(provider) == codex ]]; then
    jq -rs '[.[] | select(.type=="item.completed" and .item.type=="agent_message")
             | .item.text] | last // "(no agent message found)"' "$f"
    return
  fi
  r=$(jq -r 'select(.type=="result") | .result // empty' "$f")
  [[ -n $r ]] && { printf '%s\n' "$r"; return; }
  jq -rs '[.[] | select(.type=="assistant") | .message.content[]?
           | select(.type=="text") | .text] | last // "(no assistant text found)"' "$f"
}

errors() {
  if [[ $(provider) == codex ]]; then
    jq -r 'select(.type=="error" or .type=="turn.failed")
           | "ERROR " + (.message // .error.message // tostring)' "$f"
    return
  fi
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
  if [[ $(provider) == codex ]]; then
    echo "weight view is not available in Codex JSONL" >&2
    return 2
  fi
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
  if [[ $(provider) == codex ]]; then
    jq -r --arg f "$(basename "$f")" '
      select(.type=="turn.completed") | [$f, "?", "?", "?"] | @tsv' "$f"
    return
  fi
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
  delegate)
    if [[ $(provider) == codex ]]; then
      jq -r 'select(.type=="item.completed" and .item.type=="mcp_tool_call")
             | select((.item.server // "") | test("collaboration"; "i"))
             | "\(.item.tool): \(.item.arguments | tostring | .[0:400])"' "$f"
    else
      jq -r 'select(.type=="assistant") | .message.content[]?
             | select(.type=="tool_use" and (.name|test("Agent|Task";"i")))
             | "\(.name): \(.input | tostring | .[0:400])"' "$f"
    fi ;;
  summary)
    echo "== provider =="
    echo "  $(provider)"
    echo
    echo "== tool counts =="
    tools | cut -f1 | sort | uniq -c | sort -rn
    echo
    echo "== files written =="
    files | sed 's/^/  /' || true
    echo
    echo "== delegation =="
    if [[ $(provider) == codex ]]; then
      n=$(jq -r 'select(.type=="item.completed" and .item.type=="mcp_tool_call")
                 | select((.item.server // "") | test("collaboration"; "i"))
                 | .item.tool' "$f" | wc -l)
    else
      n=$(jq -r 'select(.type=="assistant") | .message.content[]?
                 | select(.type=="tool_use" and (.name|test("Agent|Task";"i"))) | .name' "$f" | wc -l)
    fi
    echo "  subagent calls: $n"
    echo
    echo "== cost =="
    if [[ $(provider) == codex ]]; then
      echo "  Codex JSONL does not report USD cost or elapsed time"
    else
      jq -r 'select(.type=="result")
             | "  turns=\(.num_turns // "?") cost=\(.total_cost_usd // "?") dur=\((.duration_ms // 0)/1000|floor)s"' "$f" \
        | grep . || echo "  no result event — session incomplete, or this version does not emit one"
    fi
    echo
    echo "== final =="
    final | head -40
    ;;
  *) echo "unknown view: $view" >&2; exit 2 ;;
esac
