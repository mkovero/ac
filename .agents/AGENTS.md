# .agents/

Agent specs for `ac` repo. Each file define role, inputs, outputs, hard constraints.

## agents

| file | role | trigger |
|---|---|---|
| `triage.md` | PM — writes specs, routes issues | new issue opened |
| `.agents/architect.md` | design review — resolves module/interface questions | issue labeled `needs-design` |
| `.agents/developer.md` | implementation — one issue per invocation | issue labeled `ready-to-implement` |
| `.agents/qa.md` | PR review — spec coverage, correctness, tests, standards | PR opened |
| `audit.md` | audit coordinator — orchestrates full codebase audit | manual invocation |

## invocation

### claude code (manual)
Pass agent file as context beside issue or PR:

```bash
# full audit (run specialists in sequence, then coordinator)
claude "audit the codebase as architect" --context .agents/architect.md > audit/architect-raw.md
claude "audit the codebase as qa"        --context .agents/qa.md        > audit/qa-raw.md
claude "You are the audit coordinator. Read .agents/audit.md then read audit/architect-raw.md and audit/qa-raw.md and produce the consolidated audit report."
  "triage issue #42: https://github.com/mkovero/ac/issues/42"

# implement a ready issue
claude --context .agents/developer.md \
  "implement issue #42"

# review an open PR
claude --context .agents/qa.md \
  "review PR #43: https://github.com/mkovero/ac/pull/43"
```

Claude Code need GitHub MCP server connected for issue/PR read-write:
```bash
claude mcp add github -- npx -y @modelcontextprotocol/server-github
export GITHUB_TOKEN=your_pat
```

### github actions (automated)
Use agent file contents as system prompt in workflow step.
Example trigger: label applied → run triage or developer agent.
See `.github/workflows/` for workflow definitions (if present).

## routing logic

```
new issue
  └─ triage
       ├─ needs-design → architect → ready-to-implement
       └─ ready-to-implement → developer → PR → qa → human merge

ambiguous issue
  └─ triage applies needs-clarification → wait for reporter
```

PRs touching stimulus/drive (`set_drive`, arm/fire state machine, keepalive):
  apply `drive-path` → qa use drive-path safety checklist; wire-protocol side
  route to architect as usual.

## human gates
Always human-only:
- Merging PRs to main
- Closing issues
- Deleting branches
- Changing agent spec files

## label schema

| label | set by | meaning |
|---|---|---|
| `needs-clarification` | triage | waiting on reporter |
| `needs-design` | triage | architect must review |
| `needs-discussion` | architect | human input needed |
| `design-approved` | architect | design decided, ready for dev |
| `ready-to-implement` | triage or architect | developer can pick up |
| `in-review` | developer (via PR) | PR open |
| `needs-work` | qa | PR has issues, developer must revise |
| `blocked` | any agent | external dependency |
| `epic` | triage | contains sub-issues |
| `drive-path` | triage or developer | stimulus/drive safety checklist applies |
| `agent:triage` | triage | audit trail |
| `agent:architect` | architect | audit trail |
| `agent:dev` | developer | audit trail |
| `agent:qa` | qa | audit trail |

## evidence discipline — every role

**A mechanism an agent proposes is a hypothesis with a test attached. Prefer
the test.** Where a number is derived rather than measured, the derivation is
usually right in *form* and wrong about *which quantity it applies to*. Say
what measurement would separate your explanation from an equally plausible
one, and rank that above the explanation.

Provenance: ten wrong inferences in this project share one shape — a plausible
mechanism asserted without the measurement that would distinguish it. None was
caught by reasoning; all were caught by the rig, by someone reading the code,
or by an agent scoring data. `ρ = 1/6` survived four checkpoints because each
verified the arithmetic rather than whether the formula applied, then reached
the code via a QA brief. That path is what this rule closes.

Corollaries:

- **A check that cannot fail is worth less than no check**, because it reports
  coverage it does not have. Before writing a checklist item or an acceptance
  criterion, name the case that makes it come back negative. If none exists,
  do not write the item.
- **A document cited as an independent specification input must not be folded
  into what it checks.** Lifting a ratified decision out of an expiring handoff
  into `docs/` is right — except where another document re-derives expectations
  *against* it (`work/qa/qa-brief-218-222.md:10` names
  `work/handoff/handoff-live-display-switch.md` that way, under an explicit rule
  against reading values from the implementation). Merging it into its own
  subject destroys the independence. Before deleting any document, grep for it
  as a **cited name**, not only as a subject — those are different searches, and
  only the second one is load-bearing.
- **Prose does not hold issue state.** Name the issue; let the tracker own
  open/closed. A document that restates it can be true when written and false
  half an hour later, which no review catches.

## updating specs
Agent specs are code. Change via PR like anything else. Spec make bad output → fix live in spec: tighten constraints, or add concrete example of bad behavior to relevant section.