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
| `.agents/rig.md` | hardware-in-the-loop verification — measurement record, interlocks | `bin/rig.sh <pr>` when QA's tree pass is rig-pending; or manual invocation |

## routing logic

```
new issue
  └─ triage
       ├─ needs-design → architect → ready-to-implement
       ├─ needs-ux     → ux        → ready-to-implement
       └─ ready-to-implement → developer → PR → qa → codex-qa → human merge
                                                    │
                                   requires-rig on PR or issue:
                                   qa (tree, rig-pending) → rig → qa (same commit, with the record)

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
- Removing `requires-rig` without a rig record that settles the named check. QA removes it on a record whose verdict is `pass` and that closes the named falsification test (`qa.md`). A record that fails or declines to conclude leaves the label for a human.

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
| `requires-rig` | triage or architect (on the issue, when a criterion is physical), qa (at review), rig (on an issue it files) | correctness rests on a measurement only the rig can make — blocks both approval labels. Whoever sets it names the measurement: quantity, configuration, falsifying value. **Removed only** by qa (on a passing rig record, or as moot per a design's `rig check: none` or a later push) or by a human; triage, architect, developer and rig never remove it |
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

Neither approval label may be applied while `requires-rig` is present, on the
PR or on the issue it closes. Tree QA names the measurement and stops at the
rig gate. The runner then runs the rig role against that commit
(`bin/rig.sh`), and QA runs again at the same commit with the record: a
passing record lets QA clear the label and approve; anything else stops for a
human.

### rig sessions: standing consent and the lock

**Standing emission consent** (operator, 2026-09-16): an agent rig session —
pipeline or manual — may emit without asking per run, within these limits:
- every emitting request carries a typed level **≤ −40 dBFS**, and the rig
  profile's own lower ceilings still apply (pupu: −50 dBFS on the speaker);
- bounded commands only (`plot ir`, `calibrate`, the `scripts/rig/` wrappers),
  never a stimulus that runs until stopped.

Still asked for every time: host reboots, driver reloads, cable or mic moves,
clock changes, raising a ceiling, anything that needs someone in the room.

**One session at a time.** Every rig session holds the rig lock for its
duration: `bin/rig.sh --lock "<who, what>"` before the first command that
touches the rig, `bin/rig.sh --unlock <token>` after. The pipeline takes it
itself. A held lock shows in the rig's login banner. A lock past its lease is
broken by the next taker, with a note.

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

- **Never background a command whose result you need.** The workspace gate
  (`$AC_GATE`, see below) and any targeted test run in the foreground, with the
  tool timeout raised to its maximum.
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

## workspace gate — every role that builds

The workspace gate is `cargo fmt --check`, `cargo clippy --workspace
--all-targets -- -D warnings` and `cargo test --workspace`. Run it only as
`$AC_GATE` (`bin/gate.sh` in the main checkout), from inside your worktree:

- It runs all three once per commit tree and records the result under
  `$AC_GATE_DIR`. Any later call for the same tree — yours, the runner's,
  another role's — prints the record without running cargo. Calling it again
  is free; running the three commands by hand is not.
- Output: PASS/FAIL per step, the failing lines of a red step, and the path of
  each full log. Exit 0 all pass, 1 any failed, 2 refused.
- It refuses an uncommitted tree (exit 2): the record names a commit. Commit
  locally first; push only after a pass.
- Need more than it printed? Read the log at the printed path. Need one test?
  `cargo test -p <crate> <test name>`. **Never re-run the workspace gate to
  see output** — and never pipe a gate command through `tail`, which replaces
  its exit status with tail's. Truncate-then-rerun was the largest single cost
  in the 2026-09 transcripts: qa averaged two full `cargo test --workspace`
  runs per session, the second identical to the first.
- The record is execution evidence, not a review. A reviewer still reads every
  new or changed test.

**Target dirs.** The runner pins `CARGO_TARGET_DIR` to a per-worktree
directory (`$AC_HOME/target/wt/<worktree>`), seeded warm. Never set
`CARGO_TARGET_DIR` or `--target-dir` yourself, and never create a
`target-<something>` directory. A target dir shared across worktrees does go
false-fresh (cargo trusts mtimes and runs another worktree's code); the
per-worktree dir is the fix, and a private cold one costs minutes per pass.

## updating specs
Agent specs are code. Change via PR like anything else. Spec make bad output → fix live in spec: tighten constraints, or add concrete example of bad behavior to relevant section.
