# handoff-flush-reconciliation — moving `flush-2026-08-05` into the tree

> **DELETE THIS FILE when items 1–8 below have landed.** Handoffs in this repo
> do not expire on their own. Items 9 **and 10** are what must outlive the
> deletion: both are `.agents/` changes under Markus's ratification, and until
> that happens this file is their temporary lodging. If you are about to
> delete this file and either is still only here, move it first. They go
> together — one adds a rule to a spec, the other repairs the specs that rule
> would be read alongside.

Written 2026-08-10 against the `ac-main` snapshot taken 2026-08-06. Its input
is `flush-2026-08-05.md`, a context dump of material that existed only in
conversation. Every claim below was checked against the tree rather than
recalled, and the flush is **one day older than the snapshot**, so several of
its entries were already overtaken. Those are marked rather than silently
dropped.

Provenance caveats, the same two that apply to `handoff-doc-maintenance.md`:

- The tree is a snapshot, not `origin/main`. Where this file says "present",
  it means present in the snapshot — confirm against `main` before closing
  anything on that basis.
- **No issue state below was read from GitHub.** Every "#N is open/closed"
  here is quoted from another document in the tree, which is secondary. Check
  with `gh` before acting on any of them.

Each item states what a pass looks like, so a fix can be told apart from a
change.

---

## Already overtaken by the tree — do not act on these

Recorded so the flush is not re-read as a to-do list.

- **`install.sh` ships `ac-view`.** Fixed 2026-08-06 (`fa6ee27`). The script
  installs all three binaries and prints their sha256, under `set -e`, so a
  failed install aborts before the hashes print rather than half-succeeding
  quietly. The flush's "copied by hand at least twice" is stale.

  > One residual, and it is a rig check not a code change: `install -m 755`
  > may still hit `Text file busy` on `ac-daemon` under a running daemon. Not
  > established either way. Stop the daemon first, and if it installs cleanly
  > without that, say so in `rig-verify-queue.md`'s defect list.

- **PR #214 is closed**; #205 carries it, and `conn_tags` survives only on
  `feat-205-drive-path-health` at `4bf6336`
  (`handoff-doc-maintenance.md:150`). The flush's "when it merges" premise no
  longer holds — see item 6 for what survives of the question.

- **#251 and the 1.1931 ms constant are both tracked** with pass/fail criteria
  in `rig-verify-queue.md`. Nothing to file. But see item 2 for the issue
  reference attached to the second one.

---

## ~~1. Four surviving "set the clock to Internal" instructions~~ — DONE 2026-08-10

> All four amended in place, plus a fifth file of the same class added by
> Markus: `handoff-doc-maintenance.md:150`, which asserted #243 closed. Each
> wrong instruction is struck with the ADAT reason and the `AutoSync` state
> underneath; no observation was deleted. The pass grep now returns only
> struck text with its correction, or statements that were already right.

The instruction is wrong and would break the stimulus path: the external
master clocks the card over ADAT, and ADAT carries `playback_5`. This is
already recorded as a defect in `rig-verify-queue.md` ("Any older instruction
to set Internal is wrong") and diagnosed in `rig-session-2-results.md:21`. The
correction did not reach the instructions themselves.

Live instructions — these tell an operator to do the wrong thing:

- `work/handoff/handoff-lock-and-smoothing.md:278`
- `work/handoff/handoff-rig-session-2.md:39`

Wrong-direction wording — softer, but asserts Internal would be an
improvement:

- `work/rig/rig-session-results.md:309`
- `work/handoff/handoff-rig-findings.md:250`

**Pass:** `grep -rn Internal --include=*.md work/ docs/ README.md | grep -i
clock` returns only statements that the clock stays `AutoSync` and why. Do not
delete the history — the two session-results entries are records of what was
observed, so amend them in place with the ADAT reason rather than removing the
observation.

---

## ~~2. `#243` — the queue names a closed issue as a live target~~ — VACATED 2026-08-10: #243 is open

**The premise was false, and it is kept here because it is the case that
proves the caveat at the top of this file.** `gh` says #243 is **OPEN**: closed
2026-08-06 01:44 UTC, reopened 02:19 the same day by the owner, who recorded
the close as his own error. No decision is needed, nothing in
`rig-verify-queue.md` changes, and the second branch below had already
happened before this file was written. The wrong claim entered here from
`handoff-doc-maintenance.md:150` — a document, not the tracker — exactly as
the header warned; that line is now corrected at its source.

The struck text follows.

> ~~`rig-verify-queue.md:29` heads the cable-change block "the one measurement
> that verifies it — **#243**", with a stated pass (the 1.1931 ms residual
> collapses to ~0) and an informative fail. `handoff-doc-maintenance.md:150`
> records #243 as **closed** as documented-not-fixed by owner decision.~~
>
> ~~The verification is real either way; only its home is in question. **This
> is Markus's decision, not the worker's.** Two branches:~~
>
> - ~~**Queue is the home.** Strip the `#243` reference from the block and
>   leave the pass/fail criteria where they are. The queue then owns a
>   verification that no issue tracks.~~
> - ~~**Reopen #243.** The block stays as written and the issue carries the
>   residual that `#248` explicitly did not close.~~
>
> ~~**Pass:** the queue block and the issue tracker agree about whether
> anything is open. A closed issue named as a live verification target is the
> trap `handoff-doc-maintenance.md` section 3 exists to remove.~~

