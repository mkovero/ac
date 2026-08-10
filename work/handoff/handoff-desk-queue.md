# handoff-desk-queue — what to pick up after the 2026-08-10 reconciliation

> **DELETE THIS FILE when section 3 is empty** — it is a resumption note, not a
> record. Its contents belong in `work/rig/rig-verify-queue.md` (rig work) and
> in issues (desk work); it exists only because a context ended mid-flight.
> Anything still here that has no other home when the last item clears must be
> moved before deletion, not lost with it.

Written 2026-08-10 at the close of the session that emptied
`flush-2026-08-05.md` into the tree. Read this first if you are picking the
project up cold; then `work/rig/rig-verify-queue.md` for anything needing the
rig, and `work/planning/state-live-spectrum.md` for where the measurement work
itself stands.

**Provenance, and it matters here.** The session that produced this file ran
against an `ac-main` snapshot with no `.git` and no network. Every claim about
what merged, and every issue state below, was reported by the worker with
repository access and is **second-hand in this document**. The claims about
file contents and line numbers were checked against the snapshot directly.
Before acting on any issue state below, check it with `gh` — this session
produced two separate instances of a document asserting an issue state that
was wrong or had changed underneath it.

---

## 1. What landed

Four PRs, merged in dependency order, all documentation and agent specs. No
source code changed.

- **#257** — the first four reconciliation items: the four surviving "set the
  clock to Internal" instructions corrected, the `ac-view` freq-axis comment
  amended, item 2 vacated in place, and the 3a/3c blocks written into
  `rig-verify-queue.md`.
