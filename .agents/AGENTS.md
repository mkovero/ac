# .agents/

Agent specs for `ac` repo. Each file define role, inputs, outputs, hard constraints.

## agents

| file | role | trigger |
|---|---|---|
| `triage.md` | PM — writes specs, routes issues | new issue opened |
| `.agents/architect.md` | design review — resolves module/interface questions | issue labeled `needs-design` |
| `.agents/developer.md` | implementation — one issue per invocation | issue labeled `ready-to-implement` |
| `.agents/qa.md` | PR review — spec coverage, correctness, tests, standards | PR opened |
| `.agents/codex-qa.md` | independent second review, run under Codex | PR is `claude-approved` and not `codex-approved` |
| `.agents/rig.md` | hardware-in-the-loop verification — measurement record, interlocks | manual invocation |

## invocation

### claude code (manual)
Pass agent file as context beside issue or PR:

```bash
# triage a new issue
claude --context .agents/triage.md \
  "triage issue #42: https://github.com/mkovero/ac/issues/42"

# implement a ready issue
claude --context .agents/developer.md \
  "implement issue #42"

# review an open PR
claude --context .agents/qa.md \
  "review PR #43: https://github.com/mkovero/ac/pull/43"

# run a rig session (manual invocation, hardware-in-the-loop)
claude --system-prompt-file .agents/rig.md \
  "run rig session against work/rig/rig-verify-queue.md block 4"
```

### codex (independent QA)
The second review does not run under Claude — that is the point of it. Codex
reads the root `AGENTS.md` symlink automatically, so only the role file is
named:

```bash
codex exec "Independent QA review of PR #43 in mkovero/ac.
Read AGENTS.md, then .agents/codex-qa.md, then follow codex-qa.md."
```

`.agents/bin/codex-qa-run.sh` walks the whole queue. Root `AGENTS.md` is a
symlink to this file; see `.agents/codex-qa.md` for why the tooling must be
kept from writing through it.

Claude Code need GitHub MCP server connected for issue/PR read-write:
```bash
claude mcp add github -- npx -y @modelcontextprotocol/server-github
export GITHUB_TOKEN=your_pat
```

### github actions (automated)
Use agent file contents as system prompt in workflow step.
Example trigger: label applied → run triage or developer agent.

## routing logic

```
new issue
  └─ triage
       ├─ needs-design → architect → ready-to-implement
       └─ ready-to-implement → developer → PR → qa → codex-qa → human merge

ambiguous issue
  └─ triage applies needs-clarification → wait for reporter
```

PRs touching stimulus/drive (`set_drive`, arm/fire state machine, keepalive):
  apply `drive-path` → qa use drive-path safety checklist; wire-protocol side
  route to architect as usual.

## human gates
Always human-only:
- Merging PRs to main — an agent reviewing another agent's PR shares the same
  specs, the same failure modes, and the same blind spots, so that review is
  not an independent check; merge needs one. Codex QA reduces the common mode
  (different model, different harness, no sight of the Claude review until its
  own findings are formed) but does not remove it, so merge stays human.
  **Merge only when both `claude-approved` and `codex-approved` are present
  and both approve comments postdate the last commit on the branch.** A label
  is a claim about a tip; read the timestamps rather than trusting the label,
  same posture as the rig interlocks.
- Closing issues
- Deleting branches
- Changing agent spec files
- Removing `requires-rig` — an agent cannot take the measurement, so it cannot
  retire the requirement for one

## label schema

| label | set by | meaning |
|---|---|---|
| `needs-clarification` | triage | waiting on reporter |
| `needs-design` | triage | architect must review |
| `needs-discussion` | architect | human input needed |
| `design-approved` | architect | design decided, ready for dev |
| `ready-to-implement` | triage or architect | developer can pick up |
| `in-review` | developer (via PR) | PR open |
| `claude-approved` | qa (step 5, approve verdict) | Claude QA passed **at the commit it reviewed** |
| `codex-approved` | codex-qa (pass verdict) | independent Codex QA passed at the commit it reviewed |
| `needs-work` | qa **or** codex-qa | PR has issues, developer must revise |
| `blocked` | any agent | this issue waits on something else — see below |
| `blocks-others` | any agent | other work waits on **this** issue |
| `epic` | triage | contains sub-issues |
| `drive-path` | triage or developer | stimulus/drive safety checklist applies |
| `requires-rig` | qa | correctness rests on a measurement only the rig can make — human clears it after the measurement exists |
| `agent:triage` | triage | audit trail |
| `agent:architect` | architect | audit trail |
| `agent:dev` | developer | audit trail |
| `agent:qa` | qa | audit trail |

