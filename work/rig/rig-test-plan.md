# Rig test plan — 2026-08-22, 192.168.9.25

> **Executed 2026-08-22, partially.** Results in
> `work/rig/rig-2026-08-22-tau-loopback-results.md` (P, A1, A2a) and
> `work/rig/rig-2026-08-22-onset-distance-results.md` (B1 at both positions).
>
> - **A1 (#341 / PR #355): pass** — the Babyface leg reproduces #277 exactly.
>   PR unblocked.
> - **B1 (#346 AC5): fail, with a mechanism** — #353's floor coupling is active
>   at ordinary SNR and makes the onset's error distance-dependent, so it does
>   not cancel in an increment. 22× the criterion by two independent measures.
> - **A2a: τ = 43.7500 ms, corroborated** — #347's mechanism works on hardware.
> - **Not run:** B2 (#356 AC7), the A2 period ladder, A3's repeat statistics,
>   `CHECK ROUTING` post-lock. Reasons recorded in the Track B file.
> - **Filed:** #358, #359, #360, #361.
>
> **Read the two result files, not this plan, for what is true.** Where they
> disagree with the expectations written below, the session is right. The parts
> of this plan still worth executing are the four blocks named above; the
> temperature discussion below is superseded by the operator obtaining a
> thermometer and a laser distance meter.

Written for a single on-site session. Scope: everything currently carrying
`requires-rig`, plus the queue items that ride along at no extra cost.

**Host: 192.168.9.25** (RME Babyface Pro on pipewire-jack, speakers on ADAT out
through the external converter, mic on IN1). Confirmed with the operator at
plan time. Every session record in `work/rig/` was taken here; 192.168.9.40 has
no τ, no converter constant and no noise floor on record, so a result taken
there would not compare against anything.

**Acoustic scope: full** — mic set, taped 1.000 m and 3.000 m on axis.

**Drive: −30 dBFS, operator-authorized for this session, server-side clamped.**
Above the standing −40 dBFS cap, so both halves of the exception apply: the
authorization is recorded in each session record, and `drive_max_dbfs: -30.0`
must be in the config of the daemon actually running (`HOME=/home/mui/rig2-home`
is the existing one that has it). A request-side limit is not the interlock.
Per-run consent is still taken before each emitting block; this only pre-agrees
the ceiling.

This plan expires when the session runs. Its blocks are marked executed against
the record files they produce, same convention as `rig-verify-queue.md`.

---

## Standing interlocks — read before touching anything

These are from `.agents/rig.md` and are blocking, not advisory.

1. **No emission without per-run operator consent.** Consent does not carry
   between runs, and not from a previous session.
2. **−30 dBFS ceiling, clamped server-side.** Verify `drive_max_dbfs` in the
   running daemon's config before the first emitting block, not after.
3. **Stop the daemon before installing over it.**
4. **Verify every binary by sha256.** Size and mtime have already produced a
   false pass on this rig. `install.sh` prints the hashes — read that output.
5. **Clock stays `AutoSync`** (`numid=320` = 0). The external master clocks the
   card over ADAT and ADAT carries the stimulus leg. Setting `Internal`
   silently kills the speaker path instead of erroring.
6. **Record what is physically connected, by port index, this session.** Not
   what this document says. A stale wiring assumption inherited from a handoff
   cost three sessions once.
7. **Confound is a required field on every run.** "None identified" is a valid
   entry; blank is a defect in the record.
8. **Decline to conclude is a valid outcome** and is preferred over forcing a
   pass/fail the data does not support.
9. **No source edits, no PRs, no issue transitions in-session.** A defect found
   becomes a new issue or a note against a queue block, referencing the record.

---

## Hardware requirements

Set up before the session starts; the build matrix below runs while this is
being taped.

**Signal path**

- [ ] Babyface Pro on pipewire-jack, **96 kHz, period 1024, nperiods 4** — the
      #277 / #243 configuration, so results compare against those records.
- [ ] **Electrical loopback: AN2 → IN4** (`playback_2` → `capture_4`). Cable,
      not acoustic. Used by Track A; **leave it patched through Track B** — it
      is on different channels from the mic and reference legs, so nothing has
      to be re-patched mid-session.
- [ ] **Stimulus leg:** speaker A on `playback_5` (ADAT) through the external
      converter.
- [ ] **Reference leg:** `playback_7` (ADAT3) → converter → analogue out →
      `capture_3` (IN3). Already patched as of 2026-08-18; confirm, don't
      assume.
- [ ] **Mic on IN1**, 48 V on, PAD off, preamp `numid=301` = 36.
- [ ] Clock `AutoSync`, `numid=320` = 0.

**Physical**

- [ ] **Tape marks at 1.000 m and 3.000 m on axis from speaker A**, capsule at
      the mark. Marks from 2026-08-18 may still be down — measure them again
      rather than trusting them. The mic is currently at session 3's near-wall
      position (2.4 m from A, 28 cm off the wall, off axis), so it moves.
- [ ] **A thermometer at capsule height.** This is not optional this session —
      see the note under Track B; the temperature uncertainty is comparable to
      the pass criterion it feeds.

**Machine**

- [ ] `jack_iodelay` available — it is the external truth Track A scores
      `measure_tau` against.
- [ ] Space for binaries only (~150 MB), **not** for a build tree. Nothing is
      compiled on this box — see "Build on the VM" below.

---

## Build matrix

Four trees. Three are unmerged branches; `main` is the control.

| ref | PR | what it is for |
|---|---|---|
| `main` @ `0ef2c81` | — | control; Track A2/A3 run against it (the τ fixes are merged) |
| `issue-341` | #355 | Track A1 — the loopback thresholds under test |
| `issue-346` | #352 | Track B1 — the onset estimator |
| `issue-243` | #356 | Track B2 — the gated metres readout |

### Build on the VM, never on 192.168.9.25

**192.168.9.25 is the hypervisor host that the development VM runs on.** It is
a test rig, not a build box: it holds the hardware, and its RAM is committed to
the guests it is running.

**This was learned the expensive way on 2026-08-22.** Four `cargo build
--release` runs were started there, each with its own target directory. The
host's OOM killer took a `qemu-system-x86` at 24 GB RSS — a running VM — while
the builds proceeded. The audio stack survived (pipewire up, JACK still
96 kHz / 1024), which is luck, not design: a rig session that loses JACK
mid-block loses the client lifetimes every absolute τ reading is scoped to.

So:

- **Build on the development VM.** `mold` is installed there, so
  `ac-rs/.cargo/config.toml` works unmodified and no `RUSTFLAGS` override is
  needed. The override (`RUSTFLAGS="-C target-cpu=native"`, which drops the
  linker choice and keeps the codegen) is only for the case where something
  must genuinely be built on a box without `mold` — it is not the normal path.
- **Copy binaries to the rig**, into `~/rig-<date>/bin/`, under distinct names
  (`ac-daemon-issue-346`, `ac-view-issue-243`, …).
- **Integration-test binaries travel too.** `cargo test --no-run` on the VM
  produces a self-contained binary under `target/release/deps/`; copy that
  across and run it on the rig with its environment set. There is no need for a
  toolchain on the rig at all.
- **Record `sha256sum` on the rig, after the copy** — that is the hash that
  says what actually ran, and it is the whole point of the rule.

**Do not `install.sh` four times.** Run the binaries by explicit path instead.
This keeps the hash discipline while removing three stop-daemon/install/restart
cycles from the middle of the acoustic block, where a mistake is expensive
because the mic has not moved yet.

**One trap in the side-by-side scheme, seen on 2026-08-22:** binaries built in
different target directories differ by sha256 even when the source is
identical, because absolute paths are baked into panic messages. `issue-341`
changes only a test file, so its `ac-daemon` is functionally identical to
`main`'s — but the hashes will not match. **A differing hash proves the file
differs; it does not prove the code differs.** Check the diff, not the hash,
when deciding whether two refs need separate runs.

**And the reverse trap, which is worse and also happened:** a *shared*
`CARGO_TARGET_DIR` across several worktrees of the same repo went false-fresh —
three of four refs reported `Finished in 0.3s` without compiling, and the same
binary was copied out under four names with four identical hashes. Identical
hashes across refs that should differ is the signature. Use one target
directory per ref, and read the build log for an actual `Compiling ac-daemon`
line per ref before trusting the output.

---

## Ordering, and why

**P (pre-flight) → A1 → B → A2/A3 → C.**

- **A1 before B** because it is five minutes on a cable already patched, and it
  is the only check that the measurement chain on this box is sound before
  anything acoustic is read off it.
- **B before A2/A3** because B is the only work that unblocks a merge. #352 and
  #356 are both held open by exactly one rig criterion each. A2/A3 produce
  constants for issues with no PR behind them — valuable, but they lose nothing
  by being second, and B loses everything by being cut. The acoustic half has
  been cut for time in three consecutive sessions; put it where that cannot
  happen again.
- **B's two builds share one position set.** #346 AC5 and #356 AC7 both score
  the *increment* over the taped 2.000 m. Capture both builds at 1.000 m before
  the mic moves, then both at 3.000 m. Moving the mic twice instead of four
  times is the single biggest error reduction available in this session.
- **C last, and outside the acoustic budget.** No mic, no emission. It competes
  with nothing, which is exactly why it is normally first to be cut — a
  category error.

---

## P — pre-flight (no emission)

1. Stop any running daemon.
2. Build the four refs (above). Record `sha256sum` for every binary used.
3. Confirm clock `AutoSync` (`numid=320` = 0) and record it.
4. **Confirm wiring by probe, per leg, by port index.** Write down what is
   actually connected — this is a required field in both records.
5. Confirm `drive_max_dbfs: -30.0` in the config of the daemon that will run.
6. Measure τ with `jack_iodelay` at period 1024 and write it down. This is the
   session's reference number and Track A scores against it.
7. Read the thermometer. Record it, with the time.

**Free ride-along, costs one line:** does `install -m 755` over a running
`ac-daemon` fail `Text file busy`? Never established either way, and the script
runs under `set -e`, so a failure aborts before the sha256 lines print. Try it
once, deliberately, then stop the daemon and install properly.
> **Pass:** a stated answer, either way, added to `rig-verify-queue.md`'s rig
> defects list.

---

## Track A — electrical. Cable only, no mic, no room emission.

### A1 — #341 / PR #355: the Babyface leg of `it_loopback_ir`

The one thing holding #355. Its dummy legs are verified (48 kHz and 96 kHz, run
independently by QA); the Babyface leg was hand-checked against #277's recorded
numbers, never executed.

```
AC_LOOPBACK_OUT="Babyface Pro Pro:playback_2" \
AC_LOOPBACK_IN="Babyface Pro Pro:capture_4" \
AC_LOOPBACK_LEVEL_DBFS=-40 \
cargo test -p ac-daemon --test it_loopback_ir -- --ignored --nocapture
```

at 96 kHz / period 1024. Note `-40`, not `-30` — the runbook specifies it and
this leg is a cable; there is no reason to drive it harder.

> **Pass:** the test reports ok. Expect the peak near **12392–12404** against a
> bound of roughly **[8096, 13952]**, and SNR well above the 25 dB floor —
> #277 measured **36.71 / 36.85 / 36.89 dB** on this exact leg.
>
> **Falsifying outcomes, each of which means something different:**
> - peak outside the bound → the geometry in #355's re-derivation is wrong;
> - SNR below 25 dB → *not* a threshold problem. #277 got 36.9 dB here, so a
>   failure means something in the path regressed, and Track B's numbers become
>   suspect before they are taken;
> - `window_len_used[0] != linear_ir.len()` → the new assertion is catching the
>   indexing bug it was added for.

**Known gap, do not try to close it here:** `MAX_ROUND_TRIP_S = 60 ms` never
binds at `DEFAULT_DURATION_S = 0.5` — the gap-clamped half-window is ~30 ms at
any sample rate, so `hi_bound` saturates at the window edge on every runnable
config. This run does not exercise the constant either. QA already recorded it;
it is a dev change (longer default duration, or a debug assert), not a
measurement.

**Confound to watch:** the one-period-per-client jump. `jack_iodelay` and the
test are separate clients, so their absolute round trips are only comparable
within one client's lifetime. Take the P6 reference reading and this test in
the same stretch, and do not restart JACK between them.

### A2 — #340 / #350: where `measure_tau` actually stops working

`ac calibrate`'s τ path **has never been exercised on this rig.** The
2026-08-18 session says so explicitly: #340 and its Result 5 are both reasoned
from measurements taken outside it. This is the first run through the real
thing.

Geometry, from `calibrate.rs` on `main` — worth having in hand, because it is
not what the old comment said:

- `n_harmonics = 1`, so the harmonic-gap clamp **never runs**. The requested
  window is the binding bound (this is what #349 fixed).
- At 96 kHz: `half` = 4800 samples, `window_len` = 9600, edge margin =
  `round(0.10 × 4800)` = **480 samples = 5.0 ms**.
- Largest τ accepted = **4319 samples = 44.99 ms**, at every sample rate.
- The rig's 43.75 ms (4200 samples) clears that by **119 samples = 1.24 ms**,
  which is 12.5% of the half-window — just past the 10% margin.

**The experiment: walk τ toward the edge with the JACK period.** Round trip is
roughly `2 × period + fixed`; at period 1024 the fixed part comes out ~2152
samples. Predictions, to be falsified by `jack_iodelay` at each setting rather
than assumed:

| period | predicted τ | as % of half-window from the edge | expected |
|---|---|---|---|
| 256 | 2664 smp / 27.8 ms | 44.5% | accept |
| 512 | 3176 smp / 33.1 ms | 33.8% | accept |
| 1024 | 4200 smp / 43.75 ms | 12.5% | accept, narrowly |
| 2048 | 6248 smp / 65.1 ms | outside | **refuse** |

At each setting: `jack_iodelay` first (external truth, same client discipline
as A1), then `ac calibrate` on the loopback leg, capturing the full `cal_done`
frame — `tau_state`, `tau_s`, `tau_agreement_count`, `tau_period_size`, and the
refusal text when it refuses.

> **Pass, and there are two separate things being scored:**
> 1. **The refusal path fires and says the right thing.** At period 2048 the
>    peak is outside the window, and the failure must name the measured peak
>    position, the window's half-width, and the fact that the arrival is
>    outside it. A returned number here is #340 reopening.
> 2. **`measure_tau` agrees with `jack_iodelay`** at every setting it accepts.
>    Disagreement by a multiple of the period is the #347 jump, not an
>    estimator error — score it as such. Disagreement by anything else is a
>    finding.
>
> **The surprising outcome to watch for:** period 1024 refusing. It sits 119
> samples inside the margin, which is a small number of samples for a peak that
> has to be located against a real noise floor. If it refuses intermittently,
> that is `TAU_EDGE_MARGIN_FRAC = 0.10` being too aggressive for this rig at
> its normal configuration — and it is the single most useful thing this block
> can produce, because it turns #350 from a question into a measurement.

**#350 cannot be fully answered this session, and here is why — decide before
running, not after.** The issue asks where peak-detection SNR degrades near the
window edge. The only lever this rig has on edge proximity is τ itself, and τ
moves in period-sized steps: 44.5%, 33.8%, 12.5%, then off the end. There is
no way to sample between 0% and 12.5% with the current binary, because the
half-window is a compiled-in constant. So:

- what this session **can** settle: that the refusal fires, that the accept
  band is real at four points, and whether 12.5% is stable at this rig's noise
  floor across repeated runs;
- what it **cannot**: the shape of the degradation between 0% and 10%, which is
  where `TAU_EDGE_MARGIN_FRAC` is actually decided.

Closing the rest needs `TAU_MIN_HALF_WINDOW_S` reachable from the environment
in a test build — a dev change, filed as such after this session, not attempted
here.

**Capture:** per-run frames, not counters. Every `cal_done` frame, plus the
`jack_iodelay` output that goes with it.

**Confound:** changing the JACK period restarts the graph, and therefore every
client. Absolute τ comparisons across settings are comparisons across client
lifetimes, which is precisely the #347 hazard. Take `jack_iodelay` and
`calibrate` **within each setting** and compare the pair, never a number from
one setting against a number from another.

### A3 — #347: does the period jump actually reach `calibrate` in the wild?

#348 is merged: `measure_tau_twice` takes two readings in separate client
lifetimes and refuses when they disagree, naming the period when the delta is a
period multiple. The synthetic case is unit-tested. Whether the fault it guards
against is reachable through the real stack has never been observed.

At period 1024, run `ac calibrate` **ten times**, each a fresh invocation.

> **Pass — and both outcomes are results, so say which was expected before
> starting:**
> - Every run stores `tau_state: measured` with `tau_agreement_count >= 2` and
>   the τ values agree with the P6 `jack_iodelay` reference → the corroboration
>   works and the jump did not occur in ten tries. Record the ten values; the
>   spread is itself the datum.
> - At least one run refuses, naming the period → **the guard fired on real
>   hardware for the first time.** This is the more valuable outcome. Capture
>   the full refusal text and both readings.
>
> **Failure:** a run stores a τ that is one period off the `jack_iodelay`
> reference with `tau_agreement_count >= 2` — meaning both readings jumped
> together and the corroboration did not catch it. That is #347 unfixed for the
> case that matters, and it is a new issue on the capture.

**Confound:** ten invocations in quick succession may not sample the condition
that produces the jump. If none refuses, the record must say "not observed in
ten attempts", not "does not occur".

---

## Track B — acoustic. Mic, emission, −30 dBFS clamped.

**Take per-run consent before the first drive here.**

Both blocks score the increment between the two taped positions, because the
increment cancels every constant term (converter asymmetry, acoustic centre,
capsule) and is the only external truth available. The absolute is not an
independent check — the constant is derived from these same measurements.

> **Correction, 2026-08-23 — this section omits the dominant term, and every
> band below that scores a taped distance inherits the omission.**
>
> The mic is *physically moved* between the 1 m and 3 m positions and the
> distance is measured **by hand** each time. Operator: *"you can expect +-5cm
> accuracy at the best there. anything <1cm is pure luck and very very good
> guess. this is the fact until laser and temperature meter makes itself known
> someday (dont stay waiting)."*
>
> | term over a 2.000 m increment | magnitude |
> |---|---|
> | **tape placement, hand-measured** | **±50 mm** |
> | sample quantisation @ 96 kHz | 3.6 mm |
> | temperature, ±1 °C | 3.5 mm |
>
> Tape placement dominates by more than 10×. **No criterion stated in
> millimetres can be certified against it**, so the AC7 bands below
> (`≤1.5 mm` / `1.5–8.5 mm` / `>8.5 mm`) are finer than their own ground truth
> and cannot be applied as written; #346 AC5's 4.7 mm is likewise one lucky
> draw from a ±50 mm distribution rather than a demonstrated capability.
>
> What survives, and it is the important half: **the c-free
> estimator-against-estimator comparison below is not merely the stronger
> option, it is the only valid one**, because it removes the tape entirely.
> Both estimators see the same physical move whatever the tape says it was.
> Taped-distance criteria should be restated at ~5 cm, or replaced by the
> time-domain form. Do not wait for a laser to score what is already
> scoreable.
>
> See `work/rig/rig-243-criterion7-results.md`.
>
> **Tagging, 2026-08-24 (#375 AC3).** Swept `work/rig/*.md` and open issues
> for criteria stated in millimetres against a hand-measured distance. Two
> found, both already named on this page:
> - **#356 AC7 (#243), `≤1.5 mm` / `1.5–8.5 mm` / `>8.5 mm` bands below —
>   tape-scored.** Compares a metres readout to the tape directly; `c` and the
>   tape both enter. Reported bounded, not pass/fail, per the temperature
>   band below (`decline to conclude` where the answer depends on an
>   unmeasured °C). Ran 2026-08-23: 23 mm, a pass at the ±5 cm bar the tape
>   actually supports (`rig-243-criterion7-results.md`) and a `decline` at the
>   mm-banded table as written.
> - **#346 AC5 — c-free.** The estimator-vs-estimator increment above; tape
>   and `c` both cancel. Re-derived bar in the section below.
>
> No other mm-against-tape criterion found in `work/rig/` or in an open
> issue.

### Temperature: no thermometer this session (2026-08-22)

`c` moves 0.606 m/s per °C, which over a 2.000 m increment is **3.5 mm per
°C**. #346 AC5's criterion is 4.7 mm and #356 AC7's is 5 mm, so a ±1 °C
uncertainty eats most of the pass budget before the instrument is considered.
No thermometer is available; the operator states 25–27 °C, the same range the
2026-08-18 session estimated. That range is **±1 °C about 26 °C = ±3.5 mm**.

This does not sink the session. The two criteria depend on `c` differently, and
splitting them is what makes the run scoreable:

**#346 AC5 is scoreable c-free, and more cleanly than via the tape.** Its
wording is "at least as well as `transfer_stream`'s 4.7 mm". `transfer_stream`
and the onset estimator can both be run at the same two positions in the same
session, and compared **against each other in the time domain**, where `c`
cancels exactly and the tape does not enter at all.

> ~~2.000 m at 347.06 m/s = 5.7627 ms = 553.2 samples at 96 kHz.~~
> ~~4.7 mm over 2.000 m = 0.235% = 1.30 samples.~~
>
> ~~**Pass: |Δt_onset − Δt_transfer_stream| ≤ 1.3 samples**, where each Δt is~~
> ~~that estimator's own increment between the two taped positions.~~
>
> **Superseded, 2026-08-24 (#375).** The derivation above is sound — an
> estimator-vs-estimator increment is still the only c-free, tape-free form —
> but the 1.30-sample figure was `transfer_stream` agreeing with the **tape**
> to 4.7 mm in one session (`rig-243-343-results.md`, 2026-08-18), not a
> demonstrated capability of either estimator. The tape is ±50 mm
> (`work/rig/rig-243-criterion7-results.md`), so 4.7 mm is one lucky draw from
> a distribution ±10× wider than itself; the same comparison returned 23 mm on
> 2026-08-23. Converting a tape draw to samples moves the uncertainty out of
> view without removing it — a multiple quoted against 1.3 samples inherits an
> authority the draw never had. See issue #375.

**Re-derived bar.** State the disagreement in units of the candidate
estimator's own measured repeatability instead of a tape figure, so the bar
needs no `c`, no tape, and tightens on its own as the estimator improves:

> **Pass: |Δt_est − Δt_transfer_stream| ≤ 3 × se(Δt_est)**, where `Δt_est` is
> the candidate estimator's own increment between the two taped positions,
> `se(Δt_est)` is the standard error of that increment estimated from repeated
> captures at each position in the same session, and `Δt_transfer_stream` is
> treated as the reference value.

`transfer_stream` is the reference and not a second noisy term because its own
scatter was `sd 0` across `n=3` fresh locks at each taped position in the
2026-08-23 session (`rig-2026-08-23-onset-353-results.md`) — deterministic to
sample resolution at that repeat count, and negligible next to any candidate
estimator's `se`.

> **Record committed, 2026-08-24 (this PR, in response to QA on #375's PR).**
> `work/rig/rig-2026-08-23-onset-353-results.md` and its raw logs
> `audit/rig-353-2026-08-23/` (`ir-1m.log`, `ir-3m.log`, `ladder-3m.log`,
> `xfer-locks.txt`) were on disk from the 2026-08-23 session but had never
> been committed — QA's `git log --all` correctly found nothing. Both are now
> in this tree; the n=12/n=3 table below is read from the committed file, not
> reproduced from issue-body prose. `xfer-locks.txt` shows the raw per-session
> `transfer_stream` locks (392/942 samples, zero spread across 3 fresh locks
> each) that the 550.00-sample increment above sums to.

**This bar assumes `se(Δt_est)` is estimated from n=12 captures at each of the
two taped positions**, per the 2026-08-23 session table (also quoted in #378):

| estimator | increment, 1.000 m → 3.000 m | own se | repeats/position |
|---|---|---|---|
| `transfer_stream` | 550.00 smp | sd 0 | n=3 (fresh locks) |
| IR peak | 557.42 smp | se 0.60 | n=12 |
| onset (#352) | 575.67 smp | se 3.26 | n=12 |

A scatter estimate from fewer repeats — n=3, say — is a weaker claim than one
from n=12 and should not be plugged into this bar without restating the
resulting `se` and flagging the smaller n. 3σ is used because it is the
conventional threshold at which measurement noise stops being a plausible
explanation for a disagreement (>99.7% under a normal approximation): a result
inside it cannot be distinguished from agreement using only the estimator's
own measured scatter, and one outside it is a finding independent of tape,
temperature, or `c`.

> **Multiplier not yet accepted, 2026-08-24.** 3σ is this derivation's own
> proposed convention, not a value #375 or the architect's implementation note
> hands down. It needs a human comment on #375 accepting 3σ specifically (or
> naming a different multiplier) before this bar is treated as settled rather
> than proposed.

Applying this bar to #346 AC5 and restating the verdict is AC4 of #375, and is
deferred to #378: #378 is expected to move the onset's own increment and
`se`, so scoring the current numbers against this bar now would score code
about to change. The illustrative case is already on record — the same
2026-08-23 onset numbers above disagree with `transfer_stream` by **25.67
smp**, which is **7.9σ** on the onset's own `se` of 3.26 — but that is
evidence for #378's problem statement, not this issue's verdict.

This remains a stronger test than the tape comparison, not a weaker substitute
for it: it removes the temperature uncertainty *and* the tape-placement
uncertainty, and it measures the thing AC5 actually asks about — whether the
new estimator inherits the accuracy of the one already validated against tape.
Record the raw sample counts and each position's own scatter, not just the
increment's difference.

**#356 AC7 is not scoreable c-free, and must be reported bounded.** It compares
a metres readout against the tape, so `c` is in the answer. With the assumed
26 °C, a measured deviation `X` from 2.000 m means the true deviation lies in
`[X − 3.5, X + 3.5]` mm. So, decided before running:

| measured \|X\| | verdict |
|---|---|
| ≤ 1.5 mm | **pass** — holds across the whole 25–27 °C band |
| 1.5–8.5 mm | **decline to conclude** — the answer depends on the temperature, which was not measured |
| > 8.5 mm | **fail** — no temperature in the band rescues it |

Write the verdict in that form, with `X` and the band, rather than as a bare
pass. A `decline to conclude` here is the honest outcome and costs only a
thermometer to convert later.

**Free consistency check, and what it is not.** Back-solve `c` from the two
taped positions: `c = 2.000 m / Δt`, then `T = (c − 331.3) / 0.606`. If that
lands inside 25–27 °C, the tape and the estimator corroborate each other; if it
lands outside 20–30 °C, something is wrong with one of them and that is a
finding worth the capture. **It cannot rescue AC7** — deriving `c` from the tape
and then checking the tape-derived metres against the tape is circular. It is a
sanity check on the estimator, nothing more.

**Cheapest fix if one is to hand:** a thermostat or HVAC display reading in the
same room converts the AC7 `decline` into a verdict. A phone weather app does
not — that is outdoor air, not the air between the baffle and the capsule.

### Position 1 — capsule at taped 1.000 m, on axis

Both builds, before the mic moves.

**B1 — #346 AC5 (PR #352, `issue-346`).** The onset estimator's arrival at this
position. Capture the full IR stats, per frame, including whatever field
records which rule produced the arrival.

**B2 — #356 AC7 (PR #356, `issue-243`).** The gated metres readout with a
stored per-pair distance calibration. Capture the readout and the underlying
`delay_evidence`.

**Ride-along, no extra rig time — #251.** 20 s `transfer_probe` captures with
the full uncapped `delay_evidence.candidates` at each position. **Check first
whether this is already answered:** `audit/rig-243-2026-08-18/` holds seven
captures with uncapped candidates at both 1.000 m and 3.000 m. If those cover
it, #251 is offline scoring, not rig time — say so in the record and don't
recapture.

**Ride-along — the converter constant.** `pairs=[[2,2],[0,2]]` if the reference
is `capture_3`. Zero extra cost, same call, and it re-measures
`arrival(d) = 1.1931 ms + d/c` under this session's actual temperature.

### Position 2 — capsule at taped 3.000 m, on axis

Move the mic once. Re-read the tape and the thermometer. Repeat B1 and B2 with
both builds.

> **B1 pass (#346 AC5):** ~~the onset estimator's increment between the two
> taped positions matches the taped 2.000 m at least as well as
> `transfer_stream`'s 4.7 mm~~ — superseded 2026-08-24 (#375), the 4.7 mm
> figure was a single tape draw, not a demonstrated capability; see the
> re-derived, tape-free bar above (`|Δt_est − Δt_transfer_stream| ≤ 3 ×
> se(Δt_est)`). For scale: the IR *peak* managed 4.5 samples (1.6 cm) on the
> increment while being 145–151 samples wrong in the absolute — the increment
> is forgiving, which is exactly why it is the criterion, and also why a
> failure here is unambiguous.
>
> **B1 falsifying outcome:** the increment is worse than 4.7 mm, or the
> estimator returns the peak (145–151 samples late in the absolute at these
> positions). The second would mean the guard-band coupling in #353 is biting
> on real data, which would be a significant finding — capture the IR, not just
> the derived number.
>
> **B2 pass (#356 AC7):** the corrected metres readout agrees with the taped
> 2.000 m increment **within 5 mm**.
>
> **What neither of these fixes, so nobody reads a correct result as a
> failure:** roughly 1.07 ms — about 37 cm — of phantom distance survives in
> the *absolute* readout. It was never conversion latency; #343 attributed it
> to the loudspeaker's own acoustic group delay, a property of this DUT. The
> metres readout will still over-read at a taped distance. Only the increment
> is being scored.

**Ride-along — `CHECK ROUTING` post-lock, the one path never exercised on
hardware.** Wants the mic where it already is, so it costs no position change.
Order matters and is what makes it a test:

1. Lock at the position and **confirm the ladder has settled.** A pair that has
   not locked reproduces the pre-lock case session 3 already proved
   unreachable, and the run tells you nothing.
2. **Start capturing before blocking anything.** The informative frames are the
   ones where `mtw` is still present *and* coherence has collapsed.
3. **Block the mic capsule by hand with the drive still running**, and keep
   capturing through and past the transition.

> **Pass:** a capture showing `mtw` present, `coherence_dead` true, and whatever
> the banner did. Either outcome is worth having — the banner firing exercises
> the path for the first time; the banner staying dark with `mtw` present and
> coherence collapsed is a defect with its evidence already attached. **File on
> the capture, not on the recollection.**
>
> **Do not let the drive stop.** Drive off with both legs at the floor is a
> deliberate `None` and paints a blank pane that looks like the same symptom
> while testing nothing.

**Confounds for the whole track:** mic SNR here is 9.69 dB at preamp 36 and is
limited by room noise at the capsule, not by preamp or converter — 15 units of
gain bought 1.3 dB on 2026-08-18. Do not reach for gain if the numbers look
thin; record the room instead. Also record a silent baseline at each position,
contemporaneously, since the room floor moved ~10 dB across one evening in a
previous session.

---

## Track C — free blocks. No mic, no emission, outside the acoustic budget.

- **`install.sh` / `Text file busy`** — already folded into P, above.
- **Snapshot references.** #337 is closed and the five references were
  regenerated on this box on 2026-08-20 at `issue-243` `e0e9341`. Nothing to do
  *now*. What is worth one minute: after #356 merges, run
  `cargo test -p ac-view --test it_transfer_snapshots -- --ignored
  --test-threads=1` **without** `UPDATE_SNAPSHOTS` and confirm green, since the
  committed references were generated on a branch.

---

## What this session does not cover, and why

- **#357** (delay rows colliding with the trace) carries `requires-rig`, but not
  yet. It needs a layout decision from the architect first; the rig's role is
  re-generating the snapshot on the real adapter *after* a fix exists. QA
  reached the same conclusion independently. No block here.
- **#351** (τ still on `argmax`) needs no rig at all for its main check — the
  zero-path case is `--fake-audio`, where true distance is exactly 0. It is
  blocked on #352 merging, which is what Track B unblocks.
- **#353** (guard band vs. lobe width) has one rig-shaped question: whether
  real gate windows are ever narrow enough for the fixed guard to bite. B1's
  captured IRs answer it as a by-product — **keep the raw IRs**, not just the
  derived arrivals.
- **Run D** (#208's positive control, 50 ms gated burst) is dropped a third
  time, deliberately. #208 is **closed**, so its positive control no longer
  gates anything; what survived was its role as the control for an onset guard
  that was never shipped. Recording the decision rather than deferring it
  again: if the guard is ever revived, Run D comes back with it.
- **Absolute distance accuracy.** Only the increment is scored. The constant
  term is derived from these same measurements and is not an independent check.

---

## Records to write

**Two files, not one.** Track A changes the JACK period, which changes the
configuration the numbers were taken under; merging it with Track B would make
one set read as if measured under the other's conditions.

- `work/rig/rig-2026-08-22-tau-loopback-results.md` — P, A1, A2, A3.
- `work/rig/rig-2026-08-22-onset-distance-results.md` — Track B.

Each carries, per `.agents/rig.md`: build under test (sha256, git ref), drive
level and its authorization, what is physically connected, clock state and why,
per-run result stated as pass / fail / decline-to-conclude, **confound on every
run**, rig state left behind, and what should happen next ordered by what blocks
what.

Then mark the executed blocks in `rig-verify-queue.md` with a pointer to the
record that ran them.

---

## Rough budget

| block | time |
|---|---|
| P — builds, hashes, wiring, clock, `jack_iodelay`, thermometer | 30–40 min (builds run unattended) |
| A1 — loopback leg | 10 min |
| mic setup, tape, settle | 30 min |
| B — two positions × two builds, plus ride-alongs | 60–75 min |
| A2/A3 — period ladder and ten calibrations | 60–90 min |
| C — post-merge snapshot check | 5 min |

Call it 3.5–4 hours with the acoustic half protected. If time runs short, A2's
period ladder is the block to shorten (drop period 256, keep 512/1024/2048 —
the accept/refuse boundary is the part that matters), **not** Track B.
