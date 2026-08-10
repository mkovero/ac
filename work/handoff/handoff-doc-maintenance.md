# handoff-doc-maintenance — the docs that assert something false, and the sweep that should have caught them

> **DELETE THIS FILE when item 5 of "Suggested order" has landed** — the `gh`
> triage of the older open-issue population. That is the only thing left in
> here. Handoffs in this repo do not expire on their own — that is how
> `work/rig/rig-verify-queue.md` came to open with "nothing here has been run" on the
> day after all of it was run — so the condition is written down rather than
> left to judgement.
>
> **One thing must outlive the deletion:** check 4 of section 7 (a verification
> must be able to fail). A process rule inside a handoff dies with the handoff.
> Its permanent home is `.agents/qa.md` or `.agents/audit.md`, under Markus's
> ratification; this file is its temporary lodging until that happens. If you
> are about to delete this file and check 4 is still only here, move it first.
>
> **Status, 2026-08-06 (`4b2e603`).** Items 1, 2, 3, 4 and 6 are done — struck
> through in place below, with what each turned into. Item 5 is half done: the
> issue population *was* enumerated with `gh` and mechanically checked (every
> file path in every open issue, results in section 5), and the missing
> arrival-disclosure issue was filed as #255. What remains is the per-issue
> triage decision on the older population — nothing below #193 has been ruled
> on.
>
> Two errors in this file's own analysis were found by checking it against the
> tree; both are corrected in place, in sections 3 and 6. They are left visible
> rather than silently fixed, because a sweep document that cannot be wrong is
> the thing check 4 is about.

Written 2026-08-06 against the `ac-main` snapshot taken 2026-08-05, after rig
session 3 and after #246 / #247 landed. Everything below was checked against
the tree, not recalled. Two caveats on provenance:

- The tree is a snapshot, not `origin/main`. Where this file says "landed",
  it means the code is present in the snapshot — confirm against `main` before
  closing anything on that basis.
- GitHub's issue list gives only the newest 12 of 40 open issues without
  authentication, and blocks the paginated query. Section 5 is therefore
  incomplete by construction and ends in a command rather than a list.

Each item states what a pass looks like, so a fix can be told apart from a
rewrite.

---

## ~~1. The files every agent session reads first, and all three are wrong~~ — DONE (`9fd8436`)

> All three fixed, plus two the section did not name: `ARCHITECTURE.md` (which
> also omitted `ac-scene` / `ac-view`, and now states the display-truth rule)
> and `TESTING.md` (per-module count tables removed entirely — they were wrong
> within weeks last time and named modules that no longer exist). Real figure
> was **899 passed / 14 ignored**, against ~485 claimed here and ~795 in the
> README. This also discharges most of **#107**, which had been open since
> 2026-07-24 asking for exactly this and is not mentioned anywhere in this
> file.
>
> Found while doing it: `cargo test --workspace` **did not compile on `main`**.
> #252 was branched before #248 added two fields to `TransferInput`; no textual
> conflict, both merged clean, break existed only in the combination. Fixed in
> `6d2557c`. A per-crate run passes on either side of it — this is the
> argument for `--workspace`, and for #201.

This is the priority. `CLAUDE.md` is what a Claude Code session loads before
it does anything, so an error here is inherited by every role in the workflow.

**Root `CLAUDE.md`** names the workspace as `ac-core, ac-daemon, ac-cli`.
Five crates exist. `ac-scene` and `ac-view` — the display-truth boundary and
the entire GUI — are absent from the map an agent starts from.

> Pass: the crate list matches `ls ac-rs/crates` exactly, with one line each
> and a pointer to `ARCHITECTURE.md`. Nothing else in the file needs to change.

**`ac-rs/CLAUDE.md`** is worse, because it is specific:

| claim in the file | in the tree |
|---|---|
| "~485 tests + 1 `#[ignore]`'d" | README says ~795, 14 ignored |
| `ac-core` 243 tests | README says 366 |
| crate table: 3 rows | 5 crates |
| "CLI client and ZMQ daemon" | plus a native egui/wgpu view and a scene crate |

> Pass: no number in the file that a `cargo test --workspace` run contradicts.
>
> The standing rule against citing test counts from memory applies to prose as
> well as to conversation — a hardcoded count *is* a memory, and it rots
> silently. Either date the figure and name the commit it was taken at, or
> give the order of magnitude and tell the reader to run the command.

**`ac-rs/PLAN.md`** states "`ac-cli` + `ac-daemon` are the product now", which
stopped being true when `ac-view` landed, and frames the work as a migration
from a Python server that finished months ago.

