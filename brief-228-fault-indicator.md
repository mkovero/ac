Role: worker on issue #228 (fault indicator). Read .agents/ for your role spec first,
per CLAUDE.md.

GATE — read before planning any work: #228 waits on #227 landing. #227 is PR #232,
still OPEN, branch issue-227-earliest-prominent-peak at f54b2cc (5 commits ahead of
main). Do not merge #228 ahead of it, and do not treat the two as parallel tracks.

The reason is not that #227 moves any of #228's thresholds. It is that #227 converts
silent wrong locks into refusals, and a refusal is invisible without #228: h1_estimate
falls back to unaligned zero, which collapses HF exactly like a bad lock did. So a
refusing session presents as a blank top end — arguably worse for an operator than the
confident wrong answer it replaced. #228 is what makes #227's improvement legible.

This overrides handoff-issue-strategy.md, which you are about to read: that document
says #227 is "Independent of #226 and #228 — different crate, different code — so it
can run in parallel with them" (line 76), and sequences #228 as item 2 and #227 as
item 3. That line is superseded. Everything else in the document still stands.

You can do the rebase and the implementation while #232 is open. What waits is landing.

Repo: /home/mui/src/ac is a shared checkout with concurrent sessions. Do not work in it.
Create a worktree on disk (not /tmp — tmpfs is too small for cargo):

    git -C /home/mui/src/ac fetch origin
    git -C /home/mui/src/ac worktree add /home/mui/src/ac-wt-228b issue-228-fault-indicator

A worktree at /home/mui/src/ac-wt-228 already exists at the same branch tip; use it
instead of a second one if it is free.

Read, in this order, from the main tip (git fetch origin first — these documents are
still being added to):

  1. state-live-spectrum.md          — current state of the live spectrum path
  2. handoff-issue-strategy.md       — how the issues relate and what order they land in,
                                       subject to the gate above
  3. handoff-lock-and-smoothing.md, the "#228 — becomes load-bearing, and gains the
     full state set" section only (currently lines 164-203). The rest of that file is
     ratified decisions for #226/#227; read them only if #228 turns out to depend on one.

