# rig-2026-08-22-tau-loopback-results — 192.168.9.25

Track A of `work/rig/rig-test-plan.md`. Electrical only — AN2 → IN4 cable, no
microphone, nothing into the room.

**Operator:** Markus, on site. **Session start:** 2026-08-22 ~20:50 EEST.

**Temperature:** not measured — no thermometer available. Operator states
25–27 °C. Irrelevant to this file (electrical only); it bounds the acoustic
record, not this one.

## Build under test

Built **on the development VM**, not on the rig — see "Host" below. Copied to
`~/rig-2026-08-22/bin/` and hashed *after* the copy.

| binary | sha256 | ref |
|---|---|---|
| `it_loopback_ir-issue-341` | `1febe37e81267a8368f15033691853bbc5e4eb72c86b94d99b5fe73ef95112aa` | `ca41897` (PR #355) |
| `ac-daemon` (spawned by the test) | `aac2ff0bd8f211a69731210a965d6068ccc5382c61bb7dd56b258a72ee99d712` | `ca41897` |
| `ac-daemon-main` | `ee309d89cb5ab3d9e9a15599266dca0eb1dde2d9285a001177eb0d233a24240b` | `0ef2c81` |
| `ac-main` | `6ecc86f2e213db6d63b705de60fc07f2fa6186ae851e82afd111a93c165b95c7` | `0ef2c81` |

The test locates its daemon through `env!("CARGO_BIN_EXE_ac-daemon")`, an
absolute path baked in at compile time. `$HOME` is `/home/mui` on both machines,
so the build path was replicated on the rig
(`/home/mui/work/rig-2026-08-22/target-issue-341/release/ac-daemon`) rather than
rebuilding anything here.

## Host — and an incident that must not repeat

**192.168.9.25 is the hypervisor host the development VM runs on.** Earlier in
this session four `cargo build --release` runs were started *on it*, following a
line in the test plan that has since been corrected. At 20:56 the host's OOM
killer took a `qemu-system-x86` at 24 GB RSS — a running guest.

The audio stack survived (pipewire up, JACK still 96 kHz / 1024) and the host
did not reboot (31 days uptime). **That was luck, not margin.** A rig session
that loses JACK mid-block loses the client lifetimes that every absolute τ
reading is scoped to, and every number in this file would have been
uncomparable.

Build artifacts were removed from the host afterwards (`~/target-rig4*`,
2.1 GB); `~/target-rig2` and `~/target-rig3` from earlier sessions were left
alone. Nothing is compiled on this box now.

## Drive level

**−30 dBFS, authorised by the operator for this session**, obtained before the
first emitting run.

**The server-side clamp does not cover this path, and that is a finding, not a
footnote.** `drive_max_dbfs` is applied only in `set_drive`
(`handlers/transfer.rs:133`). `plot_ir` has no clamp at all, and `calibrate`
passes `ref_dbfs` straight through `dbfs_to_amplitude` into `measure_tau`
(`handlers/calibrate.rs:379`, `:490`) unclamped — with a **default of
−10.0 dBFS**, 30 dB above the standing cap. So on the runs in this file the
requested level is the only thing bounding what reaches the converter. The
rig interlock's "server-side clamp" condition **cannot be satisfied** on these
commands as the code stands. Recorded as a deviation, with operator consent, on
a cable — not a speaker. To be filed as an issue after the session.

## Wiring — confirmed this session

- `Babyface Pro Pro:playback_2` (AN2) → **physical cable** → `Babyface Pro
  Pro:capture_4` (IN4). Electrical loopback.
- Speaker and microphone legs present but **not exercised in this file**.
- Note: the loopback is a *cable*, so it does not appear in the JACK graph. A
  `jack_lsp -c` probe shows no connection between these ports and that is
  correct — it is not evidence the leg is dead.

## Clock

`AutoSync` (`numid=320` = 0), read this session. The external master clocks the
card over ADAT and ADAT carries the stimulus leg; `Internal` silently breaks the
speaker path rather than erroring.

## Configuration

96 kHz, period 1024, JACK via pipewire. Unchanged from #277 / #243 so the
numbers compare.

---

## Run A1a — `it_loopback_ir` at the default 0.5 s sweep — **FAIL, and informative**

**What was being verified:** the Babyface leg of `it_loopback_ir` (#341 /
PR #355), never executed on hardware before — QA hand-checked it against #277's
recorded numbers and said so plainly.

**What a pass looked like, stated before running:** test reports ok, peak near
12392–12404, SNR above the 25 dB floor.

**What happened:**

```
window_len:   5768 (ir len), frame window_len_used 5768
peak_index:   5709
peak_abs:     1.293534e-1
floor_abs:    7.934221e-3
snr_db:       24.25
peak_offset:  +2825 samples from centre (2884) = +29.4271 ms
```

Failed on SNR: 24.25 dB against a 25 dB floor. **Missed by 0.75 dB.**

**The SNR failure is not the finding.** The peak is at **+29.4271 ms**. This
leg's true round trip is **43.75 ms**, measured two independent ways by #277
(Farina peak and `jack_iodelay`, agreeing to 0.4 samples). At a 0.5 s sweep the
gap-clamped window reaches only `0.5 × ln2/ln(f2/f1) / 2` ≈ **30.0 ms** past
centre, so **the true arrival is outside the window entirely** and what was
reported is a pinned peak.

29.4271 ms is not a new number: #340 records #277 getting **29.427 and
29.510 ms** on the two rig legs whose true τ are 43.750 and 43.875 ms —
"reproducible to within 8 samples of each other, converging not because they
measure the same thing but because both are pinned against the same edge." This
run reproduces that to four decimal places.

**And the peak-position assertion passed on it.** `MAX_ROUND_TRIP_S = 60 ms`
never binds at this sweep duration, so `hi_bound` saturates at the window edge
and a peak 14.3 ms wrong is admitted. QA flagged exactly this on PR #355 as the
unguarded direction — "only the direction 'too small' is guarded here … a
threshold that's quietly too permissive." **This run is that prediction
demonstrated on hardware.** Had the SNR floor been set 1 dB lower, the test
would have reported `ok` on an arrival it could not see.

**Confound:** operator error, mine — `AC_LOOPBACK_DURATION_S` was left at the
0.5 s default when #277's runbook config is a 2.0 s sweep. That is what makes
the run informative rather than merely wrong: the misconfiguration is a
plausible one, and the test's response to it was a silently wrong peak caught
only by an SNR threshold that had 0.75 dB to spare.

## Run A1b — `it_loopback_ir` at the #277 runbook 2.0 s sweep — **PASS**

Same leg, same level, same session. Only the sweep duration changed.

```
window_len:   16384 (ir len), frame window_len_used 16384
peak_index:   12392
peak_abs:     9.338723e-1
floor_abs:    1.363341e-2
snr_db:       36.71
peak_offset:  +4200 samples from centre (8192) = +43.7500 ms
ok
```

**Pass, and an exact reproduction of the prior record:**

| quantity | this run | prior record |
|---|---|---|
| peak_index | 12392 | 12392 / 12404 (#341 issue body) |
| round trip | 43.7500 ms (4200 samples) | 43.75 ms / 4200 samples (#277) |
| SNR | 36.71 dB | 36.71 / 36.85 / 36.89 dB (#277) |
| `window_len_used[0]` | 16384 = `linear_ir.len()` | — (new assertion, #355) |

All three of PR #355's hardware-dependent claims hold, and the `window_len_used`
assertion — which was silently always `None` before #355 fixed the indexing —
now returns the window and matches.

**PR #355's one open rig item is discharged.**

**Confound:** none identified. Same client lifetime as A1a, no JACK restart
between them, so the two are directly comparable.

---

## Run A2a — first `ac calibrate` ever run on this rig — **τ measured, after a defect got in the way**

`ac calibrate`'s τ path had never been exercised here; the 2026-08-18 session
says so explicitly. Two attempts were needed, and the difference between them is
a defect.

**Attempt 1 — channels passed on the command line. Failed, misleadingly.**

```
ac calibrate -30dbfs output 1 input 3        (HOME=/home/mui/rig2-home)
  Input cal — measure ADC input Vrms with DMM (captured -105.8 dBFS)
  Calibration saved: [out1_in3]
  Delay:  not measured (loopback not detected this run)
```

−105.8 dBFS is digital silence. `ac devices` confirms index 1 → `playback_2` and
index 3 → `capture_4`, which are exactly the ports `it_loopback_ir` had just
driven successfully. Re-running A1b immediately afterwards reproduced
`peak_index 12392`, `+43.7500 ms`, `36.60 dB` — **the cable was live the whole
time.**

**Attempt 2 — same ports, given as sticky `output_port` / `input_port` in the
config instead. Worked:**

```
  Input cal — loopback detected (-33.6 dBFS captured)
  Delay:  43.7500 ms   (measured, 2 readings agree, 96000 Hz, period 1024)
```

−33.6 dBFS against the expected `ref_dbfs − 3.01` = −33.01, inside the ±2 dB
heuristic.

### τ result

**τ = 43.7500 ms, corroborated by two readings in separate client lifetimes.**
Identical to A1b's independently measured `+43.7500 ms` and to #277's 43.75 ms.
**#347's corroboration mechanism works on real hardware** — this is the first
time it has run outside a unit test.

### The defect — `calibrate` ignores `output_channel` for routing, then keys the stored calibration by it

`handlers/calibrate.rs:338-360`:

```rust
let out_ch = cmd.get("output_channel")...unwrap_or(cfg.output_channel as u64) as u32;
let in_ch  = cmd.get("input_channel") ...unwrap_or(cfg.input_channel  as u64) as u32;
let out_port = match resolve_output(&cfg, state) { ... };   // <- &cfg, not out_ch
let mut cfg_in = cfg.clone();
cfg_in.input_channel = in_ch;                                // <- input override applied
cfg_in.input_port = None;                                    // <- and sticky cleared
let in_port = match resolve_input(&cfg_in, state) { ... };
```

`out_ch` is used at **exactly one** place in the whole file —
`Calibration::load_or_new(out_ch, in_ch, None)` at line 503, the storage key.
It never reaches the routing.

Three consequences, in increasing order of seriousness:

1. **Silent misroute.** The request went to `playback_2`; the tone went to
   `cfg.output_channel = 4` → `playback_5` — **the loudspeaker**. At the
   consented −30 dBFS, so within policy, but not where it was asked to go.
2. **A fault that points at the wrong thing.** The operator-visible result is
   "loopback not detected this run", which reads as a wiring problem. It sent
   this session to re-verify a cable that was provably fine — the exact failure
   mode `AGENTS.md` warns about in "name what to check, not why".
3. **The stored calibration is labelled with a channel it did not measure.**
   The entry was saved as `[out1_in3]` while the output was physically
   `playback_5` (index 4). That is mislabeled measurement data, and unlike the
   other two it *survives the session*.

**Why nothing caught it:** `out_ch` is not unused — it feeds the key — so no
unused-variable warning fires and clippy sees nothing. The asymmetry is the
tell: input clears its sticky port so the index can win, output has no
equivalent, so a sticky `output_port` also silently overrides an explicit
request.

**Scope:** `calibrate_spl` and `calibrate_mic_curve` take `out_ch` the same way
and also use it only as a key, but neither routes an output, so they mislabel
without misrouting.

**Confound:** attempt 2 changed two things at once — sticky port names *and*
config channel values. The code above is what separates them; the routing path
reads `cfg` either way, so the sticky name is what made the difference. Stated
as a code-supported conclusion, not as an isolated-variable experiment.

## What Track A says should happen next

**Issues filed from this file's runs:** #358 (calibrate misroute), #360 (drive
clamp scope), #361 (pinned-peak bound). Track B filed #359.

1. **PR #355 can merge** on this evidence. Its `requires-rig` item is met;
   result posted to the PR.
2. **File the unguarded-direction finding** against #341/#355: the peak-position
   bound admits a pinned peak whenever the sweep is too short to hold the round
   trip, because `MAX_ROUND_TRIP_S` never binds at any runnable sweep duration.
   A1a is the hardware evidence. QA's suggested fix (raise
   `DEFAULT_DURATION_S`, or assert `hi_bound < ir.len() - 1`) is the right
   shape; A1a says the failure is reachable by ordinary operator error, not
   only by a regression.
3. **File the unclamped-drive finding**: `drive_max_dbfs` reads as a global
   ceiling but governs `set_drive` alone, while `calibrate` defaults to
   −10.0 dBFS on an unclamped path. Agreed with the operator to file after the
   session.
4. **A2 / A3 (τ ceiling, edge margin, corroboration) not yet run.** See the
   test plan. Note for whoever runs them: A1b establishes that this leg's τ is
   43.75 ms *today*, in this client lifetime — that is the external truth A2
   scores `measure_tau` against, and it agrees with #277 exactly.

## What the stored τ entry looks like, and one gap

`~/rig-2026-08-22/home-tau/.config/ac/cal.json` after the successful run:

```json
"tau_history": [{
  "conditions": { "device": 0, "backend": "jack", "sample_rate": 96000,
                  "period_size": 1024,
                  "output_port": "Babyface Pro Pro:playback_2",
                  "input_port":  "Babyface Pro Pro:capture_4" },
  "tau_s": 0.04375, "measured_at": "2026-08-22T18:46:53Z",
  "method": "farina_short_ess_v2", "agreement_count": 2
}]
```

#347's storage requirement is met exactly: the entry records **how many readings
agreed**, so a corroborated τ is distinguishable from a single-shot one.

**`ref_dbfs` reads −10.0 in the entry although the run was at −30 dBFS, and that
is correct, not a defect** — checked before recording it as one.
`calibrate.rs:506-510` only moves `ref_dbfs` when a voltage reading was actually
stored, and both DMM prompts were skipped (no DMM on this box). The field
describes the voltage legs, which were not measured.

**The gap:** `TauConditions` records device, backend, sample rate, period size
and both ports — but **not the drive level, and not the sweep parameters**. So
the level τ was measured at is recorded nowhere. #340's still-open acceptance
criterion already asks whether the sweep parameters should join the tuple; drive
level is the same question and belongs with it rather than in a new issue.

## Rig state left behind

**No emission in progress. No `ac-daemon` running** — all stopped at session
end. Clock `AutoSync` (`numid=320` = 0), verified after the last run. Loopback
cable `playback_2` → `capture_4` still patched. Speaker powered. Mic taped at
**3.000 m** on axis (moved from 1.000 m mid-session for Track B).

Binaries in `~/rig-2026-08-22/bin/`, hashes above. **No build tree on the host** —
`~/target-rig4*` removed; `~/target-rig2` and `~/target-rig3` from earlier
sessions untouched.

Two `cal.json` files were written this session:

1. `~/rig-2026-08-22/home-tau/.config/ac/cal.json` — the good τ entry above.
2. **`/home/mui/rig2-home/.config/ac/cal.json` — carries a mislabeled entry and
   is the operator's own file, so it was left alone.** The failed attempt wrote
   key `out1_in3` while physically driving `playback_5` (#358). It has no
   `tau_history`, so nothing downstream will read a wrong τ from it, but the
   entry claims a channel pair that was never measured. **Decide whether to
   delete it before the next session that uses `rig2-home`** — that is an
   operator call, not one this role makes.

One incidental: `ac-daemon` does not reject unknown flags. `ac-daemon-main
--help` started a server rather than printing usage; it was killed. Minor, but
it is the same shape as the `ac plot` surprise recorded in the Track B file.
