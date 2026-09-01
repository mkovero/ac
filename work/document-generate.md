# Generating a loudspeaker verification report

How the 2026-08-28 Genelec S30 unit #2 report was produced, written down so
unit #1 can get the same treatment after its driver swap and the two can be
read side by side.

**Expiry:** delete this when `ac` can emit a comparative verification report
itself — tracked in **#398**. Until then this is the procedure, and it is
deliberately explicit about the parts that went wrong the first time.

**Audience of the output:** someone who does not work on this software. No
tooling, no branch names, no `ac` internals in the report. State what was
measured, what it means, and what it does not cover.

---

## 1. Measurement protocol

A verification report needs a **level series**, not one sweep. Every fault
conclusion in the report is the cabinet compared against *itself* at different
drive levels, which is what makes the room and the microphone cancel.

Three sweeps per cabinet, 10 dB apart:

```bash
export HOME=<isolated session home>          # keeps the operator's config intact
export PATH=<session bin>:$PATH
ac setup output 0 input 0                    # AN1 -> speaker, IN1 -> mic
ac setup temp 28
for L in -50 -40 -30; do
  ac plot ir 20hz 20khz 4s ${L}dbfs 5harm 8192win 2s
  sleep 2
done
```

- **4 s sweep**, not 2 s: it puts the harmonic impulses ~400 ms apart instead
  of ~200 ms, which keeps the harmonic windows clean.
- **8192-sample gate** at 96 kHz = 85 ms, wide enough to hold a ~6 ms arrival
  plus decay.
- **2 s tail**, and still expect the ISO 18233 §6.3.2 tail-decay check to fail
  in the 32 Hz band. That is why the report excludes below 40 Hz.
- **Emission consent is per run and includes duration.** Ask before each new
  level, state how many seconds of sound. See `drive-level-consent` and
  `generate-has-no-duration` in the memory directory — `ac generate sine` has
  no duration argument and must never be backgrounded.

### Record these before the first sweep

Absolutely required for a later cross-cabinet comparison:

| what | why |
|---|---|
| `amixer -c0 cget numid=289` (`01-AN1 Playback Volume`) | changed mid-session on 2026-08-28 and silently broke the #1/#2 level comparison |
| `numid=301` (mic preamp gain) | part of the acoustic gain chain |
| mic distance and height | sets the floor-reflection notch frequency |
| room temperature | archived in the report |

The playback volume control is **exactly linear in amplitude** — verified on
the silent AN2→IN4 loopback: 16384 → 61523 → 16384 moved the deconvolved peak
0.4681 → 1.7575 → 0.4681, a ratio of 3.755 against a nominal 61523/16384 =
3.755. So a mid-session change is correctable as `20·log10(ratio)`, *but only
if the old value was written down*.

Absolute acoustic output of a run is:

```
output_dB = drive_dBFS + 20*log10(volume/65536) + measured_gain_dB
```

Use it to check the two cabinets were actually driven over comparable ranges.
On 2026-08-28 they were not: #2's loudest sweep was only 1.6 dB above #1's
quietest, which had to be stated in the report rather than buried.

### Silent controls cost nothing — use them

The AN2→IN4 loopback is a cable. Sweeping it makes no sound, needs no consent,
and settles "is this the instrument or the speaker?" immediately. Every
surprising acoustic result should get a loopback control run before it is
believed.

---

## 2. Analysis

Work from the archived report JSON, not from a re-measurement. Fetch them off
the rig before the session directory is cleaned:

```bash
scp 192.168.9.25:<session>/reports/<stamp>-plot_ir.json .
```

Each JSON carries `data[0].data` (`linear_ir`, `harmonics`, `sample_rate_hz`)
and `data[1].data.points` (the gated frequency response, 2049 points to
48 kHz).

### Microphone correction

Certificate: `~/src/ac/beyer/449350_34804_0Grad.txt` — beyerdynamic 449350
s/n 34804, 0° on-axis, 100 points, 50 Hz – 19.98 kHz, values −0.4 to +4.1 dB.
The value is how much **more sensitive** the capsule is, so a corrected
magnitude is **measured minus the curve**.

