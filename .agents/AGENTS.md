# .agents/

Agent specs for the `measuring` repo. Each file defines a role, its inputs,
what it must produce, and its hard constraints.

## agents

| file | role | trigger |
|---|---|---|
| `triage.md` | PM — writes specs, routes issues | new issue opened |
| `architect.md` | design review — resolves module/interface questions | issue labeled `needs-design` |
| `ux.md` | UX design — output format, display, information hierarchy | issue labeled `needs-ux` or any PR touching output formatting |
| `developer.md` | implementation — one issue per invocation | issue labeled `ready-to-implement` |
| `qa.md` | PR review — spec coverage, correctness, tests, standards | PR opened |
| `audit.md` | audit coordinator — orchestrates full codebase audit | manual invocation |

## invocation

### claude code (manual)
Pass the agent file as context alongside the issue or PR:

```bash
# full audit (run specialists in sequence, then coordinator)
claude "audit the codebase as architect" --context .agents/architect.md > audit/architect-raw.md
claude "audit the codebase as ux"        --context .agents/ux.md        > audit/ux-raw.md
claude "audit the codebase as qa"        --context .agents/qa.md        > audit/qa-raw.md
claude "You are the audit coordinator. Read .agents/audit.md then read audit/architect-raw.md, audit/ux-raw.md, and audit/qa-raw.md and produce the consolidated audit report."
  "triage issue #42: https://github.com/mkovero/measuring/issues/42"

# implement a ready issue
claude --context .agents/developer.md \
  "implement issue #42"

# review an open PR
claude --context .agents/qa.md \
  "review PR #43: https://github.com/mkovero/measuring/pull/43"
```

Claude Code needs the GitHub MCP server connected for issue/PR read-write:
```bash
claude mcp add github -- npx -y @modelcontextprotocol/server-github
export GITHUB_TOKEN=your_pat
```

### github actions (automated)
Use the agent file contents as the system prompt in a workflow step.
Example trigger: label applied → run triage or developer agent.
See `.github/workflows/` for workflow definitions (if present).

## routing logic

```
new issue
  └─ triage
       ├─ needs-design → architect → ready-to-implement
       ├─ needs-ux → ux → ready-to-implement (or needs-design if structural)
       └─ ready-to-implement → developer → PR → qa → human merge

ambiguous issue
  └─ triage applies needs-clarification → wait for reporter
```

## human gates
These actions are always human-only:
- Merging PRs to main
- Closing issues
- Deleting branches
- Changing agent spec files

## label schema

| label | set by | meaning |
|---|---|---|
| `needs-clarification` | triage | waiting on reporter |
| `needs-design` | triage | architect must review |
| `needs-ux` | triage | UX agent must produce display design |
| `needs-discussion` | architect or ux | human input needed |
| `design-approved` | architect | design decided, ready for dev |
| `ux-approved` | ux | display design decided, ready for dev |
| `ready-to-implement` | triage, architect, or ux | developer can pick up |
| `in-review` | developer (via PR) | PR open |
| `needs-work` | qa | PR has issues, developer must revise |
| `blocked` | any agent | external dependency |
| `epic` | triage | contains sub-issues |
| `agent:triage` | triage | audit trail |
| `agent:architect` | architect | audit trail |
| `agent:dev` | developer | audit trail |
| `agent:qa` | qa | audit trail |

## updating specs
Agent specs are code. Change them via PR like anything else. When a spec
produces bad output, the fix is in the spec — improve the constraints or
add a concrete example of the bad behavior to the relevant section.
