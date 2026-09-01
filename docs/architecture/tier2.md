### Tier 2 — Live analysis

### Tier 2 commands

- `ac transfer` — transfer function
- `ac monitor` — defaults to FFT spectrum
- `ac monitor spectrum` — explicit form
- `ac monitor cwt` — Morlet wavelet scalogram
- `ac monitor cqt` — constant-Q transform (Brown 1991 / Schörkhuber-Klapuri pragmatic)
- `ac monitor reassigned` — Auger-Flandrin reassigned spectrogram
- `ac tuner` — pitch tracker (Tier 2 but pre-existing; keep the name)

The existing `monitor_spectrum` ZMQ command is retained as the transport
layer. The CLI surface adds the `ac monitor <mode>` parsing on top. An
`ac monitor cwt` call sends `monitor_spectrum` with a mode set to `cwt`
(the server already has `analysis_mode` supporting this).

Goal: **show the user what the signal is doing right now, with numbers
they can trust.** Every visible dB value is a real, calibration-aware
measurement at the technique's bin centre — not a sketch or a hint.
What separates Tier 2 from Tier 1 is the *technique* (Morlet wavelet,
constant-Q kernel, reassigned STFT, …) not the numeric rigor inside
that technique.

Properties:
- Numeric rigor matches Tier 1: f64 internally, dBFS calibrated, peak
  amplitude correctly recovered for an aligned cosine. The voltage /
  SPL / mic-curve calibration layers (`shared/calibration/`) apply
  identically to Tier 1 and Tier 2 reads — a level shown on a CWT
  waterfall reflects the same physical quantity as the same level
  read off `ac plot`.
- The technique itself is chosen for time-frequency behaviour and
  per-frame CPU budget, not for standards conformance. CWT uses
  Morlet wavelets, CQT uses Hann-windowed Q-invariant kernels,
  reassigned uses Auger-Flandrin reassignment — none is an
  IEC 61260-1 fractional-octave filterbank. A CWT band level is a
  real, calibrated level at that *kernel's* centre frequency, but it
  is not "the 1/3-octave band level per IEC 61260-1 Class 1."
- Every influence on a displayed number — calibration (voltage / SPL /
  mic-curve), smoothing, fractional-octave aggregation, A / C / Z
  weighting, time integration, mic-correction enable — is surfaced
  as a labelled tag in the overlay so the user can always tell what
  is affecting the reading, and toggling any of them shows the effect
  live.
- Output is a continuous **frame stream** — magnitudes + frequencies
  per tick — not an archived report. Subscribers that want long-term
  storage record the frames; the daemon does not.
- Tested for correctness against synthetic signals: pure tones read
  expected dBFS within bin-leakage tolerance, calibrated cosines read
  expected dB SPL with both SPL offset and mic-correction applied,
  chirps track diagonally on log-frequency axes.

### Tier 2 frames

- `visualize/spectrum`
- `visualize/cwt`
- `visualize/cqt`
- `visualize/reassigned`
- `visualize/fractional_octave`
- `visualize/fractional_octave_leq`
- `visualize/tuner`

All four spectrum-shaped frames (`spectrum`, `cwt`, `cqt`, `reassigned`)
share the same `magnitudes` + `frequencies` payload and carry an
optional `spl_offset_db` field plus a `mic_correction` tag — see the
calibration composition note below.

Existing frames (`type: "spectrum"`, `type: "tuner"`, etc.) are aliased
during migration: the server emits both old and new types for one
release cycle, then drops the old. Python test clients and existing UI
code continue to work during the transition.

## Testing strategy

### Tier 2

- Unit tests for correctness of the underlying transform (CWT of a
  tone lands at the right scale with the right magnitude).
- Property tests for robustness (random input does not panic, NaN is
  handled, edge-case sample rates work).
- Performance tests / benchmarks guard against regressions in the
  per-frame cost, since visual fluidity is the tier's objective.


### Transfer

- H1 estimator (`ac-core/visualize/transfer.rs`) use Müller-Massarani windowed cross-correlation. Estimator internal changes must preserve math correctness of transfer function estimate.