- **#259** — the untracked sort. 55 files resolved into commit / keep /
  delete, `audit-2026-05-27.md` filed under `docs/superseded/`, a `.gitignore`
  comment recording that evidence under `audit/<session>/` is committed
  because it is equipment rather than output. (This carries the seven commits
  that #258 put on the wrong base; #258 itself is not the record.)
- **#260** — the `.agents/` repair, split mechanical / judgment. All five
  specs regenerated against the real five-crate workspace; three review checks
  rewritten so a wrong answer is possible; `architect.md:37`'s dead invariant
  replaced with the calibration layer topology; `AGENTS.md` gained an
  evidence-discipline section.
- **#262** — item 3b resolved and struck, with the post-lock `CHECK ROUTING`
  block queued in its place.

**Issues touched:** #261 filed (`ready-to-implement`); #184 commented and
moved `needs-design` → `ready-to-implement`; #205 commented.

---

## 2. Open by design

None of these is an oversight. They are named here so a clean tracker does not
read as a finished project.

- **#184** — three criteria remain after #260: the `blocker`/`blocked` label
  collision, `agent:dev` creation plus schema-label existence, and the
  `AGENTS.md` table entry for whichever label survives. Named in #260's body
  so a merged PR does not read as completion. Label state plus one table —
  small, and deliberately not bundled with the spec sweep.
- **#261** — the `monitor.rs` parity case. Closes the last gap between the
  calibration invariant and being fully machine-backed. See section 3.
- **#205** — the two-vocabulary conflict recorded for when the drive-path
  health check lands. Not actionable until it does.

---

## 3. Desk queue, in order

### ~~3.1 Block 2's offline question~~ — **done 2026-08-10, answered "no"**

`audit/rig-session-3/negative-lag-rule.md` + `negative_lag_rule.py`, 843
attempts, no rig time. **The premise is false, which closes the family rather
than this one variant**: the all-lag floor carries 3.5% contamination against
±17.5% per-attempt noise, so there is nothing for an uncontaminated floor to
remove. Second measured refutation, after
`audit/rig-verify-125/gate-rules-offline.md` §2 (≤8% on 12 captures). The
variant also underperforms — margin against the pooled silence ceiling narrows
1.37× → 1.04×, the near-wall 52 cm-wrong lock is promoted, 1/8 wall sessions at
admission 24 becoming 3/8 — but that is downstream of the premise, and reporting
it first would invite a better variant of a dead family.

**It did not change what the next rig visit is for** — which was the reason it
went first, and is a legitimate outcome of asking. The gate remains a
wrong-peak problem that no floor addresses.

Two things worth carrying out of it, both in section 5's failure class:

- **It was settleable by reading.** `visualize/transfer.rs` held the premise
  (`:319`) and its refutation (`:491`) about 170 lines apart, for as long as the
  proposal was open. Nobody put them side by side, so a claim the tree already
  contradicted shaped a capture plan across two sessions.
- **The one surviving property has a home rather than being a remainder.**
  `R = median_value / negative_lag_median` < 0.5 is a measured onset signature
  and the discriminator the dropped onset guard lacked. Written into
  `rig-verify-queue.md` block 1, with Run D as its control, as a capture
  requirement: **per-frame floors, not counters** — which is what `run4` got
  wrong, and why it could not be re-scored.

### 3.2 #254 — the three-channel stall

`blocker`. A `transfer_stream` over three or more distinct channels reports
`ok: true` and then publishes no frames, indefinitely, under `--fake-audio`.

It compounds: while it stands, nothing three-channel can be rehearsed off the
rig. `[[3,3],[0,3]]` (the converter-constant measurement) is two channels and
unaffected, but adding a second measurement position — `[[0,3],[1,3]]` — is
three. Landing it converts a class of rig work into desk work.

### 3.3 #261 — the `monitor.rs` parity case

Bounded, `ready-to-implement`, with a model test to copy at
`it_cross_tier_parity.rs:827`. Five acceptance criteria in the issue body.

Two of them matter more than the rest:

- **Criterion 3 — falsify before landing.** Compose the layers deliberately,
  confirm the test goes red, revert. A parity test never seen to fail is the
  thing the issue exists to stop accepting.
- **Criterion 2 — justify the tolerance in a comment.** The margin must be
  large enough that composition lands far outside it, and the comment must say
  the tolerance absorbs capture jitter only. A number that looks like a
  tolerance to tighten is exactly what gets tidied up two years later by
  someone who does not know what it was sized against.

When it lands, `architect.md`'s read-only caveat about `monitor.rs` goes with
it — the caveat names #261 and was written to be deleted by it.

### 3.4 Items 7 and 8 — the two documents

These are the only reason `work/handoff/handoff-flush-reconciliation.md` still
exists, and the only reason `flush-2026-08-05.md` still has to be protected.
Both are writing, not code. Full substance for both is in that handoff — the
worker does not need the flush file.

- **Item 7** → `docs/design/design-parametric-reflection-removal.md`. The
  floor-bounce / MEDLL proposal. Architect output. Write it as a proposal, not
  a plan; nothing in it is ratified. The load-bearing part is the
  architectural requirement: **the fit residual must be an output, not an
  internal.**
- **Item 8** → `docs/coherence-diagnostics.md`. Operator-facing reference.
  Half the material is already tracked in `handoff-lock-and-smoothing.md`
  decision 5 — cross-reference, do not restate. What is untracked: the
  algebraic gain-invariance argument, the `γ² = SNR/(1+SNR)` table, the two
  diagnostic rules, and the Open Sound Meter conjecture, which must go in
  **labelled as unverified**.

### 3.5 Item 4 — the `#[ignore]` audit

No deadline. `qa` in audit mode — read-only, and it fits `qa.md`'s audit
section exactly. Roughly 11 real attributes across 8 files, all carrying
stated reasons, so the audit is cheap.

The question per test is **what would notice if the thing it covers broke**. A
row reading "nothing does" is the useful output, not a failure of the audit.
The fixture-regeneration entries are a different category from genuinely unrun
coverage and should be named as such rather than sharing an attribute.

Full table of the population is in `handoff-flush-reconciliation.md` item 4.

### 3.6 Smaller

- **#184's three remaining criteria** — label state plus a table entry.
- **#255** — `needs-design`, and it needs no rig time. Session 3's captures
  are committed and the ambiguous case is reproducible on demand (two of the
  room's three speakers energised). An architect pass, not a trip.

---

## 4. Rig queue — for when a visit happens

Detail is in `work/rig/rig-verify-queue.md`; this is the scheduling shape only.

| block | needs the mic? | note |
|---|---|---|
| 3a — snapshot PNG regen | **no** | wgpu render check on the box |
| 3c — `install.sh` under a running daemon | **no** | file copy question |
| post-lock `CHECK ROUTING` | yes, **where it is** | no position change |
| cable change (#243) + #251 | yes — **move to 1 m on axis** | |
| Run D — #208's positive control | yes | competes for time, legitimately |

**3a and 3c compete with nothing.** They need no microphone and no position
change, which is why cutting them for time is a category error — that
reasoning is now in the queue itself. Run D is the opposite: it needs the
emission path, per-run consent and the `cda40ef` A/B, so dropping it is a real
trade. It has now survived two sessions unrun; a third cut should be recorded
as a decision to close #208's verification unproven rather than deferred
again.

**The mic was left at the near-wall position** after session 3 — 2.4 m from A,
28 cm off the wall, off axis. Anything assuming 1 m on axis moves it first.
This has cost a session before.

---

## 5. One observation with no home yet

Not filed, and a candidate for `.agents/` rather than for tonight.

The pattern that produced most of this session's work was not wrong facts. It
was **true statements that decayed**. `handoff-doc-maintenance.md` was correct
for thirty-five minutes. The `#[ignore]`d snapshots were correct until #252
moved the layout. #184's scope line was correct when the repo had three
crates. None was wrong when written, so no review at the time could have
caught any of them.

That is a different failure class from the one the evidence-discipline section
covers, which is about asserting a mechanism without the measurement that
would distinguish it. The operational form is probably the same rule already
applied to issue state — do not restate in prose what another system holds
authoritatively, and where you must, date it — but generalised beyond issues.

The de-enumeration of `computes_nothing.rs` in #260 is the first instance of
the fix being applied deliberately: name the file as authoritative, give the
current contents as *what it enforces today*, dated. Adding a fourth test then
makes the spec under-describe rather than mis-describe.

---

## 6. If you only do one thing

~~**Block 2.**~~ Done 2026-08-10 — see 3.1. It was the only item where waiting
cost a session, and it now costs none.

**Next: #254**, the three-channel stall. It is the item that converts rig work
into desk work, and everything below it in section 3 is bounded and can wait.