codex-qa has no `agent:` row. It sets `codex-approved` and `needs-work` and
nothing else; its `<!-- agent: codex-qa -->` comment marker is the audit trail,
so a fifth `agent:` label would be a second record of the same fact and one
more label for a reviewer to keep in sync.

### the two approval labels are one gate each, and neither is the merge gate

`claude-approved` and `codex-approved` are set by different reviewers running
under different models, and both must be present for a human to merge (see
human gates). Neither agent may set the other's label.

**Clearing is asymmetric, deliberately.** `codex-approved` is cleared by
codex-qa itself on a later fail. `claude-approved` is cleared by *whoever
pushes to the branch after it was applied* — the pusher, not the reviewer,
because the push is what invalidates it and the reviewer is not watching. This
is the label-level expression of qa.md's post-approval rule; the rule is the
authority, the label only tracks it.

A useful consequence, free rather than designed: QA never pairs
`claude-approved` with a request-changes verdict, so the label pair encodes
which reviewer objected.

- `needs-work` alone → a Claude QA finding.
- `claude-approved` + `needs-work` → a Codex finding, unambiguously.

`in-review` and `requires-rig` compose unchanged; Codex touches neither.
`in-review` + `needs-work` can now coexist, in the Codex-fail case only. That
is accepted drift: nothing currently routes on the *absence* of `in-review` as
"developer attention needed", and anything added later that does would break
here.

Only a fresh Claude QA pass restores `claude-approved`, so a Codex-failed PR
cannot re-enter the Codex queue until Claude QA has re-reviewed it. The queue
predicate is the interlock — there is no separate mechanism to keep in sync.

### `blocked` and `blocks-others` are opposite relations

They point in opposite directions and were previously named `blocked` and
`blocker`, one letter apart. `blocker` was renamed rather than retired: nine
issues carried it, and a rename preserves them where a delete would have
stripped them silently, with nothing in git to restore from.

- **`blocked`** — *this* issue cannot proceed yet.
- **`blocks-others`** — *other* work cannot proceed until this one lands.

An issue can legitimately carry both.

### the `blocked` lift condition — write it in the comment that applies it

`ready-to-implement` describes **spec completeness, not queue position**. A
spec-complete issue whose predecessor is unmerged is still ready in the sense
that label means, so it carries `blocked` as well.

**Whoever applies `blocked` names the exact condition that lifts it**, in the
comment that applies it: *"#180 merged → remove `blocked`"*. #181 and #182 are
the established form.

Two reasons this is a rule rather than a habit. A developer agent routing on
`ready-to-implement` alone would otherwise pick up work whose dependencies do
not exist yet. And a `blocked` label with no stated lift condition is
indistinguishable from one whose condition was met months ago — the label
stops being state and becomes sediment.

## evidence discipline — every role

**A mechanism an agent proposes is a hypothesis with a test attached. Prefer
the test.** Where a number is derived rather than measured, the derivation is
usually right in *form* and wrong about *which quantity it applies to*. Say
what measurement would separate your explanation from an equally plausible
one, and rank that above the explanation.

**Provenance tag — the rule above given a name, so it travels with a numeric
acceptance criterion instead of living only in this section.** `triage.md`
and `architect.md` tag each numeric acceptance criterion with one of:

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
toward more scrutiny, not less. `qa.md` step 1 branches on the tag: it
still is not licensed to re-litigate a `measured` criterion, but a
`derived` or `assumed` one gets asked what `ρ = 1/6`, the circular ±2-sample
tolerance, `((W−D)/W)²`, and the settle-anchored clock did not get asked in
time.

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
- **The common failure is not a wrong statement, it is a true one that
  decayed.** `handoff-doc-maintenance.md` was correct for thirty-five minutes.
  The `#[ignore]`d snapshot references were correct until #252 moved the
  layout. `#184`'s scope line was correct when the repo had three crates. A
  test-file header claiming four properties were unobservable headless was
  correct until `it_set_drive` covered three of them. None was wrong when
  written, so **no review at the time could have caught any of them** — which
  makes this a different class from asserting a mechanism without its
  measurement, and one that review cannot fix.

  The operational form: **do not restate what another artefact holds
  authoritatively; where you must, name the artefact and date the restatement.**
  Prefer describing what a file *enforces today* over enumerating its contents.
  `computes_nothing.rs` is the worked example — `architect.md` names it as
  authoritative and gives its current checks as dated commentary, so adding a
  fourth check makes the spec under-describe rather than mis-describe.

  Corollary for citations: **cite a section by name, not by line number.** A
  line range is invalidated by the next edit to the file, including the edit
  that adds the citation.