```python
import re
import numpy as np

def mic_correction(freqs, path='/home/mui/src/ac/beyer/449350_34804_0Grad.txt'):
    """dB to SUBTRACT from a measured magnitude at `freqs`."""
    f, v = [], []
    for ln in open(path, encoding='utf-8', errors='replace'):
        ln = ln.strip()
        if not ln or ln.startswith(';'):
            continue
        p = re.split(r'[\t ,;]+', ln)
        try:
            f.append(float(p[0])); v.append(float(p[1]))
        except (ValueError, IndexError):
            continue
    f, v = np.asarray(f), np.asarray(v)
    o = np.argsort(f); f, v = f[o], v[o]
    freqs = np.asarray(freqs, float)
    out = np.zeros_like(freqs)
    ok = freqs > 0
    # log-frequency interpolation; hold the end values flat outside 50 Hz-20 kHz
    # rather than extrapolating a +4 dB rise the certificate does not support
    out[ok] = np.interp(np.log10(freqs[ok]), np.log10(f), v, left=v[0], right=v[-1])
    return out
```

Rules:

- Apply to the **raw** spectrum **before** smoothing.
- Apply to the frequency response **only**. Not to the impulse response, and
  not to the harmonic figures — each of those is a single broadband ratio with
  no one frequency at which a frequency-dependent correction belongs. Say so in
  the report.
- It changes **no** pass/fail figure. Level linearity, arrival, repeatability,
  distortion-vs-level and any cabinet-vs-cabinet sensitivity gap all use the
  same microphone in the same place, so the curve is common-mode and cancels.
  Only the response curve moves.

### Smoothing and the statistics

Third-octave smoothing, arithmetic mean in dB over a `±1/6`-octave window:

```python
def sm(f, m, fr=2 ** (1 / 6)):
    o = np.empty_like(m)
    for i, fc in enumerate(f):
        if fc <= 0:
            o[i] = m[i]; continue
        b = (f >= fc / fr) & (f <= fc * fr)
        o[i] = m[b].mean() if b.any() else m[i]
    return o
```

Define each published number once and use that definition everywhere:

| figure | definition |
|---|---|
| gain / "output for input" | `20·log10(peak of linear_ir)`; spread across the level series |
| arrival | `argmax` of `abs(linear_ir)` minus `len/2`, in samples |
| noise floor | peak ÷ RMS of everything before the peak, minus a `len/32` guard |
| response flatness | max abs deviation from **that band's own median**, 1/3-oct |
| repeatability | max abs deviation of each level's curve from the mean of the three, over **100 Hz – 16 kHz** |
| harmonic level | `20·log10(peak of harmonic IR ÷ peak of linear IR)` |
| harmonic reality test | change per 10 dB of drive vs the order-n theory (+10 / +20 / +30 / +40 dB); a term that does not track drive is floor-limited and is an **upper bound** |

Always normalise per band against that band's own median, and quote the band
with the number. Both mistakes below came from getting this wrong.

---

## 3. Traps that cost time on 2026-08-28

**Normalisation trap — the one that produced a false finding.** Subtracting the
mic curve from a curve that was already normalised against the *uncorrected*
median shifts the whole curve and looks exactly like a rolloff. It led to
"a real 3–4 dB HF rolloff above 6 kHz", which does not exist. Re-normalise
after correcting, then compare like with like. Correctly done, mic correction
*improves* treble flatness above 5 kHz from ±3.26 to ±1.16 dB.

**Band-median trap.** After correction the 2–16 kHz flatness got *worse*
(±3.32 → ±4.99 dB) purely because the band median dropped and left a 2.46 kHz
peak standing proud. Nothing about the cabinet changed. Split the band or quote
the feature, don't report the aggregate as if it were a degradation.

**Sub-100 Hz swamps every aggregate.** Level-to-level deviation is 0.18 dB
above 100 Hz and 1.10 dB at 25 Hz. Bound every band explicitly — and note the
raw point grid runs to 48 kHz, so an unbounded `f >= 100` mask silently
includes the dead band above 20 kHz and returns nonsense.

**Minimum-search seeding.** Finding the floor notch with `ni = 0` then
`if in_band and v[i] < v[ni]` never updates when index 0 (25 Hz) is lower than
the in-band minimum, and the marker pins to the left edge. Seed with `-1` and
guard, or search the band slice directly.

**Display grid vs measured value.** Label features from the full-resolution
curve, not from the ~170-point log display grid — the grid rounded 492 Hz to
501 Hz and contradicted the prose.

**Check the axis against the data.** The corrected curve peaks at +15.6 dB and
silently clipped a +14 ceiling. Assert bounds programmatically before
publishing; every chart's projection is worth re-implementing in a few lines to
check it stays inside its viewBox.

---

## 4. Report structure

