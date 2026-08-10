# Report: spectrum HF garbage — multiband repro + unit audit (follow-up)

**status:** both P1a and P1b resolved with exact numeric matches. Root cause
of the "9 dB gap" the prior report couldn't close: found, and it's a second,
independent bug in a file the prior report never looked at.
**NO PATCH APPLIED** — per handoff.md's hard fence, evidence only.

## Task 1 — multiband repro

Harness: `ac-core/tests/tmp_multiband_dump.rs` (temporary, deleted after
capture — zero production code modified). Drove the real
`ac_core::visualize::spectrum::spectrum_only` and
`ac_core::visualize::aggregate::spectrum_to_columns_multiband_wire` with the
field config (sr=96000, LF N=65536, HF N=8192, crossover=750 Hz, f_min=20,
f_max=48000, 4096 columns), both legs fed **silence** (all-zero samples) to
isolate the bin-count mechanism from real signal content, matching how
`ch4`–`ch9` behave in the field CSVs (those channels are unconnected/silent
at every frequency — confirmed by grep, they read -240 dBFS from 20 Hz up
regardless of capture). A read-only mirror of `spectrum_to_columns`'s inner
loop reports `(src_start, src_end, n_src_bins, accumulated_power)` per band
per column; the real `_multiband_wire` fn supplies the merged output.

**Critical addition over the prior report**: the prior report examined
`aggregate.rs`'s output directly and stopped there. It never followed the
wire frame into `ac-ui`. I did, and that's where the missing ~9 dB was
hiding (see Task 2). The dump below applies that same downstream step
(`20·log10(v.max(1e-12))`, copied verbatim from
`ac-ui/src/data/receiver.rs:492-494`) to the aggregator's raw merged value
before comparing to the field CSV, because that's what the field CSV
actually contains.

### P1a — dual-FFT-leg accumulation: **falsified**

```
col=4048 f=43857.312 hf_k=3739 hf_n=8 hf_power=8.000000 hf_dbfs=9.031 merged_pre_ui=9.030900 ui_final=19.115
col=3981 f=38614.505 hf_k=3292 hf_n=7 hf_power=7.000000 hf_dbfs=8.451 merged_pre_ui=8.450980 ui_final=18.538
```

At every column that reproduces 18.538 or 19.115, `merged_pre_ui` equals
`hf_dbfs` exactly (the HF-only branch) — these frequencies (37.8–48 kHz)
are far above the ±1/6-octave blend region around the 750 Hz crossover
(`lo≈668 Hz, hi≈841 Hz`), so **no LF-leg contribution reaches these
columns at all.** The mechanism is single-leg (HF), `n_src_bins` growing
toward the Nyquist edge exactly as the prior single-band repro found
(ceiling `10·log10(8) = 9.031` at `n=8`) — the multiband stitch is a
complete red herring for this specific symptom. **P1a is falsified**: it
is not "some column accumulates bins from both FFT legs"; it's the
already-known single-leg mechanism plus a second conversion (below).

`n_src_bins=7` → `9.030900`... wait, `n_src_bins=7` → `power_sum=7.0` →
`10·log10(7)=8.451`; `n_src_bins=8` → `10·log10(8)=9.031`. Both exact
matches to the field's 18.538/19.115 once you apply
`20·log10(8.451)=18.538` and `20·log10(9.031)=19.115` — bit-exact.

### P1b — empty-column −240 interleave: **confirmed, and it's this aggregator, not a downstream clamp**

Synthetic dump: first `−240`-producing column at **500.508 Hz**, last at
**12046.867 Hz** — zero occurrences above that in 2402 sampled columns.
Field data (`ch4`, silent channel, `spectrum_20260704T133115Z.csv`):
zero `−240.000` occurrences above 12047 Hz (checked directly), matching
the synthetic cutoff to the Hz.

Mechanism: near the interpolation→aggregation crossover (~6533 Hz for the
HF leg, per the prior report — the *within-band* count threshold, not the
750 Hz LF/HF splice), `n_src_bins` for a given column doesn't cross from 0
to ≥1 cleanly; it jitters 0/1/0/1 for a stretch before settling ≥2
permanently. For pure silence:
- `n_src_bins == 0` (interpolation, both neighbours silent): output is a
  literal passthrough of a `0.0` neighbour value.
- `n_src_bins == 1` (aggregation branch, exactly one bin): `power_to_db(db_to_power(0.0)) == 0.0` **exactly** (this is not approximate — `power_to_db`
  and `db_to_power` are exact algebraic inverses for a single term).

Both cases hand the wire frame a literal `0.0`. `ac-ui/src/data/receiver.rs:493`
then computes `20.0 * v.max(1e-12).log10()`, and `20·log10(1e-12) = −240.000`
exactly. **This is the −240 clamp** — not a CSV writer default (the CSV
writer's own missing-value fallback is `-140.0`, `ac-ui/src/ui/export.rs:127`,
never hit here since these are real data cells) and not
`amplitude_to_dbfs`'s `MIN_DBFS` (`-200.0`, never `-240` — checked, it
doesn't match). Only once `n_src_bins` is reliably ≥2 does the sum exceed
1.0 and the bin-count creep take over for good.

## Task 2 — unit table

