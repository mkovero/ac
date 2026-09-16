# .agents/

Agent specs for `ac` repo. Each file define role, inputs, outputs, hard constraints.

## agents

| file | role | trigger |
|---|---|---|
| `.agents/triage.md` | PM — writes specs, routes issues, owns the scope label | new issue opened; any issue it touches that has no scope label |
| `.agents/architect.md` | design review — resolves module/interface questions | issue labeled `needs-design` |
| `.agents/ux.md` | output-surface design — what the operator sees, and in what units | issue labeled `needs-ux` |
| `.agents/developer.md` | implementation — one issue per invocation | issue labeled `ready-to-implement` |
| `.agents/qa.md` | PR review — spec coverage, correctness, tests, standards | PR opened |
| `.agents/codex-qa.md` | independent second review, run under Codex | PR is `claude-approved` and not `codex-approved`; or a runner recheck after a Codex-finding revision |
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
| `tier-1` `tier-2` `scene` `view` `scope-none` | triage, architect corrects, qa raises | exactly one. `tier-1` = a standard in the document map governs correctness, so qa runs the standards check. Unlabelled is a triage gap and reads as `tier-1`. qa may raise a label to `tier-1`, never lower one |
| `in-review` | developer (via PR) | PR open |
| `claude-approved` | qa (step 5, approve verdict); runner after a Codex recheck pass | Claude QA passed **at the commit it reviewed**, with no pending rig gate — or at an earlier commit whose only successors answer a Codex finding and passed a Codex recheck (the runner's comment names both) |
| `codex-approved` | codex-qa (pass verdict) | independent Codex QA passed at the commit it reviewed, with no pending rig gate |
| `needs-work` | qa **or** codex-qa | PR has issues, developer must revise |
| `blocked` | any agent | this issue waits on something else — see below |
| `blocks-others` | any agent | other work waits on **this** issue |
| `epic` | triage | contains sub-issues |
| `requires-rig` | qa | correctness rests on a measurement only the rig can make — blocks both approval labels; human clears it after the measurement exists |
| `agent:triage` | triage | audit trail |
| `agent:architect` | architect | audit trail |
| `agent:dev` | developer | audit trail |
| `agent:qa` | qa | audit trail |

### the two approval labels are one gate each, and neither is the merge gate

`claude-approved` and `codex-approved` are set by different reviewers running
under different models, and both must be present for a human to merge (see
human gates). Neither agent may set the other's label.

One carry-forward exists. After Codex fails a tip Claude approved, the
revision goes back to Codex alone (`codex-qa.md` → recheck mode). On a recheck
pass the runner restores `claude-approved` and comments that Claude QA
approved the base commit and did not review the delta. `AC_CODEX_RECHECK=0`
turns this off.

Neither approval label may be applied while `requires-rig` is present. Tree QA
defines the measurement and stops at the rig gate; after the measurement is
recorded and a human clears `requires-rig`, QA runs again at the same commit.

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

## bounded reading discipline — every role

Reading a file means obtaining enough direct evidence for the decision at
hand; it does not mean printing every byte of every named file. Start with the
diff, named symbols, headings, or a search restricted to the already-authorised
paths, then open the surrounding region. Read a small file in full when that is
cheaper. Expand into callers, adjacent sections, or the full file only when the
local context leaves a concrete question unanswered.

Do not batch-dump large files. Tool-output truncation in one batch does not
invalidate files or regions that were returned successfully, and is not a
reason to reread them. Continue only from the missing region. A required read
order governs the first inspection of each source; it does not require an
exhaustive linear scan before useful work begins.

This rule narrows reading cost, not evidence. A location cited in a durable
comment must still have been opened, and a scope manifest remains the boundary
for what may be changed.

## headless sessions — every role

Pipeline roles run non-interactively (`bin/*.sh` → `claude -p` or `codex exec`).
The session has **no later turn**: when your reply ends, the process exits. Nothing
wakes it for a finished background command.

- **Never background a command whose result you need.** Gate commands
  (`cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`)
  run in the foreground, one call each, with the tool timeout raised to its
  maximum. A fresh target dir makes each one take minutes — still foreground.
- **Deliverable before the turn ends.** The review comment, the push, the PR
  comment, the labels — whatever your role produces must already exist when you
  stop. "Waiting for the background run" is the end of the session, with
  nothing posted.
- **A step that cannot finish in one call** (e.g. over the timeout): say so in
  your comment and give the verdict the evidence supports. Don't park it.

Concrete bad output, 2026-09-15, PR #437 — three rounds lost:
- qa: *"Holding here — clippy compiling full workspace in a fresh isolated target dir … Will resume automatically once it finishes."* → no review.
- developer: *"Standing by for the `cargo test --workspace` background run to finish before posting the PR comment."* → fix written, never committed or pushed.
- qa: *"Waiting on background gate run — will continue the review once notified."* → no review.

## updating specs
Agent specs are code. Change via PR like anything else. Spec make bad output → fix live in spec: tighten constraints, or add concrete example of bad behavior to relevant section.