- **Added precision must come from a lookup, not from an inference.** The
  citation corollary above says where a cite should point; this says where the
  precision may come from. A bare `plot.rs:430` is ambiguous but true, and
  `ac-cli/src/commands/plot.rs:430` — inferred from the command name — is
  specific and false, because the code is in
  `ac-daemon/src/handlers/audio/plot.rs` and the CLI file is 212 lines long.
  Resolving an ambiguous cite is worth doing; resolving it from memory of the
  layout is not, and the result is *harder* to catch than the ambiguity it
  replaced, because the failure reads as diligence. Open the file, name the
  section, or leave the cite as it was.
- **Report the sign of an unscored gap.** Where a gap is left unscored — a
  check not run, a case not tested, a value not verified — say which direction
  its error would push a result, or say that the direction is unknown. An
  unscored gap with no stated direction reads as harmless; most are not.
- **A cite that was added is not a cite that was verified.** In a final
  artifact the two look identical: a reference resolved cleanly against the
  tree and one that was wrong and got corrected both appear as correct
  references. Only the second says anything about the draft's reliability, so
  counting additions as corrections inflates the apparent verification rate of
  the source document. When reporting what a verification pass found, separate
  *corrected*, *added*, and *checked and unchanged* — this is the project's own
  harm-statistic discipline turned on the verification process itself.

### repowise — a locator, with one exception

This repo is indexed by repowise, and its tools are available to every role.
They exist to cut the *exploration* cost — the candidate reads that find the
right file — not to replace the read that a finding rests on.

**The line:**

- **`get_symbol` returns raw source bytes with exact line bounds. When
  `_meta.indexed_commit` equals HEAD of the tree under review, a `get_symbol`
  result counts as having opened that span** — it *is* the tree, arrived at
  more cheaply than `Read` plus offset arithmetic. This is the exception, and
  it is conditional on the commit matching.
- **Everything else repowise returns is a locator.** `get_context`,
  `get_answer`, `get_risk`, `get_change_risk`, `get_health`, `get_dead_code`,
  `search_codebase`, `get_why`, and the wiki are summaries or scores. No
  finding, no acceptance criterion, no citation and no approval rests on one.
  The file it points at gets opened — by `Read`, or by `get_symbol` at HEAD —
  or the claim does not get made.
- **Index behind HEAD → the exception lapses.** Every result is approximate
  and `get_symbol` loses verified-read status until the index is resynced
  (`repowise update`). `indexed_commit` is what makes staleness observable
  instead of silent, which makes checking it load-bearing rather than hygiene.

This is `qa.md`'s existing rule — "a `Grep` hit is a candidate, not a verified
read" — restated for a tool that returns prose instead of line numbers, and it
is the same rule for the same reason: a summary is an assertion about the tree
by something that is not the tree.

**A savings mechanism may compress a locator; it may not compress evidence.**
repowise ships hooks that rewrite tool results in flight. `search_digest`
compacts search output, and search results are already locators here, so it
costs nothing this rule depends on. `read_skeleton` makes a `Read` return a
skeleton instead of file bytes — summarized content arriving through the exact
channel this section designates as verified, and arriving invisibly, since
nothing in the result says it is a summary. It is therefore off, and any
future hook is judged on the same line. `repowise distill`, which compacts the
output of `cargo test` and `clippy`, is unaffected: a command's stdout is not
a file read.

**Common mode.** Claude QA and Codex QA query the same index. A wrong entry in
it is wrong for both, which is exactly the correlation the second review
exists to break. Ground findings in the tree and the diff, never in the shared
index — this is why the exception above is narrow and conditional rather than
a general "trust the index".

## updating specs
Agent specs are code. Change via PR like anything else. Spec make bad output → fix live in spec: tighten constraints, or add concrete example of bad behavior to relevant section.