| stage | file:line | unit |
|---|---|---|
| (a) interpolation branch, `count==0` | `aggregate.rs:86-103` | Input `spectrum_db[..]` is **linear amplitude** (mislabeled). Snap cases (`c<=f_below`/`c>=f_above`) pass it through completely unconverted. The 2-point blend case runs it through `db_to_power`/`power_to_db`, but because linear-amplitude values are tiny (≪1), that round-trip is a first-order identity (`power_to_db(db_to_power(x)) ≈ x` for small `x`, both algebraically for a single term and via Taylor expansion for the weighted 2-term blend, since interpolation weights `t, 1-t` sum to 1 — unlike the aggregation branch's unnormalized sum). **Net effect: still linear amplitude**, approximately unchanged, all the way to the wire. |
| (b) aggregation branch, `count>=1` | `aggregate.rs:74-84` | `count==1`: **exact identity**, `power_to_db(db_to_power(x)) == x` algebraically — still linear amplitude. `count>=2`: **neither unit** — `Σ db_to_power(tiny x) ≈ n_src_bins` (each term ≈1, real signal content erased), so output is `10·log10(n_src_bins)`, a pure bin-count artifact with no dB meaning at all. |
| (c) multiband stitch (`spectrum_to_columns_multiband`) | `aggregate.rs:284-296` | Linearly blends whatever (a) or (b) produced for each leg (`lf[i]*(1-t) + hf[i]*t`) with no unit awareness. Confirmed irrelevant to the 18.538/19.115 symptom (P1a) since those columns sit outside the blend window and the blend is 100% one leg — but it is a latent second bug: if one leg is mid-(a) (≈linear amplitude, ~1e-5) and the other mid-(b)-count≥2 (≈dB-looking, e.g. `4.77`), the blend adds two incompatible quantities. Not chased further per handoff scope. |
| (d) CSV serialization + `−240` clamp | **`ac-ui/src/data/receiver.rs:492-494`**, *not* `ac-core`/`ac-daemon` at all | This is the actual, and only, place a genuine `20·log10` dB conversion happens on this path. Comment at the site: *"Daemon publishes a linear amplitude spectrum... match `ac/ui/spectrum.py:131`"* — i.e. it was written assuming the wire `spectrum` field is always linear amplitude, which is **true for (a) and count==1 of (b)**, but **false for count≥2 of (b)**, where the value is already a (meaningless) dB-domain number and gets log10'd a second time. `v.max(1e-12)` is where `−240.000` is produced (`20·log10(1e-12) = −240` exactly). The CSV writer itself (`ac-ui/src/ui/export.rs:127`) only ever sees post-conversion values and has an unrelated `-140.0` fallback for genuinely missing bins. |
| (e) transfer path (`render_pipeline.rs:199-201`, feeding `tf.magnitude_db`) | `ac-core/src/visualize/transfer.rs:245` computes `magnitude_db[k] = 20.0 * mag.log10()` | **Already correct dB**, confirmed by reading the transfer measurement source directly. `samples_on_axis_to_columns` therefore receives real dB in and emits real dB out — no mislabeling. Critically, `render_pipeline.rs` builds the transfer `DisplayFrame` directly from the already-parsed `TransferFrame` struct; it **never round-trips through `receiver.rs`'s wire-JSON `spectrum` parsing**, so it never hits the blind `20·log10` at (d) either. The transfer path is unaffected by either bug. |

## The double-conversion question — answered

**Converting at the `monitor.rs` call sites (prior report's Option 1) would not double-convert anything inside `ac-core`/`ac-daemon`** — the transfer path is a structurally separate function and call site, confirmed clean in (e).

**But it would create a *new* double-conversion in `ac-ui`.** `ac-ui/src/data/receiver.rs:492-494` unconditionally applies `20·log10(v.max(1e-12))` to every `monitor_spectrum` wire frame's `spectrum` field, on the explicit (and, until now, accurate-by-coincidence) assumption that it is always linear amplitude. If `monitor.rs` is fixed to hand `spectrum_to_columns_wire`/`_multiband_wire` genuine dB (Option 1), the aggregator's math becomes correct and its output becomes genuine dB too — and `receiver.rs` will then take the log of an already-logged value a second time, corrupting every frame the fix was meant to repair. The same is true of Option 2 (push the conversion into `aggregate.rs`): whatever unit the aggregator settles on internally, its *output* unit doesn't change, so `receiver.rs` still needs to stop assuming linear amplitude. **This is the second bug the handoff's opening paragraph anticipated** ("makes the obvious fix... a probable regression") — it lives outside `ac-core` and `ac-daemon` entirely, in `ac-ui`, and neither prior-report fix option mentions it because neither followed the wire frame that far.

One incidental note per the handoff's fencing (not chased further): this also plausibly explains "meters-vs-spectrum disagreement" — `fundamental_dbfs`/loudness meters go through `amplitude_to_dbfs` (a single, correct conversion), while the spectrum display goes through this two-stage, only-sometimes-correct path. Flagging only; not investigated.

## Acceptance criteria status

- [x] Multiband dump reproduces 18.538 AND 19.115 exactly, with per-band bin counts (`hf_n=7` and `hf_n=8` respectively; LF contributes nothing at these frequencies)
- [x] `−240` interleave mechanism identified: `aggregate.rs`'s own `count∈{0,1}` exact-zero collapse near the HF within-band crossover (~6533 Hz), consumed by `ac-ui/receiver.rs`'s `.max(1e-12)` floor — confirmed against field data (last occurrence 12047 Hz, both synthetic and field)
- [x] Unit table complete for (a)–(e), all file:line
- [x] Double-conversion question answered: no regression within ac-core/ac-daemon, but a **new** double-conversion risk in `ac-ui/src/data/receiver.rs` that neither prior fix option addresses
- [x] Zero production code modified; harness (`ac-core/tests/tmp_multiband_dump.rs`) deleted after capture
