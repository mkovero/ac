# .agents/

Agent specs for `ac` repo. Each file define role, inputs, outputs, hard constraints.

## agents

| file | role | trigger |
|---|---|---|
| `.agents/triage.md` | PM — writes specs, routes issues | new issue opened |
| `.agents/architect.md` | design review — resolves module/interface questions | issue labeled `needs-design` |
| `.agents/ux.md` | output-surface design — what the operator sees, and in what units | issue labeled `needs-ux` |
| `.agents/developer.md` | implementation — one issue per invocation | issue labeled `ready-to-implement` |
| `.agents/qa.md` | PR review — spec coverage, correctness, tests, standards | PR opened |
| `.agents/codex-qa.md` | independent second review, run under Codex | PR is `claude-approved` and not `codex-approved` |
| `.agents/rig.md` | hardware-in-the-loop verification — measurement record, interlocks | manual invocation |

## routing logic

```
new issue
  └─ triage
       ├─ needs-design → architect → ready-to-implement
       ├─ needs-ux     → ux        → ready-to-implement
       └─ ready-to-implement → developer → PR → qa → codex-qa → human merge

ambiguous issue
  └─ triage applies needs-clarification → wait for reporter

design wrong rather than code
  └─ qa or developer applies needs-design / needs-ux ON THE ISSUE
       └─ architect or ux revises its decision → ready-to-implement
            └─ back to the same PR, re-reviewed in full
```

## human gates
Always human-only:
- Merging PRs to main 
- Deleting branches
- Changing agent spec files
- Removing `requires-rig` — an agent cannot take the measurement, so it cannot retire the requirement for one

## label schema

| label | set by | meaning |
|---|---|---|
| `needs-design` | triage, **qa or developer** | architect must review — see the handback section |
| `needs-ux` | triage, architect, **qa or developer** | output surface must be specified before implementation — see the handback section |
| `needs-discussion` | architect | human input needed |
| `design-approved` | architect | design decided, ready for dev |
| `ready-to-implement` | triage, architect or ux | developer can pick up |
| `tier-1` `tier-2` `scene` `view` `scope-none` | triage, architect corrects | what the change touches; exactly one. `tier-1` is what makes qa run the standards check — an unlabelled issue is a triage gap, and qa treats it as `tier-1` |
| `in-review` | developer (via PR) | PR open |
| `claude-approved` | qa (step 5, approve verdict) | Claude QA passed **at the commit it reviewed** |
| `codex-approved` | codex-qa (pass verdict) | independent Codex QA passed at the commit it reviewed |
| `needs-work` | qa **or** codex-qa | PR has issues, developer must revise |
| `blocked` | any agent | this issue waits on something else — see below |
| `blocks-others` | any agent | other work waits on **this** issue |
| `epic` | triage | contains sub-issues |
| `requires-rig` | qa | correctness rests on a measurement only the rig can make — human clears it after the measurement exists |
| `agent:triage` | triage | audit trail |
| `agent:architect` | architect | audit trail |
| `agent:dev` | developer | audit trail |
| `agent:qa` | qa | audit trail |

### the two approval labels are one gate each, and neither is the merge gate

`claude-approved` and `codex-approved` are set by different reviewers running
under different models, and both must be present for a human to merge (see
human gates). Neither agent may set the other's label.

**Whoever applies `blocked` names the exact condition that lifts it**, in the
comment that applies it: *"#180 merged → remove `blocked`"*. #181 and #182 are
the established form.

## evidence discipline — every role

**A mechanism an agent proposes is a hypothesis with a test attached. Prefer
the test.** Where a number is derived rather than measured, the derivation is
usually right in *form* and wrong about *which quantity it applies to*. Say
what measurement would separate your explanation from an equally plausible
one, and rank that above the explanation.

**Provenance tag — the rule above given a name, so it travels with a numeric
acceptance criterion instead of living only in this section.** triage and architect tag each numeric acceptance criterion with one of:

- `measured` — a value read off a rig, a test run, or an existing recorded
  result. Claims: this number was observed, not inferred.
- `derived` — a value computed from other known quantities by a stated
  formula. Claims: the formula is right and applies to this quantity — the
  exact claim the rule above asks you to test rather than trust.
- `assumed` — a value chosen without either measurement or derivation
  (a round number, a guess at a reasonable bound, an unexamined carry-over
  from a similar criterion). Claims: nothing yet: this is the tag with the
  least evidence behind it.

An untagged numeric criterion defaults to `assumed` — the default fails
toward more scrutiny, not less. 

## updating specs
Agent specs are code. Change via PR like anything else. Spec make bad output → fix live in spec: tighten constraints, or add concrete example of bad behavior to relevant section.