The pass condition as written is met: queue and tracker agree that the cable
verification is open.

The generalisation is stronger than "resolve issue state against `gh` first",
though do that too. `handoff-doc-maintenance.md` was **true when written and
false thirty-five minutes later**; no review of it at the time could have
caught this, so no diligence rule applied to the reader fixes it. **Prose
should not restate an issue's open/closed state at all** — name the issue, let
the tracker hold its state, and keep in the document the reasoning, which does
not expire.

---

## 3. Three additions to `rig-verify-queue.md`

None of the three is in the file today.

### ~~3a. Snapshot PNG regeneration~~ — QUEUED 2026-08-10

> In `rig-verify-queue.md` under "Still to run", with the command, the pass,
> and the "do not read the first failure as a regression" warning. Staleness
> confirmed from history rather than assumed: the PNGs were last written at
> `de4b658` (#194); #245's reserve landed later as `d569907`. The block is
> written; **running it is still rig work.**

Five snapshots under `ac-rs/crates/ac-view/tests/snapshots/` are stale by
roughly 7 px after #252's layout shift. All five tests are `#[ignore]`
real-adapter-only (`it_transfer_snapshots.rs`, reason string *"real-adapter
only (wgpu); run on 192.168.9.25 per A3 policy"*), so nothing offline catches
it and the next rig run pixel-diff-fails all five at once.

That failure will read as a regression. It is not one, and it is the third
instance of the pattern in item 5.

    UPDATE_SNAPSHOTS=1 cargo test -p ac-view --test it_transfer_snapshots \
        -- --ignored --test-threads=1

then re-run without `UPDATE_SNAPSHOTS` to confirm the diff is clean.

**Pass:** five regenerated PNGs committed, and the second run green.

### 3b. The video — reproduce it, do not re-interpret it

Markus has a 2.4 s phone clip of `ac-view`: both traces continuous at ~1.5 s,
both **completely gone** by ~2.35 s, empty grid, no indicator in the pane. A
hand passes in front of the laptop about a second before.

The flush frames the open question as *"was the hand through the acoustic
path, or on the keyboard?"* **That is the second question now, not the
first**, and neither can be answered from the clip.

Why it matters, and why this is a *third* reachability question rather than
the known one:

- The known gap is **pre-lock**. Session 3's Run 4 confirmed it: unrelated
  legs refuse, no lock means no ladder, `FaultInput::coherence` is empty, and
  `coherence_dead` returns `false` on an empty slice. `CHECK ROUTING` has
  nothing to read. That is `fault.rs`'s "`CHECK ROUTING` remains a post-lock
  state, deliberately" section, and it is documented as a known gap.
- The video is **post-lock**. Traces were present, so the pair had locked.
  The daemon caches a pair's delay and never re-estimates (`handlers/
  transfer.rs`, `pair_delays[i].is_some()`), so `delay_locked` stays true,
  `mtw` keeps publishing, `refusing_since_s` is `None`, and `classify` falls
  through to `coherence_dead(input.coherence)` → `Fault::CheckRouting`. **That
  branch is reachable.** If the drive was on and the columns died, the banner
  should have fired.

So "empty grid" has two sub-cases the clip cannot separate:

| what happened | `coherence` | expected banner |
|---|---|---|
| columns masked, ladder still published | 504 values, <10% alive | **`CHECK ROUTING`** |
| `mtw` absent or `lengths_agree()` false | empty | *(nothing)* — correct |

The reproduction is a minute of rig time and it discriminates directly: lock
at a normal position, confirm a settled ladder, then block the mic capsule by
hand with the drive still running, and capture frames across the transition.
Read whether `mtw` survives and what the banner does.

**Before any of that, establish which build the clip was shot on.** If it
predates #234 there is no indicator to expect and nothing to explain.

**Pass:** a capture that shows `mtw` present with `coherence_dead` true and
the banner state that accompanied it. File an issue only on that evidence —
this is exactly the class of inference (`plausible mechanism asserted without
the measurement that distinguishes it`) that item 9 is about.

### ~~3c. `install.sh` under a running daemon~~ — QUEUED 2026-08-10

One line, from the "Already overtaken" section above: confirm whether
`install -m 755` still fails `Text file busy` on `ac-daemon` with the daemon
up. Zero cost alongside any other block.

---

## 4. The `#[ignore]` audit

Three times a test has been unable to **see** a defect, rather than having a
weak assertion:

1. the vacuous equality assertions;
2. `FakeEngine` lacking `last_drain_occupancy`, so ring defects were invisible
   in the one mode built to reproduce them;
3. item 3a — snapshots that cannot run in the environment where they would
   fail.

Enumerate what is `#[ignore]`d and ask, per test, **what would notice if the
thing it covers broke**. The current population is small — 22 grep hits,
roughly 11 real attributes across 8 files, and every one carries a stated
reason string, which is the right convention and makes the audit cheap:

| file | why ignored |
|---|---|
| `ac-view/tests/it_transfer_snapshots.rs` (5) | real-adapter only (wgpu), A3 policy |
| `ac-view/tests/it_stimulus_live.rs` | real daemon, M4c |
| `ac-daemon/src/audio/jack_backend.rs` (2) | needs a running JACK server |
| `ac-daemon/src/audio/contiguity.rs` (2) | emits stimulus, per-run consent |
| `ac-daemon/tests/it_loopback_ir.rs` | needs live JACK |
| `ac-daemon/tests/it_scene_fixture.rs` | fixture regeneration, manual |
| `ac-scene/tests/regenerate_fixture.rs` | fixture regeneration, manual |
| `ac-core/src/snapshot/mod.rs` | fixture regeneration, manual |

Entirely offline work; no rig time. The fixture-regeneration entries are a
different category from the rest and should probably be named as such rather
than sharing an attribute with coverage that is genuinely unrun.

**Pass:** a short written finding — per ignored test, what covers the same
defect when it does not run, or an explicit "nothing does". A row reading
"nothing does" is the useful output, not a failure of the audit.

---

## ~~5. The freq-axis comment in `ac-view`~~ — DONE 2026-08-10, comment amended, nothing filed

> The ordering check settles it: session 3's overlap was observed on `4659b25`
> (2026-08-04), and #245's fix is `d569907` (2026-08-05, merged as PR #252) —
> `4659b25` is an ancestor of it, so the fix **postdates** the observation and
> the cosmetic issue is discharged. #245 is closed. No issue filed, and no
> duplicate.
>
> `view.rs:100` now says what is true: the reserve clears the **top** edge
> only, the frequency labels at `rect.max.y` with `Align2::CENTER_TOP` still
> hang a full line **below** the rect, and that overlaps nothing only because
> nothing is drawn under a view today. The coincidence the old comment relied
> on is now stated as the condition it is, so whoever stacks something below a
> view finds the reserve they need to add.

`view.rs:100` claims the reserved half-line "keeps the whole of every view
inside its own rect". `view.rs:184` paints the frequency ticks at
`egui::pos2(x, rect.max.y)` with `Align2::CENTER_TOP`, so a full line hangs
*below* the rect. The comment is true by coincidence — only floating windows
follow `draw_view` today.

**Check the ordering before filing anything.** `rig-session-3-results.md`
records a related real overlap (the `20` tick struck through by the connection
banner, observed on `4659b25`, called out as "worth a separate cosmetic issue,
not a hold"), and #245 looks like the fix for exactly that. Whether #245
landed before or after that observation cannot be told from a snapshot.
`git log` settles it.

**Pass:** either the comment is amended to say what is actually true, or a
cosmetic issue exists — and in neither case a duplicate of #245.

---

## 6. What survives of the two-indicators question

With PR #214 closed, the question attaches to **#205**. It is unchanged in
substance: when the drive-path health check lands, two operator-facing
elements will report the same physical fault in different vocabulary — one
reads graph topology, the other signal levels. They will eventually disagree,
and a disconnected edge carrying a hot signal from elsewhere is exactly that
case.

**Pass:** the question is recorded on #205 before both elements are on screen
at once. This is a comment on an issue, not code.

---

## 7. New file: `docs/design/design-parametric-reflection-removal.md`

Zero occurrences of MEDLL, "floor bounce", "image-source" or
"quasi-anechoic" anywhere in the tree. The material below exists only in
conversation and in Markus's own notes.

**Write it as a proposal, not a plan.** Nothing here is ratified.

### The pitch is not delay estimation

The method exists because a GPS C/A chip is 1 µs wide — 300 m of path — so
multipath inside ~30 m is buried under the direct peak. Broadband acoustics is
the opposite regime:

| | bandwidth | peak width | path equivalent |
|---|---|---|---|
| GPS C/A | 1 MHz | 1.0 µs | 300 m |
| acoustic 20 Hz–20 kHz | 20 kHz | 50 µs | **1.7 cm** |

Room reflections sit 15 cm to several metres out — 10 to 200 correlation
widths, already resolved. Session 3 showed exactly this: direct at 780,
reflection cluster at 987–1020, cleanly separated. That problem is *selection*
among well-separated peaks, which is a decision rule.

### The pitch is the LF gate ceiling

Source and mic at 1.2 m height, 1 m apart. Direct path 1.0 m; image-source
path √(1² + 2.4²) = 2.6 m; excess 1.6 m = **4.62 ms**. The gate must close
before that, putting the quasi-anechoic LF limit at **~216 Hz**. Remove the
floor bounce parametrically and the binding constraint becomes the next
arrival — ceiling or side wall, 15–20 ms — dropping the limit to **50–67 Hz**.

Two octaves of valid quasi-anechoic response from the same measurement, no
hardware. The alternatives are ground-plane measurement (changes loading,
half-space only), a bigger room, or a Klippel NFS.

Byproduct: the fit hands you per-surface reflection coefficients — the
frequency-dependent absorption of a specific boundary, from one measurement.

### What this project adds that the general method does not have

- **The delay is predicted, not fitted.** Session 3 validated
  `arrival = const + d/c` against tape across eight positions to ≤5 cm. Mic
  height, source height and distance give the image-source path directly. That
  removes the most ill-conditioned parameter.
- **Model order comes from surfaces, not an information criterion.** Floor,
  ceiling, two side walls, enumerated from a room description. Order selection
  is the acknowledged soft spot of the general method; geometry dissolves it.
- **Why it is well-conditioned despite helping at LF.** At 50 Hz the
  reflection sits 0.23 of a period behind the direct — frequency-domain
  separation there is hopeless. But estimation runs in the time domain at full
  bandwidth, where the two arrivals are 444 samples apart at 96 kHz. Estimate
  where it is easy, apply the correction where it is needed. **This is the
  first objection anyone will raise; the file must answer it explicitly.**
- **Validation is conservative in the right direction.** Gate conventionally
  above 500 Hz, run the parametric fit on ungated data, compare in the
  overlap. Above 500 Hz the speaker is directional so the two paths differ
  most; at LF it is omnidirectional and the reflection is nearly a scaled
  copy. Validate at HF, deploy at LF. Add a second, independent check: does
  the *fitted* delay match the tape-measured image-source geometry? That
  validates the fit without depending on the gated comparison at all.

### The architectural requirement — record this as binding

**The fit residual must be an output, not an internal.** A gated measurement
is honest about its limit: state the gate, the frequency floor follows. A
parametrically corrected one asserts a room model, and when the model misfits
the error is invisible. That is the exact failure class three rig sessions
have gone into removing. Whatever ships must let the operator see when the
model did not fit.

### Where the literal model fights back

Scaled-and-delayed replicas are wrong for acoustics twice over: the floor
reflection leaves the source off-axis, so the speaker's own directivity
colours it, and boundaries have frequency-dependent absorption. A single
scalar gain per path will not fit. Practical compromise: reflection magnitude
per octave or third-octave, delay as a single scalar per path.

Not unexplored territory — it overlaps sparse deconvolution of room impulse
responses and subspace methods such as matrix pencil on early RIRs. It is just
not something measurement tools ship.

---

## 8. New file: `docs/coherence-diagnostics.md`

Alongside `docs/loudness-bs1770-5.md` as a reference doc rather than a design
one — the audience is an operator asking "why is coherence low", which is the
most common question this instrument will ever get.

**Half of this is already tracked and must not be duplicated.** The delay
tolerance table, the 616 µs derivation and its 625 µs measurement, the clock
drift figures, the re-lock interval and the PPO coupling are all in
`handoff-lock-and-smoothing.md` decision 5. Cross-reference; do not restate.

What is untracked and needs writing:

### Gain cannot reduce coherence

It cancels algebraically: `|Gxy|²` scales as (ab)², `Gxx` as a², `Gyy` as b²,
and the ratio is invariant. The *evidence* is tracked in five places (Run 7 —
20 dB of input gain moved stage 0 coherence by 0.006, and a mic 15 dB below
the reference read 1.0 on a loopback). The *mechanism* is nowhere, so the
finding reads as an empirical accident rather than an identity.

What looks like a gain effect is SNR: `γ² = SNR/(1+SNR)`.

| SNR | γ² |
|---|---|
| 20 dB | 0.990 |
| 10 dB | 0.909 |
| 6 dB | 0.799 |
| 0 dB | 0.500 |

### The two diagnostic rules

These are the operator-facing payoff and the reason the file exists.

- **Absolute or relative?** Does the loss track the *absolute* level of the
  quiet leg, or the *ratio* between the legs? Absolute means the noise floor,
  and no implementation could do better.
- **HF-first or broadband?** HF-first loss is aggregation bandwidth (phase
  rotation across a column, `sinc(τ·BW)`). Broadband loss is window overlap or
  SNR.

Note alongside them that stage 0 at 0.755 is reverberation-limited and is not
a defect — already recorded in four places, and it is the first thing an
operator will misread.

### Why Open Sound Meter appears stricter — **as a conjecture, labelled**

OSM's documented default is 24 points per octave against ac's 48, which makes
it about twice as delay-intolerant at HF, independent of FFT size. That is
most likely the whole explanation for its "loopback must match the signal
path" strictness — aggregation bandwidth, not anything about how it computes
coherence.

**OSM's internals have not been verified.** Write this as the mechanism that
fits, not as a claim about their code. If it goes in unqualified it becomes
the next thing on the error list.

---

## 9. The one thing that outlives this file

Not a doc task. It belongs in `.agents/` — `architect.md` or `qa.md` — under
Markus's ratification, and it is here only until that happens.

The flush records ten wrong inferences: ρ = 1/6 applied to a bias quantity, a
circular tolerance anchored on the estimator's own predecessor, "reflection
structure" read from three lags against an unrecorded speaker configuration,
an `ac-scene` fallback bug inferred from grep line numbers without reading the
code, an assumed view mapping, "check timestamps not build output", "set the
clock to Internal", ~3.05 ms latency inferred where 1.19 ms was measured, the
repeatability rule proposed as a correctness test, and `((W−D)/W)²` as the
dominant HF mechanism.

**The shape is the same every time:** a plausible mechanism asserted without
the measurement that would distinguish it from an equally plausible
alternative. None was caught by reasoning. All were caught by the rig, by
someone reading the code, or by an agent scoring the data.

The rule that follows, and the thing worth ratifying: **treat any mechanism an
agent proposes as a hypothesis with a test attached, and prefer the test.**
Where a number is derived rather than measured, the derivation is usually
right in form and wrong about which quantity it applies to. `ρ = 1/6` survived
four checkpoints because each verified the arithmetic rather than whether the
formula applied — and it then reached the code by way of a QA brief, which is
the path this rule has to close.

---

## 10. Every `.agents/` spec describes a repo layout that no longer exists — **extends #184**

Found 2026-08-10 while reading `developer.md` for a role brief. Not one file —
**all five specs**, and the tracked-item case is that this is the first
doc-correctness defect here that **misdirects work** rather than misinforming
a reader. Agents cannot edit `.agents/` unilaterally, so it waits on Markus,
like item 9.

> **This is already filed, and the filing is narrower than the defect.**
> **#184** (open since 2026-07-24, `ready-to-implement`) has "regenerate the
> module maps" as its first acceptance criterion — but names only
> `architect.md` and `developer.md`, and scopes the work as regeneration.
> `qa.md`, `triage.md` and `ux.md` carry the same dead layout and are not in
> it, and its **out of scope** line ("any content change beyond regenerating
> the module maps") reads as excluding the three unfailable checks below,
> which are the part that misdirects work. #184's own framing supports
> widening it — it was opened because "the architect pass on #180 had to
> explicitly note it was orienting from the tree instead". Widen #184 rather
> than filing a second issue; two issues over one spec file is how the
> `blocker`/`blocked` collision it also tracks came about.

The specs describe three crates — `ac/src/{main,estimator,session,level,
signal}.rs`, `thd_tool/src/`, `ds/src/`. The tree is a five-crate workspace
under `ac-rs/`: `ac-core`, `ac-daemon`, `ac-cli`, `ac-scene`, `ac-view`. There
is no `thd_tool` and no `ds` anywhere.

**Read `architect.md:109` first.** It is a design-review checklist line:

    - Three crates (`ac`, `thd_tool`, `ds`) cleanly separated?

A reviewer answers it about a structure that does not exist, so it cannot come
back negative — **item 4's pattern verbatim**, sitting in the file that steers
design review. Two more of the same shape:

- `qa.md:198` — standards conformance "for each output value in `ac`,
  `thd_tool`, `ds`", against the standards table in that spec.
- `ux.md:188` — an audit instructed to read every stdout path "across `ac`,
  `thd_tool`, `ds`". The real output surfaces are `ac-cli` and `ac-view`, and
  `ac-view` is not text at all, so the instruction misses the entire graphical
  surface it was written before.

The rest are references rather than unfailable checks, and the population is
larger than a first pass suggests — `grep -n 'thd_tool\|ds/src\|ac/src' .agents/*.md`:

| file | lines |
|---|---|
| `architect.md` | 21, 38, 109, 118, 142 |
| `developer.md` | 22, 29, 34, 44 |
| `qa.md` | 13, 54, 58, 119, 193, 198, 214, 215, 222 |
| `triage.md` | 12, 29 |
| `ux.md` | 10, 34, 188, 193 |

Some carry substance that outlived the layout and must be re-pointed rather
than deleted — `qa.md:54` names the Müller & Massarani PDF as the estimator's
primary reference, which is still true of `ac-core`; the `thd_tool` standalone
invariant (`architect.md:38`, `developer.md:44`) is about a tool that no longer
exists and simply goes.

**Pass:** every crate name in `.agents/` resolves in `ac-rs/`, and each check
that named the old layout either names the current one or is removed — with
`architect.md:109`, `qa.md:198` and `ux.md:188` rewritten so that a wrong
answer is *possible*. A checklist item nothing can fail is worth less than no
item, because it reports coverage it does not have.

**The sweep is not "delete every `thd_tool` mention."** Two kinds of line are
mixed together and only one of them goes. A line naming a tool that no longer
exists is dead — the `thd_tool` standalone invariant (`architect.md:38`,
`developer.md:44`, and its checklist row at `architect.md:142`) has no
referent and goes. A line carrying a **live reference wearing a dead label**
must be re-pointed instead: `qa.md:54` names the Müller & Massarani PDF as the
primary reference for the H1 estimator, which is still exactly what
`ac-core/visualize/transfer.rs` implements, whatever `ac/src/estimator.rs` in
the surrounding sentence says. A blanket delete costs a standards pointer,
which is what `stddocs/` discipline exists to prevent. Read each of the 24
lines; the grep locates them, it does not judge them.

---

## Suggested order

> **State 2026-08-10.** Done: 1, 5. Vacated: 2 (#243 was open all along).
> Written into `rig-verify-queue.md` but not yet run: 3a, 3c. Untouched: 3b
> (needs the build the clip was shot on identified first), 4 (qa in audit
> mode — read-only coverage map, not developer work), 6, 7 (architect's
> output, not developer's), 8, 9, 10. Items 6, 9 and 10 are Markus's; 9 and 10
> are both `.agents/` and should land together.

1. Item 1 — the wrong instructions, first, because a rig session could read
   them tomorrow.
2. Item 2 — Markus's decision; unblocks the queue edits.
3. Item 3 — the three queue additions.
4. Item 6 — one issue comment.
5. Item 5 — check `git log`, then file or amend.
6. Items 7 and 8 — the two new documents.
7. Item 4 — the audit, which has no deadline and will produce work of its own.
8. Items 9 and 10 — Markus ratifies, or they die with this file.