Four facts not yet in those documents:

  1. Deliverable 1 has already landed. Commit 1f78729, "feat(daemon): publish observed
     drive state on transfer_stream frames", is on issue-228-fault-indicator. It was
     branched from bd40ed4 — main *before* the #233 merge — so your first task is to
     rebase it onto the current main tip, which contains a14ee4a, the #233 merge.
     #233 is the #225 reference-output fix and it touches
     ac-rs/crates/ac-daemon/src/handlers/transfer.rs, the same file deliverable 1
     edits. Expect to resolve that overlap by hand; do not resolve it by dropping
     either side. Verify with `cargo test` in ac-rs/ after the rebase, not before.

     Rebase onto main, not onto #227's branch. The gate is about merge order, not about
     building on unmerged work.

  2. `drivable` is already on the wire and is a real addition to the spec worker1
     wrote — do not re-derive it or design around its absence. It is on main via #233
     and documented in ac-rs/ZMQ.md. Semantics:

       drivable=true  — output ports opened and connected at launch, but silent;
                        emission stays gated on set_drive
       drive=true     — opened, connected, and emitting at level_dbfs; implies drivable

     This is what makes the #228 state table implementable as written: it distinguishes
     "could drive, is not driving right now" from "never drives at all". The table's
     first row ("not driving" -> show nothing, idle and expected) is the former;
     a session that is not drivable at all is the latter, and must not be reported as
     a fault. ac-view already launches sessions with drivable: true
     (ac-rs/crates/ac-view/src/session.rs).

  3. LOST LOCK reads `delay_locked: false`. It is NOT inferred from top-stage coherence.

     This supersedes the "both legs live, HF collapsed, LF fine" discriminator in the
     #228 table you will read, and the 0.715-0.755 good / 0.05-0.06 bad coherence
     figures that go with it. Those were designed when refusal did not exist and the
     only evidence of a bad lock was its downstream effect. #227 makes the estimator
     say so itself: coherence is a symptom, `delay_locked` is the cause. It also
     removes the hardest threshold in the set — stage 0 sits at 0.755 legitimately in
     a live room, so any coherence threshold risks flagging a healthy measurement.

     Coherence may still be worth a *secondary* indication: a valid lock with poor HF
     coherence is a real condition. But it is a different message — "poor coherence",
     reverberation-limited — and must not be conflated with a lock fault.

     Two constraints from ac-rs/ZMQ.md (transfer_stream frame, ~line 1603) that the
     implementation has to respect:

       - `delay_locked: false` is also what a pair publishes *while warming up*, not
         only on refusal. Warmup must not present as a fault.
       - `delay_prominence` is documented "Diagnostic only: nothing downstream may gate
         on it, since the threshold is the estimator's to own." Accepted at >= 24;
         null before the first attempt. Display it if useful, do not branch on it.
         If you conclude #228 genuinely needs to read it — including reading null-vs-
         present to separate warmup from refusal — raise that as a spec question
         rather than doing it quietly. Silently gating on a value the estimator owns
         is how the two ends drift apart.

  4. A persistent refusal needs different words than a transient one. #227 retries at
     1 Hz; a mic at 3 m off-axis may never lock. The operator needs "move the mic",
     not a blank display and not a message that reads as a passing glitch. #226 owns
     retry policy, but #228 owns what is on screen, so handle the distinction now
     rather than shipping both cases as one message. Drive it off elapsed time in the
     refusing state, not off prominence — see the constraint above.

     **Threshold: no lock 10 seconds after the ladder settles.** The ladder settles at
     2.560 s (design-mtw-ladder.md, stage 2 — four independent 1.024 s windows need
     2.56 s of audio), and #227 retries at 1 Hz, so 10 s is roughly ten retries past
     the first point at which a lock was even possible. Anything past a handful of
     retries is genuinely persistent.

     The number is arguable, not sacred — argue it in review if the rig says otherwise.
     What is not open is leaving it unset: an invented threshold gets decided silently
     and re-litigated later.

     The clock starts *after* the ladder settles, not at session start. This is the
     interaction with fact 3: `delay_locked: false` during warmup is indistinguishable
     from a refusal, so a timer started at t=0 fires the persistent-refusal message on
     healthy sessions. Same reason LOST LOCK must not paint during warmup — a fault
     indicator that cries wolf on startup gets ignored, which defeats the point of
     having one.

     **SUPERSEDED BY #238 — the anchor, not the number.** "After the ladder settles"
     is undefined for the case it was written for: a pair that never locks never
     builds a ladder, so both refusal states were unreachable as shipped. Settle was
     standing in for "a lock was possible by now"; the daemon now publishes
     `delay_attempts`, which observes that directly, and the clock is anchored on the
     first refused attempt. For a never-locked pair that fires 2.56 s earlier than
     this text implies. The 10 s number is unchanged, and so is the reasoning for it.

     **Also superseded: LOST LOCK's scope.** It is now only for a pair that held a
     lock and lost it. A pair that has never locked shows NO LOCK from the start,
     without the instruction, which the persistent row adds at 10 s. LOST LOCK on a
     session that never locked asserts something untrue, which is the failure this
     brief exists to prevent.

     **What #238 does not make reachable: LOST LOCK.** It needs `delay_locked` to
     go true and then false, and today's daemon estimates a pair's delay once and
     caches it, so the flag is monotone for the life of a session. The row is
     written and tested against #226's producer, not this one. A rig tester should
     expect NO LOCK from a refusing session and should not read the absence of
     LOST LOCK as a defect in #238.

Constraint on stimulus, if any part of this reaches the rig: -40 dBFS maximum, and
never emit without explicit per-run consent from Markus.

Build gates, run in ac-rs/: cargo test | cargo clippy -- -D warnings | cargo fmt --check