Verdict first, then the evidence, then what the run cannot support. Sections in
the order a sceptic asks for them:

1. **Verdict panel** — what a faulty cabinet does when you raise the level, and
   which of those this one did.
2. **Stat tiles** — the four headline numbers.
3. **Level-deviation chart** — each level minus the mean, on a ±1.2 dB axis.
   The fine scale *is* the argument; say so in the caption.
4. **Gain vs drive** — the same question asked a second way.
5. **Distortion by order** — as percentages for a lay reader, with the
   scaling-vs-theory column that separates real distortion from the floor.
6. **Frequency response** — mic-corrected, with the room-dominated region
   shaded and the floor-reflection notch marked.
7. **Arrival and reflections** — impulse envelope in 0.1 ms blocks.
8. **Measurements table** — every run, nothing hidden.
9. **Findings** — one row per claim with a status chip.
10. **What this does not cover** — not optional. It is the section that makes
    the rest trustworthy.

Attribute honestly. The 492 Hz notch is the floor and the geometry predicts it
(mic 0.30 m up, source ~0.38 m → 512 Hz predicted, 492 Hz measured). The
2.46 kHz peak is *not* explained by that comb, so it is labelled unattributed
rather than assigned to the cabinet — one on-axis point at 50 cm cannot
separate a driver feature from a room mode.

### Charts and palette

Load the `artifact-design` and `dataviz` skills before building. Run the
palette validator — do not eyeball it:

```bash
python3 <dataviz-skill>/scripts/validate_palette.py "#7FB0D6,#2E76B0,#0D3F66" \
        --ordinal --mode light --surface "#FBFBF9"
```

Drive levels are **ordinal**, so they take a sequential single-hue ramp, not
categorical hues. Validate light and dark separately; the dark lightness band
is narrower and a ramp that passes in light will usually fail in dark.

---

## 5. PDF rendering

The published artifact is the deliverable; the PDFs are rendered from it with
headless Chromium. Three things that are not obvious:

- **Chrome drops background colours when printing.** Without
  `print-color-adjust: exact` a dark page prints as dark text on white.
- **A background on `body` does not reach the page margins.** Set it on the
  root *alone*, with `body { background: transparent }`, so it propagates to
  the page canvas. Chrome still will not paint the `@page` margin area, so a
  genuinely edge-to-edge dark page needs `@page { margin: 0 }` with the inset
  moved into the content wrapper.
- **Margins vanish at page breaks, padding does not.** Convert the top margin
  of unboxed blocks to padding so anything landing at the top of a page keeps
  its air. Do *not* do this with a negative margin — it overrides the element's
  own margin and jams headings against the figure above.

```bash
chromium --headless --disable-gpu --no-sandbox --hide-scrollbars \
  --virtual-time-budget=25000 --run-all-compositor-stages-before-draw \
  --no-pdf-header-footer --print-to-pdf=out.pdf "file://$PWD/preview.html"
```

The artifact is content-only (no `<html>`/`<head>`/`<body>`), so wrap it in a
skeleton for local rendering. Keep the on-page print stylesheet **light** — it
is the right default for paper — and drive the dark variant by adding a class
on the root at render time.

Then **look at the rendered pages**. Reading the PDF back caught the clipped
axis label, the misplaced notch marker and the 501/492 Hz contradiction; none
of them were visible on screen.

---

## 6. Doing unit #1

After the driver swap, to make #1 directly comparable with #2:

1. Set `01-AN1` to **16384** and confirm it, since that is what #2 was measured
   at. Record it.
2. Same mic geometry — 50 cm, 30 cm above the floor. The arrival landing at
   **+575 samples** again is the check that the position was reproduced; it was
   identical across all six runs on 2026-08-28.
3. Same ladder: −50 / −40 / −30 dBFS, 4 s, `5harm 8192win 2s`.
4. Loopback control run at the same settings before believing anything odd.
5. Reuse the analysis above, including mic correction, then regenerate both the
   #1 report and the pair report — the pair report's curves are still
   uncorrected and will need it.

#2's archived numbers to compare against: gain +5.07 / +5.24 / +5.09 dB,
arrival +575 samples on all three, noise floor 64.1 / 64.2 / 64.4 dB, 2nd
harmonic 0.202 % at the top step, flatness ±1.16 dB above 5 kHz.

---

Native support tracked in **#398** — "ac report renders a single run, so a
multi-run verification report has to be assembled by hand". When that lands,
this file goes.
