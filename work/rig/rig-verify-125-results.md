# rig-verify-125-results — 2026-08-03/04, 192.168.9.25

Executes `$AC_HOME/handoff/handoff-rig-verify-125.md` at the fixed 3 m on-axis position, and
the parts of `$AC_HOME/rig-verify-queue.md` that need no microphone movement.

**Build under test: `main` @ `447f417`** — not a branch. PR #237
("fix(transfer): causal-only delay search, and captures that reproduce their
own decision") merged at 2026-08-03T22:40:32Z, so `rig2-fixes-125` is now
`main`. Built on the rig into `~/target-rig2`, installed, verified by sha256.

**Drive level: −30 dBFS nominal pink**, authorised by the operator for this
session against the standing −40 dBFS ceiling, same as session 2. Clamp
enforced server-side by `drive_max_dbfs: -30.0` under `HOME=~/rig2-home`.
Emission stopped between every run.

Session 2's numbers are in a **different prominence definition** and are not
merged into these tables. Where they appear it is as context, marked as such.

---

## Pre-flight

| check | result |
|---|---|
| clock `numid=320` | `0` = AutoSync — left alone, as required |
| mic preamp `numid=301` | 36, the session-2 baseline |
| loopback | `playback_2` (AN2) → `capture_4` (IN4), confirmed live |
| session config | `reference_channel: 3`, `reference_output_channel: 1` |
| sample rate | 96 kHz |

### Installed binaries — the stale-binary trap fired again

The queue says verify by sha256, not size and mtime. It was right, and the
failure mode was new: `sudo cp` of all three binaries **partially succeeded**.
`ac` and `ac-view` were replaced; `ac-daemon` failed with `Text file busy`
because the running daemon held it, and `cp` reported that one error while the
other two went through. A check of "did the install command run" would have
passed. Only the hash caught it.

Final state, all three matching a fresh build of `447f417`:

```
9741d70f8a1154c963f7acb11be9f19ed05bb68685f03a9077340821d279f1af  ac
59d2550cfa1f6fb3b03aec45468908c0b576b1e66177a4a0aa67d20705a1d657  ac-daemon
c60bcc82a814944c71ffc2125bacef8d56696dca12e5e6025f8c91511c5bbb79  ac-view
```

### Silent baselines — the room did not move this time

| | mic `meas_peak_dbfs` | ref `ref_peak_dbfs` |
|---|---|---|
| before, median | **−47.12** | −94.78 |
| after, median | **−47.02** | −94.72 |

**0.10 dB of drift across the whole session.** Session 2's evening moved ~10 dB
and had to caveat every level comparison; this one does not. Both baselines are
390 frames, no emission, 0 locks.

The room is ~5 dB quieter than session 2's opening baseline (−41.45).

### One protocol note for the next session

The PUB wire topic is **`data`** for every measurement frame. The message name
lives in the payload's `type`. Subscribing to `transfer_stream` matches nothing
and yields a zero-frame capture indistinguishable from a dead session. Session
2's client was not preserved; this cost a debugging cycle to rediscover.

---

## Run 1 — the wrong-lock fix: no wrong locks, and no locks

Eight fresh sessions, one per attempt, stimulus on a standalone `generate_pink`
worker on channels `[1, 4]` started **before** each session.

| session | frames | locked | `peak_lag` | | prominence median | mic |
|---|---|---|---|---|---|---|
| 1 | 259 | 0 | 1045 | 10.885 ms | 16.30 | −41.7 |
| 2 | 259 | 0 | 1045 | 10.885 ms | 15.09 | −42.0 |
| 3 | 259 | 0 | 1045 | 10.885 ms | 15.43 | −41.9 |
| 4 | 258 | 0 | 946 | 9.854 ms | 15.84 | −41.9 |
| 5 | 259 | 0 | 1045 | 10.885 ms | 15.17 | −42.0 |
| 6 | 258 | 0 | 1045 | 10.885 ms | 14.77 | −41.9 |
| 7 | 258 | 0 | 1045 | 10.885 ms | 17.03 | −42.0 |
| 8 | 259 | 0 | 1045 | 10.885 ms | 14.74 | −42.1 |

**0 locks in 8 sessions. No wrong locks, because no locks.** Prominence
14.74–17.03 against a gate of `MIN_PROMINENCE = 12.0 / 0.5 = 24.0`.

The refusals are not the interesting part. **`peak_lag` is 1045 samples in
seven of eight sessions, to the sample, in every one of 2069 frames.** The
estimator locates the arrival identically every time and the gate discards it.

### The 36-sample question, resolved

1045 samples is 10.885 ms; session 2's position-3 arrival was 1081 (11.26 ms).
That is not a geometry change. The direct arrival is a **broad correlation
peak**, and session 2's value sits inside it:

| lag | time | value | rel. peak |
|---|---|---|---|
| 1045 | 10.885 ms | 0.19122 | 0.00 dB |
| 1053 | 10.969 ms | 0.18935 | −0.09 dB |
| 1061 | 11.052 ms | 0.18654 | −0.22 dB |
| 1069 | 11.135 ms | 0.18317 | −0.37 dB |
| **1080** | **11.250 ms** | 0.17736 | **−0.65 dB** |
| 1114 | 11.604 ms | 0.17794 | −0.63 dB |

The whole cluster spans 0.65 dB. Which sample wins is decided by which peak the
6 dB window is measured against, and this branch deliberately changed that from
the global maximum to the strongest **causal** peak. The mic did not move.

---

## The onset case — the one thing the queue said needs the rig

Reversed ordering, deliberately: `transfer_stream` started first, stimulus
6 s later, so the correlation ring straddles the silence→signal transition.
This is the exact condition that produced session 2's **−826.35 ms** lock.
Four runs.

**Result: pass, and it produced the session's only lock.**

| | frames | locked | negative locks |
|---|---|---|---|
| silent phase | 360 | 0 | 0 |
| driven phase | 1500 | 370 | **0** |

- **Zero negative `peak_lag` in any frame, silent or driven.** The causal-only
  search never selects one. Session 2's failure is structurally out of reach.
- During silence the peak wanders over large positive lags (155–721 ms) at
  prominence 4.8–5.7 — far under the gate, so nothing locks. Correct refusal,
  not luck.
- In the silent phase the **non-causal peak beats the causal one** in 52–76 of
  90 frames per run. That is the −826 ms condition, present and published, and
  now not selectable.
- On stimulus onset the peak snaps to the arrival within **~0.35 s** and stays.

Run 4's lock, all 370 frames identical:

```
delay_ms 10.438   delay_samples 1002   peak_lag 1045   prominence 30.93
```

First lock landed at t+0.34 s after onset, at a plausible positive lag. That is
the queue's pass condition, met.

### Against the handoff's stated criterion — read this carefully

The handoff asks: *"every lock that occurs is at 11.34 ms ± a sample or two"*.
**10.438 ms is 86 samples early, so the criterion as written is not met.**

The failure criterion is also not met: 10.438 ms is not 14.00 ms, not 18.43 ms,
and not inconsistent with 3 m. The accepted lag 1002 sits on the leading edge
of the same correlation cluster tabulated above, and `peak_lag` in that frame
was 1045 — the same value as every other session this evening. #227's
earliest-peak rule moved the estimate 43 samples earlier within its 6 dB
window, which is the rule working as designed.

A ±2 sample tolerance was written from session 2's within-build repeatability.
It does not survive a change that deliberately alters which peak the window is
measured against. **The lock is physically right; the tolerance is too tight.**
That is a judgement call and it belongs to the operator.

---

## Run 2 — evidence completeness: pass, on both halves

The property session 2 failed in **every** position-3 session.

**Locked frames — 370 of 370:**

| check | failures |
|---|---|
| accepted lag (`delay_samples`) present in own `candidates` | **0** |
| `peak_lag` present, exactly once | 0 |
| `noncausal_peak_lag` present, exactly once | 0 |
| ≤ 35 entries | 0 |
| lags unique and strictly ascending | 0 |

Example locked frame: accepted lag 1002 at index 1 of 33, candidate span
−233…3327 (−2.43…34.66 ms).

**All frames — 2069 across Run 1's eight sessions:** same checks, zero
failures, candidate count 33 throughout.

Session 2's truncation defect is refuted directly. There, the arrival at 1081
fell outside a candidate span of 1815–3335 because 32 reverberant peaks
outranked it. Here `peak_lag` is **rank 1** in every session and the span
reaches from deep negative lags to ~34 ms.

**Offline threshold tuning is unblocked.**

---

## Run 3 — the two numbers, and an answer nobody expected

### Prominence, new definition, at 3 m — a fresh baseline

Measured against the strongest causal peak. **Not comparable to session 2's
13.6–27.8.**

| | min | median | max |
|---|---|---|---|
| Run 1, eight sessions | 14.74 | 15.30 | 17.03 |
| onset runs 1–3 (refusing) | 14.04 | 14.89 | 15.38 |
| onset run 4 (locking) | — | 30.93 | — |

Eleven of twelve sessions sit at **14–17 against a gate of 24**. Session 2 put
position 3's median at 21.8 and called it "sitting *on* the gate, a coin toss".
In the honest causal definition it is not a coin toss — it is a clear refusal,
and the ~7 point drop is the non-causal energy that used to inflate the
numerator being removed.

**The gate refuses a stable, correct, physically plausible arrival at 3 m.**
Session 2 reached that conclusion at 1 m; it now holds at 3 m too.

### `negative_lag_median` — the proposal is not closed, and it looks good

Across the eleven refusing sessions the two floors agree to within 7%
(`median_value / negative_lag_median` = 0.993–1.071, median 1.024), which on
its own reads as "the negative-lag floor changes nothing".

**That conclusion is wrong, and the one locking session is why.**

| onset run | `peak_value` | `median_value` | `negative_lag_median` | prom, all-lag | prom, neg-lag | locked |
|---|---|---|---|---|---|---|
| 1 | 0.18528 | 0.01196 | 0.01161 | 15.38 | 15.96 | no |
| 2 | 0.17601 | 0.01274 | 0.01195 | 14.04 | 14.73 | no |
| 3 | 0.18915 | 0.01278 | 0.01236 | 14.89 | 15.30 | no |
| 4 | **0.09230** | **0.00298** | 0.01049 | **30.93** | **8.80** | **yes** |

Run 4 has the **weakest** arrival of the four — `peak_value` roughly half the
others. It locked anyway, because `median_value` collapsed by 4× while
`negative_lag_median` barely moved.

So:

- The **all-lag** statistic scored the weakest arrival as twice as prominent as
  the strongest ones. That is a false-confidence spike, and it is the only
  thing that admitted this lock.
- The **negative-lag** statistic ranked run 4 worst (8.80 against ~15), which
  is the correct ordering by arrival strength.

This is exactly the contamination finding 4 was written about, caught in the
act: the all-lag median is taken over lags that hold reverberation, so when the
reverberant field changes the floor moves under the arrival. The negative-lag
floor, measured where nothing physical can arrive, did not move.

**One position cannot settle this and I am not claiming it does.** But the
proposal is no longer merely plausible — there is now a recorded case where the
two disagree, the disagreement decides a lock, and the negative-lag floor is
the one that ranks the evidence correctly. The queue asked whether a capture
set could answer "no". This one answers "not no".

The lock itself is sound — lag 1002 is on the same cluster as all eleven other
sessions. What is not sound is the number that let it through.

---

## Run 4 — the fault indicator: `CHECK ROUTING` is still unreachable, for a new reason

Scored headlessly against `ac-scene/src/fault.rs`
(`COHERENCE_THRESHOLD = 0.5`, `COHERENCE_ALIVE_FRACTION = 0.10`,
`settled = mtw.is_some()`, `refusing = settled && !delay_locked`).

| condition | frames | with `mtw` | locked | refusing | `CHECK ROUTING` |
|---|---|---|---|---|---|
| A. unrelated legs (ref = pink, meas = room) | 352 | **0** | 0 | **0** | cannot fire |
| B. both legs from one pink source | 352 | **0** | 0 | **0** | cannot fire |

**The ladder is only built once a lock exists.** No lock → no `mtw` → the
coherence slice is empty → `coherence_dead()` returns `false` by its own guard
→ `CHECK ROUTING` cannot fire. And `refusing` requires `settled`, which
requires `mtw`, so `LOST LOCK` / `NO LOCK` cannot fire either.

Fix 5 moved the threshold from all-columns to <10% alive, and the new number is
right: session 2 measured 4.4% alive on genuinely unrelated legs, which clears
10% comfortably. **But the frame that would carry those columns is never
published.** The state went from unreachable-because-too-strict to
unreachable-because-there-is-no-data.

`fault.rs` anticipates the precedence — *"a refusal outranks
`Fault::CheckRouting`: with #227 present, unrelated sources make the estimator
refuse, and the flag is the more direct statement of it"* — but the refusal
flag is itself unreachable (finding 3, untouched by this branch). Unrelated
legs render a blank window with no indicator, exactly as session 2 recorded.

**Condition B is not a valid healthy control.** It refused too, at this
position's base rate, so "a healthy measurement never shows `CHECK ROUTING`"
remains unconfirmed rather than confirmed.

Fixing this needs the ladder decoupled from the lock, or the refusal flag made
reachable. It is not a threshold problem.

---

## Not run

- **Run D / criterion 10.** Not attempted. It requires an A/B against
  `cda40ef`, and the criterion is explicit that a one-sided run proves nothing
  ("one episode on the new build only means something if the old build shows
  four"). Two further obstacles found while scoping it: the daemon has **no
  burst or gating primitive** — `set_drive on`/`off` over ZMQ cannot approach
  the 50 ms the queue describes — and the recurrence lives in a displayed
  response that at this position mostly does not exist, because sessions
  refuse. Gap unchanged.
- **Run C positions 1, 2, 4, 5** — blocked on the fixed microphone.
- **1 m and 3 m back-to-back.** Blocked on the same. Note the confound it was
  meant to resolve is weaker than feared: the room floor was stable to 0.10 dB
  tonight, so session 2's 1/12-vs-7/7 inversion is less likely to be drift.
- **Finding 4 remains open.**

---

## Verdict

| fix | verdict |
|---|---|
| 1 — no negative lock | **pass.** 0 negative locks and 0 negative `peak_lag` in 5789 frames, including 360 frames deliberately straddling a stimulus onset. |
| 2 — captures reproduce their own decision | **pass**, on all 370 locked frames and all 2069 refusing ones. Offline tuning unblocked. |
| 5 — `CHECK ROUTING` fires | **cannot be verified.** Threshold is correct; the frame that would trigger it is never published. |

The branch does what it claims. The gate in front of it does not, and this
session says so at 3 m with a stable room and a stable arrival, which session 2
could not.

---

## Rig state left behind

- Clock `AutoSync` (`numid=320 = 0`), untouched.
- Mic preamp `numid=301` at 36, 48 V on, PAD off. No mixer route written.
- **No emission in progress.** All workers stopped.
- `/usr/local/bin/{ac,ac-daemon,ac-view}` match a build of `447f417` by sha256
  (hashes above). Build dir `~/target-rig2`.
- Daemon running under `HOME=/home/mui/rig2-home`, `drive_max_dbfs: -30.0`,
  log at `~/rig2-home/daemon.log`.
- `~/ac` fast-forwarded from `7f0dd5e` to `447f417`, clean.
- **`~/.config/ac/config.json` still has `reference_channel: 2`** pointing at
  the silent `capture_3`. Untouched deliberately — it is the operator's file.
  Anything run as the operator's own user will still report `NO REFERENCE`.
- Captures in `audit/rig-verify-125/` (untracked, 2.1 MB): `run1`, `onset`,
  `run4`, `baseline_before`, `baseline_after` as `*-evidence.pkl.gz`, with the
  client `rig.py` and the run/analysis scripts beside them.

  These are **slimmed**: `delay_evidence` in full (every candidate lag and
  value), plus the scalar delay/level fields and the `mtw` coherence columns.
  The per-frame magnitude, phase, re/im and spectra were dropped — 657 MB of
  raw frames for 2.1 MB of the evidence that fix 2 exists to preserve. Offline
  tuning of `DIRECT_PEAK_FRACTION` and the two floors needs only what is kept;
  anything wanting the transfer curves must re-run.