> Leaning: archive it under a dated "superseded" header rather than maintain
> it. It is a migration plan whose migration is complete; keeping it current
> costs more than it returns. If it is kept, the non-goals list is the part
> that must change.

---

## ~~2. Docs that would send someone to the rig for data that already exists~~ — DONE (`9fd8436`)

> All three edited as specified. `work/planning/state-live-spectrum.md`'s "Corrected, and
> worth not re-deriving" is untouched; `audit/rig-verify-125/gate-rules-offline.md` keeps its rule
> ranking and gained a superseded header only; `work/rig/rig-verify-queue.md` marks each
> block executed or not with Run D promoted to the top.

**`work/planning/state-live-spectrum.md`** — the file whose first line is "Read this first
after a gap". It says #234 is open and gated on #232, and that "#226 and #227
remain". #227, #232 and #234 are all in the tree. Of its five-item `## Next`
list, items 1, 2 and 4 are done and item 5 (#221) has changed status from
latent to real.

> Pass: "Where it stands" and "Next" describe `main`. Keep **"Corrected, and
> worth not re-deriving"** untouched — the ρ=1/6 category error and the
> ship-the-inputs-not-a-derived-depth rule are the most valuable paragraphs in
> the repo, and they are still true.

**`audit/rig-verify-125/gate-rules-offline.md`** — ends with "What would
settle it: a capture of the ambiguous case ... **This is now constructible on
demand**." Session 3 constructed it. Run 2, third position, A at 1.8 m and B
at ~2.5 m, measured 1.1 dB apart at the capsule with 134 samples of
separation:

| condition | locked | lag | median prominence |
|---|---|---|---|
| A alone | 6/6 | 628 | 26.75 |
| B alone | 6/6 | 762 | 25.77 |
| A + B | 8/8 | 628 | **28.07** |

Prominence *rises* when the case gets ambiguous, because a second correlated
source moves the median slower than the peak. Neither candidate rule refuses
it, and no threshold on that statistic can. The candidate-count alternative is
separately dead: 23 → 32, censored at `MAX_CANDIDATES`.

> Pass: a "superseded" header pointing at `work/rig/rig-session-3-results.md` Run 2.
> **Do not delete the scoring.** The two rules still differ on the
> single-source data, which is where the choice between them has to be made,
> and that ranking is the only record of it.

**`work/rig/rig-verify-queue.md`** — opens "Nothing here has been run: the operator is
off site", dated 2026-08-03. Session 3 ran the next day and covered blocks 1
through 3. Run D (#208's positive control) is the one block still unrun, and
was already flagged as the first thing to cut.

> Pass: each block marked executed or not, with the surviving work promoted to
> the top. A queue that lists completed work trains its reader to skim it.

---

## ~~3. Issues and PRs pointing at things that no longer exist~~ — DONE, and this section was wrong about #230

> **#230 — closed** as done in place. ~~**#243 — closed** as
> documented-not-fixed by owner decision, with #248's own "does not close it"
> recorded in the closing comment so the residual is not lost.~~ **PR #236 —
> merged.**
> **PR #214 — closed**, #205 carries it; `conn_tags` survives only on
> `feat-205-drive-path-health`, verified still on origin at `4bf6336`.
> `work/handoff/handoff-issue-strategy.md:137`, which repeated #230's two dead references,
> is fixed in `4b2e603`.
>
> **Correction to this section.** It states that #230's two cited files are not
> in the tree. Both exist in the working checkout as untracked copies, and
> their line references resolve (`work/qa/qa-brief-218-222.md` at `:52`, not `:51`).
> They are absent from a *clone*, which is the reading that makes both
> statements true — and the distinction matters, because it is the difference
> between "the issue is stale" and "the issue is unfixable from a clone".
>
> **Second correction, 2026-08-10: #243 is open.** It was closed 2026-08-06
> 01:44 UTC and **reopened 35 minutes later** by the owner, who recorded the
> close as wrong: the README wiring section is item 1 of
> `work/handoff/handoff-243-redirect.md`, not the resolution, and PR #248 said
> "Addresses … **Does not close it**" in its own body. What is left is item 3
> — a plausibility check, unlanded — and the rig's 1.1931 ms residual, which
> reads 1.40 m at a taped 1.000 m: positive and locked, so
> `format_delay_readout`'s existing gates pass it silently. The residual
> reasoning below stands; only the closure claim was false.
>
> **The rule this argues for is not "check the tree against GitHub".** Thirty-
> five minutes separates the two states, so this document was *true when
> written* and false before anyone could have reviewed it — a staleness check
> at any plausible moment would have passed. Prose should therefore not
> restate an issue's open/closed state at all: name the issue and let the
> tracker hold its state. What belongs in a document is the reasoning, which
> does not expire.

**#230** names `work/handoff/handoff-mtw-live-spectrum.md:239` and `work/qa/qa-brief-218-222.md:51`.
Neither file is in the tree. The surviving occurrence of the `((W−D)/W)²`
model is `work/handoff/handoff-rig-findings.md:71`, and it already carries a `[revised]`
correction immediately below it.

> Action: re-point at the surviving occurrence, or close as done-in-place
> after confirming no QA criterion still checks against the wrong ceiling. It
> was filed as ten minutes of work; it is now five, or zero.

**#243** is open, but the README already carries the full wiring doctrine —
reference out the same converter as the stimulus — and explicitly declines to
subtract a stored instrument constant, on the grounds that speaker DSP latency
belongs to the DUT. ~~There is no visible remaining deliverable.~~

> ~~Action: close as documented-not-fixed, or state the remaining deliverable
> in the issue. An open bug with its resolution already shipped as
> documentation is a trap for whoever picks it up next.~~
>
> **This recommendation was acted on and was wrong** — see the second
> correction above. The deliverable was visible in PR #248's own body. The
> second half of the action ("state the remaining deliverable") is what the
> reopen comment did.

**PR #236** — a docs PR, open since Aug 3, closing the rig-session merge gate
and dropping the `conn_tags` check from Run B. Session 3 ran on Aug 4 and did
not appear to be blocked by it.

> Action: merge or close. Either the gate it closes was overtaken by the
> session, or the session ran against an ungated tree.

**PR #214** — `needs-work` since Jul 26, five comments, one linked issue.
Overlaps #205 (the `ac-view` field face of the same problem) and the drive
path that landed around #231.

> Action: rebase against `main` or close and let #205 carry it. A `needs-work`
> PR older than most of the crate it touches is a merge hazard, not a
> work-in-progress — and under qa.md #200 it cannot shortcut back to approved
> anyway.

---

## ~~4. install.sh — handled, and it leaves doc debt behind~~ — DONE (`fa6ee27`, `9fd8436`)

> The script now installs `ac-view` and **prints the sha256 of all three
> binaries**, which answers this section's open question: the queue's
> verify-by-hash instruction collapses into reading the install output, and
> `work/rig/rig-verify-queue.md` item 2 is struck accordingly. The session record's
> wording is unchanged, as this section asked.

`ac-view` now ships. Not visible in the 2026-08-05 snapshot, so the fix
postdates it; nothing below assumes otherwise.

What remains is that three documents still tell the operator it does not:

- `work/rig/rig-verify-queue.md`, "Before anything: the rig's own defects", item 2 —
  "**`install.sh` does not ship `ac-view`.** Copied by hand twice now."
- `work/rig/rig-session-3-results.md`, Pre-flight — "All three binaries copied by hand
  — `install.sh` does not ship `ac-view`."
- Section 1 of this file's own priority list, before this edit.

The session record is history and should keep its wording; the queue is an
instruction and should not.

> Pass: `work/rig/rig-verify-queue.md` no longer lists this as a defect to work around.
>
> One thing to confirm while editing it: does the script print the sha256 of
> each installed file? The queue's verify-by-hash instruction exists because
> size and mtime both matched on a stale binary. If the script prints the
> hashes, that instruction collapses into reading the install output. If it
> does not, the instruction stays and is now the only thing item 2 is for.

---

## 5. The 28 issues I could not see — **THE ONLY ITEM STILL OPEN**

> **Half done.** `gh` authenticates fine from the working checkout, so the
> population is visible after all: **40 open issues** at the time of the sweep,
> now 38 plus #254 and #255. The mechanical half of the triage rule was run —
> every file path and line reference in every open issue, checked for
> resolution — and the results are below. The missing arrival-disclosure issue
> named at the end of this section **was filed as #255**.
>
> **What is left: the per-issue decision on the older population.** Nothing
> below #193 has been ruled on. The shape of it, from the enumeration:
>
> - **#1–#13** are the hardware backlog (KiCad, PSU, transformer specs), last
>   touched 2026-04-13. Almost certainly fine as-is; they are not stale, they
>   are unstarted.
> - **#107** asked for exactly what section 1 of this file did, and is now
>   mostly discharged. Close it or restate the remainder.
> - **#112, #114, #116, #117, #121, #122, #127, #129, #130, #132** are the
>   May/June CLI and measurement-accuracy population, several marked
>   `ready-to-implement` and `ux-approved`. Untouched for two months.
> - **Dead references found mechanically** (a defect in the issue, not the
>   tree): **#107** cites `analysis.rs`, `parse.rs` and `TESTING.md:266`
>   (past EOF, the file is 241 lines). **#167** cites `receiver.rs` and
>   `ac-ui/src/data/receiver.rs` — `ac-ui` was detached. Everything else
>   resolved, though several open issues cite untracked working-copy files
>   (#219, #221, #249) and would be unfixable from a clone, which is the same
>   trap #230 fell into.

The list page returns the newest 12 open issues to an unauthenticated fetch
and refuses the paginated query. The older population — below #193 — is
exactly where stale issues live, and it is unexamined.

```bash
gh issue list --state open --limit 100 \
  --json number,title,updatedAt,labels --jq \
  '.[] | [.number, .updatedAt, .title] | @tsv' | sort -k2

gh pr list --state open --json number,title,updatedAt,labels
```

Triage rule to apply to each: **an issue whose evidence predates rig session 3,
or whose named files and line numbers no longer resolve, gets re-pointed or
closed.** Left as-is is the one option that is always wrong, because the next
reader cannot tell a stale issue from a live one without redoing this check.

~~One issue that should exist and does not:~~ **Filed as #255** (`needs-design`),
with the prominence-rises table leading so it is not misread as a refusal
request, and the `MAX_CANDIDATES` dead end recorded in the body so nobody
re-derives it. **The discarded second arrival.**
Run 2 established that on a two-source measurement the estimator locks on the
nearest arrival, correctly and confidently, and never tells the operator that
a comparable second arrival 1.4 ms later was passed over. It is a disclosure
gap, not a correctness bug, and `work/rig/rig-session-3-results.md` names the shape a
fix would need (arrival clusters, not peak counts — the count version is
recorded there as a dead end). It currently lives in one results file.

---

## ~~6. One desk item, because three numbers disagree~~ — DONE (`d70be29`), and this section was wrong too

> **Measured: 16.56 frames/s per pair**, `--fake-audio`, 48 kHz, 30 s, two
> pairs, median inter-frame gap 60.3 ms. Falsified against 0.4 and 10;
> consistent with the rig's 17.5–18/s at 96 kHz. The code was right and both
> docs were wrong: `ac-rs/ZMQ.md` gave the H1 sliding window
> (`capture_duration(4, sr)` = 2.5 s) as if it were the publish interval, and
> `transfer.rs`'s "~10 Hz" was correct when `chunk_secs` was 0.2 and was not
> revisited when it became 0.05. Both corrected.
>
> **Correction to this section.** It states that the "≈2.5 s at 48 kHz"
> sentence is not in `ac-rs/ZMQ.md` and calls `work/planning/state-live-spectrum.md`'s attribution
> a doc error carried through two sessions. The sentence is at `ac-rs/ZMQ.md:1539`.
> The attribution was correct; the source was the thing at fault.
>
> **Found while measuring, and it is the more important result:** the desk
> check specified here — "run `--fake-audio`, count frames, divide by pairs" —
> **cannot be run as written.** A `transfer_stream` over three or more distinct
> channels replies `ok: true` and then publishes nothing, forever, with no
> error frame. The measurement above only happened by substituting
> `[[0,1],[1,0]]` for a genuinely multi-channel session. Filed as **#254**
> (`blocker`), and #204 was rescoped around it as the shared root cause.

Frame cadence, unexplained across two rig sessions:

| source | figure |
|---|---|
| `ac-rs/ZMQ.md:1611` | one frame per pair per iteration — **no rate stated** |
| `handlers/transfer.rs:585–594` | capture interval 0.2 s; comment claims "capture-interval-limited ~10 Hz" |
| measured, sessions 2 and 3 | ~17.5–18 frames/s per pair |

Note that `work/planning/state-live-spectrum.md` attributes "≈2.5 s at 48 kHz" to `ac-rs/ZMQ.md`.
That sentence is not in `ac-rs/ZMQ.md`. The attribution is itself a doc error, and
it has been carried forward through two sessions as though it were the
documented contract.

> No rig needed. Run `--fake-audio`, count `transfer_stream` frames on DATA
> over 30 s, divide by pairs. Falsifiable against 5, 10 and 18. Then correct
> whichever of the three is wrong — and if the answer is ~18, the comment in
> `transfer.rs` is describing a loop that no longer behaves that way, which is
> worth understanding before it is simply edited.

---

## 7. Changes to the handover process — the maintenance sweep

Proposed. **Touches `.agents/` if adopted, so it needs Markus's ratification
in review; nothing here is applied unilaterally.**

**When.** At every rig-session boundary, not on a calendar. That is where
context resets anyway, and it is where the measured world most recently
contradicted the written one.

**Four mechanical checks, each falsifiable, each a command rather than a
judgement:**

1. **Every crate named in a doc exists, and every crate that exists is named.**
   `ls ac-rs/crates` against a grep of the doc set. This sweep found *five*
   files failing it — the three named in section 1, plus `ARCHITECTURE.md` and
   `TESTING.md`.
2. **Every file path and line reference in an open issue or handoff resolves.**
   A dead path is a defect in the issue, not in the tree. Run against all 40
   open issues on 2026-08-06; #107 and #167 fail it. Note the sharper version
   the #230 correction produced: **resolve against a clone, not the working
   checkout.** An untracked file resolves locally and does not exist for
   anyone else, which is indistinguishable from a dead path at the point it
   matters.
3. **No hardcoded test count, LOC figure, or constant value in prose that a
   command contradicts.** Where a number must appear, date it and name the
   commit.
4. **Every verification the sweep relies on must be able to fail — ask what
   would make it go red.** If nobody can answer, the check is decoration and
   the sweep should record it as unverified rather than as passing.

   **This is a running tally, not a fresh count.** The class was already
   established with at least five named instances — among them
   `FakeEngine::last_drain_occupancy` passing against an unfixed daemon, and a
   `cycling_derot_*` sibling passing on two empty panes. It is also *not* the
   coupled-constants class, which separately has exactly three instances
   (`settled` vs the three stages, `MIN_PROMINENCE` vs `DIRECT_PEAK_FRACTION`,
   the admission/refusal timer). Two classes, two counts; conflating them
   makes both look like they contradict a prior tally. Added 2026-08-06, none
   of them careless, all structurally incapable of failing:

   | green signal | what it actually touched | why it could not go red |
   |---|---|---|
   | `build_ok=$?` after `cargo test \| grep \| head` | `head`'s exit status | `head` returns 0 whether or not cargo failed |
   | `capture_multi_matches_stereo_default` | `assert_eq!(bufs.len(), 2)` | asserts the invariant that *is* #254's defect; passes because the bug is present |
   | installed-binary check by size + mtime | two attributes that collided | same size, same mtime, different sha256 — a session lost to a stale binary |

   This is #201's argument arriving from the documentation side. The two are
   the same problem: a check nobody can make fail, and a check nobody
   remembered to run, are indistinguishable from the outside.

   **This check must move to `.agents/` before this file is deleted** — see the
   header. It is the one rule here with a life longer than the sweep that
   produced it, and a process rule inside a handoff dies with the handoff.

**The sweep's output is a diff, not a report.** If a sweep produces a document
instead of commits, it has failed — this file included.

**One rule this project keeps rediscovering, worth writing down: when a
measurement supersedes a written expectation, the superseded doc gets a
header, not a delete.** `audit/rig-verify-125/gate-rules-offline.md` is the case in point — its
ranking of the two candidate rules is still the only such record, while its
closing section now sends the reader on a trip that has already happened. Both
of those facts have to survive the edit. The same applies to the #208 positive
control, to the ρ=1/6 correction, and to the agreement guard: this repo's
durable value is disproportionately in its records of what turned out to be
wrong, and a sweep that tidies those away is worse than no sweep.

---

## Suggested order

1. ~~The three `CLAUDE.md` / `ac-rs/PLAN.md` files.~~ **Done**, `9fd8436`.
2. ~~`work/rig/rig-verify-queue.md`.~~ **Done**, `9fd8436`.
3. ~~#230, #243, PR #236, PR #214 — decisions, not work.~~ **Done.**
4. ~~`work/planning/state-live-spectrum.md`, `work/rig/rig-verify-queue.md`, `audit/rig-verify-125/gate-rules-offline.md`
   headers.~~ **Done**, `9fd8436`.
5. The `gh` sweep of the older open issues — **the only item still open.** The
   arrival-disclosure filing half is done (#255); the per-issue triage of
   everything below #193 is not. See section 5 for the enumeration.
6. ~~The cadence measurement.~~ **Done**, `d70be29` — and it produced #254.

**When item 5 lands, delete this file** — after moving check 4 of section 7 to
`.agents/`, per the header.
