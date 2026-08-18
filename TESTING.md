# Testing

Run all tests:
```bash
cd ac-rs && cargo test --workspace # ~900 tests, 14 #[ignore]'d
pytest tests/ -q                  # black-box ZMQ protocol tests (spawns Rust daemon)
```

**Counts in this file are an order of magnitude, not a figure to check
against** — they rot silently. Run the command. `--workspace` matters: two
branches that each pass `cargo test -p <crate>` can still break the build
together, and with no CI here that is the only check.

**An exit status only answers for the command you asked.** Any construct that
reports somebody else's status will eventually report a lie — a pipeline gives
you the last stage's, a `;` or `&&` chain gives you the last command's, and the
same goes for `xargs` and subshells. Ask cargo, then read cargo's answer:

```bash
cargo test --workspace > /tmp/ws.log 2>&1; echo "exit: $?"      # status is cargo's
cargo test --workspace 2>&1 | tee /tmp/ws.log; echo "${PIPESTATUS[0]}"
```

Redirect and read the file. If you must pipe, `${PIPESTATUS[0]}` is the only
status worth quoting — and never truncate the output you are counting from,
which is the other half of the same mistake.

Three instances inside one session, and the third landed while the note about
the first two was being written:

| construct | reported | actual |
|---|---|---|
| `cargo test \| tail` | `tail`'s 0, whatever the tests did | — |
| `cargo test \| grep … \| head -20` | 890 passing | 904 — the result lines were truncated |
| `cargo test …; grep -c FAILED log` | **failure** | 905 passing, 0 failed |

The third is the instructive one, because **nothing malfunctioned**: `grep -c`
correctly found zero matches and correctly exited 1 to say so. The status was
accurate for the question grep was asked and meaningless for the question being
answered.

It is also the expensive direction. A false green hides a defect; a false red
sends someone hunting a regression that does not exist — the same shape as the
stale `ac-view` snapshots in `work/rig/rig-verify-queue.md`, where the first
pixel-diff failure is a real defect the tests could not see and reads as one
they introduced.

Same family as verifying an installed binary by sha256 rather than by size and
mtime: the convenient reading is not the evidence, and it fails quietly.

Among the `#[ignore]`'d, `it_loopback_ir` drives a Farina sweep through real
JACK port-to-port loopback and is run manually after starting `jackd -d dummy`
— see ARCHITECTURE.md → "Loopback IR runbook". The rest need real hardware, a
live daemon, or a real GPU adapter.

No JACK daemon or audio hardware required for the default suite — pytest spawns `ac-daemon --fake-audio` (synthetic sine + 1% 2nd harmonic) on free ports and connects via ZMQ.

## Rust tests

```bash
cd ac-rs
cargo test -p ac-core             # Tier 1 measurement (THD, filterbank, weighting, noise, sweep IR,
                                  #   loudness), Tier 2 visualize (spectrum, CWT, CQT, reassigned,
                                  #   fractional-octave, time integration), shared (3-layer
                                  #   calibration, conversions, generator, config)
cargo test -p ac-cli              # parse + command tests — all commands, abbreviations, defaults,
                                  #   error cases (incl. SPL / mic-curve subcommands)
cargo test -p ac-daemon           # ZMQ protocol, sweep IR, calibrate flows, monitor frame shapes,
                                  #   plus the #[ignore]'d loopback runbook
cargo test -p ac-scene            # scene/data layer: wire-frame parse, transfer scene, MTW display,
                                  #   smoothing, fault table, fixtures
cargo test -p ac-view             # egui shell: geometry, live/snapshot end-to-end, banner clearance,
                                  #   trace distinction (egui_kittest snapshots)
cargo test --workspace            # all five crates — the only check that catches a cross-crate break
```

Rough shape as of 2026-08-06 (899 passed, 14 ignored): `ac-core` 396,
`ac-daemon` 214, `ac-scene` 115, `ac-cli` 97, `ac-view` the remainder.

## Checked-in fixtures — and which comparison each one earns

Three fixtures are committed, each written by an `#[ignore]`d regenerator and
read by tests in the default suite:

| fixture | regenerator | currency check |
|---|---|---|
| `tests/fixtures/snapshot-fixture-v1.acsnap` | `ac-core` `snapshot::tests::generate_snapshot_fixture` | sha256, exact |
| `tests/fixtures/transfer-frame-v2.json` | `ac-scene` `regenerate_fixture` | numeric, 1e-9 relative |
| `tests/fixtures/transfer-frame-v2-live.json` | `ac-daemon` `it_scene_fixture` | key set, types, `cal_tags` |

Each currency check rebuilds the artefact and compares it to the committed
file, so a regeneration that changes something shows up in a diff instead of
passing unnoticed (#271). Without them, a format change that updates the writer
and every reader together leaves the fixture describing something neither side
produces, and every consuming test keeps passing against it.

**Which comparison is honest depends on bit-reproducibility, not on purity.**
This is the part that is easy to get wrong: `derive_pair` is a *pure* function
of the committed `.acsnap` and still not bit-reproducible, because FFT
ordering, FMA contraction and library versions move an `f64` in the last bits.
Measured across a rebuild: 1.1e-16 on `coherence`, 8.9e-16 dB on
`magnitude_db`, 3.6e-12 Hz on `spec_freqs`. Exact comparison there fails on
noise, and **a test that fails on 1e-16 gets deleted rather than debugged** —
taking the check with it.

So:

- **Bit-reproducible** (integer construction, no float reduction — the
  `.acsnap`): compare exactly, by hash.
- **Pure but float-derived** (`transfer-frame-v2.json`): compare numerically,
  with a tolerance far above measured ULP noise and far below any change worth
  catching. 1e-9 relative, and the measured noise table is in the test's doc
  comment so nobody tightens it blind.
- **Captured live** (`transfer-frame-v2-live.json`): do **not** compare values
  at all. Its numbers carry capture jitter. Compare what is stable and what the
  fixture exists to protect — the key set, the field types, and the `cal_tags`
  vocabulary.

**Regenerate deliberately.** A currency check going red means the artefact and
the code disagree; the fix is to find out which moved before committing a
regenerated file. Regenerating reflexively to get green is the same act as
deleting the check, one step slower.

## Build

```bash
cd ac-rs
cargo build                       # all crates: ac (CLI), ac-daemon
cargo build --release             # optimized
```

The Rust CLI auto-discovers the debug build at `ac-rs/target/debug/ac-daemon`. For production installs:

```bash
cargo build --release
sudo install -m 755 target/release/ac target/release/ac-daemon /usr/local/bin/
```

Use `--fake-audio` to run the daemon without JACK (for integration testing):

```bash
ac-daemon --local --fake-audio
```

## Built-in self-tests

In addition to pytest, `ac` has built-in self-tests runnable without pytest:

```bash
ac test software              # validates analysis pipeline + conversions (no hardware)
ac test hardware              # hardware validation (requires 2 loopback pairs)
ac test hardware dmm          # + cross-check against DMM over SCPI
ac test dut                   # DUT characterization (requires 2 loopback pairs)
ac test dut compare           # A/B comparison (prompts to bypass DUT)
ac test dut -10dbu            # DUT test at specific level
```

Short forms: `ac te so`, `ac te h`, `ac te h dmm`, `ac te du`, `ac te du comp`

## Test files

### Python (tests/)

`tests/` contains black-box ZMQ protocol tests that spawn the Rust daemon
with `--fake-audio` and exercise the full wire protocol end-to-end
(see `ac-rs/ZMQ.md` for the authoritative spec). The suite has no Python
runtime dependency — the pyzmq client lives inline in `conftest.py`.

### Rust unit tests

Where they live, by crate. Per-module counts are deliberately not listed —
they were wrong within weeks last time, and `cargo test -p <crate>` answers
the question directly.

#### ac-core

Unit tests sit in `#[cfg(test)]` modules beside the code:

- `measurement/` — `thd`, `filterbank`, `weighting`, `ccir468`, `noise`,
  `sweep`, `loudness`, `report`, `report_html`, `report_pdf`
- `visualize/` — `spectrum`, `cwt`, `cqt`, `reassigned`, `aggregate`,
  `fractional_octave`, `smoothing`, `spl_level`, `time_integration`,
  `transfer`, `pair_derivation`, `weighting_curves`, `mtw/`
- `shared/` — `calibration`, `conversions`, `constants`, `generator`,
  `mic_curve_filter`, `reference_levels`, `time`, `types`, `fft_cache`

#### ac-cli

`parse/` (one module per command group) and `commands/`. Covers every
command, its abbreviations, defaults and error cases.

#### ac-daemon

Unit tests in `audio::{jack_backend, cpal_backend, fake}`, `gpio`, and the
`handlers/` modules. Integration binaries in `tests/`: `it_protocol`,
`it_snapshot`, `it_set_drive`, `it_scene_fixture`, `it_cross_tier_parity`,
and the `#[ignore]`'d `it_loopback_ir`.

#### ac-scene

`tests/`: `it_transfer`, `it_live_frame`, `it_mtw_display`, `it_smoothing`,
`it_fixtures`. This is where a displayed *value* is asserted — no window
needed, which is the point of the crate split.

#### ac-view

`tests/`: `it_geometry`, `it_transfer_geometry`, `it_banner_clearance`,
`it_trace_distinction`, `it_live_end_to_end`, `it_snapshot_end_to_end`,
`it_stimulus_live`, `it_remote`, plus `egui_kittest` snapshots under
`tests/snapshots/`. Layout and composition only.

### A3 snapshot reference currency

`it_transfer_snapshots`'s 7 `#[ignore]`'d tests pixel-diff `draw_view`
against committed PNGs, but the gate is real-adapter-only and there is no
CI — nothing re-runs it when `draw_view` or a pane changes. A layout
change that doesn't also regenerate the references leaves the gate
reporting coverage it no longer has (#337: 5 of the 7 references sat
stale for weeks after #245/#252 shifted every view down half a text line,
undetected because nobody was obliged to run the gate).

**The references are current only as of the rig run recorded in
`it_transfer_snapshots.rs`'s doc comment** (box, date, commit). Treat that
line as the source of truth for staleness, not this file.

**Checklist — any PR touching `ac-view/src/view.rs`'s `draw_view` or a
pane module:**
- [ ] Regenerate the 7 references on the rig (192.168.9.25) in that PR —
      `UPDATE_SNAPSHOTS=1 cargo test -p ac-view --test it_transfer_snapshots
      -- --ignored --test-threads=1`, then a plain re-run in the same
      session — and update the doc-comment provenance line, **or**
- [ ] state in the PR body why the change cannot affect rendered pixels
      (e.g. a change gated behind a flag the fixtures don't exercise).

**What makes this come back negative:** a PR that changes `draw_view` or a
pane, ships without either a regeneration or a stated reason, and is
reviewed anyway — same shape as #337. QA checks the PR diff for `draw_view`
or pane-module changes and requires one of the two boxes above before
approving (see `.agents/qa.md`'s display-truth gate).

## What is verified numerically

### THD accuracy (ac-core `analysis` module)

These tests generate synthetic signals with mathematically known distortion and verify the analyzer returns correct values:

- **1% 2nd harmonic** → THD = 1.000% (±0.05%)
- **1% H2 + 0.5% H3** → THD = sqrt(1² + 0.5²) = 1.118% (±0.05%)
- **0.01% 2nd harmonic** → THD = 0.010% (±0.005%)
- **Three equal 1% harmonics** → THD = sqrt(3) ≈ 1.732% (±0.1%)
- **Pure sine** → THD < 0.01%
- **THD+N ≥ THD** always (physical law)
- **THD+N within 0.5x–10x of THD** (guards against np.mean vs np.sum bugs)

### THD across the audio band

- THD measured at 100, 440, 1000, 5000, 10000 Hz — all within ±0.1% of expected
- THD measured at amplitudes 0.01, 0.1, 0.5, 0.9 — level-independent (±0.1%)

### Fundamental & RMS

- **fundamental_dbfs** scales correctly: 10x amplitude = 20 dB, 5x = 13.98 dB
- **linear_rms** = amplitude / sqrt(2) for pure sine (±1% relative)
- **Harmonic amplitudes** (H2/H3 ratios vs fundamental) match injected values (±10% relative)

### Noise floor

- Injecting broadband noise raises the measured noise floor proportionally
- Clean sine noise floor is lower than noisy sine noise floor

### Unit conversions (ac-core `conversions` module)

- 0 dBu = 0.77459667 Vrms (standard definition)
- +4 dBu = 1.228 Vrms (pro audio reference)
- +20 dBu = 7.746 Vrms
- Vrms ↔ dBu roundtrip within 1e-9
- dBFS → Vrms: -20 dBFS with ref 1.0 = 0.1 Vrms
- Full chain: dBFS + calibration ref → Vrms → dBu (verified against manual calculation)
- Vpp = Vrms × 2√2

### Calibration (ac-core `calibration` module)

- `out_vrms(-20 dBFS)` with cal 0.245 → 0.0245 Vrms
- `in_vrms(linear_rms)` = linear_rms × vrms_at_0dbfs_in
- Uncalibrated → returns None (not NaN, not crash)
- Save/load roundtrip preserves values to 1e-9

### Integration: end-to-end THD (test_server_client.py)

The Rust fake audio engine generates amplitude 0.1 with 1% 2nd harmonic. Through the full pipeline (fake engine → analyze → sweep_point_frame → ZMQ → client):

- **THD ≈ 1.0%** (0.8–1.3% tolerance for transport/rounding)
- **fundamental_dbfs ≈ -20 dBFS** (±2 dB)
- **THD+N ≥ THD** verified through the full stack
- **plot_level** produces correct step count and cmd field

### None vs NaN safety (test_server_client.py)

Without calibration, `gain_db`, `out_dbu`, `in_dbu` are `None` in sweep_point frames. Tests verify:
- These fields are indeed `None` (not missing, not NaN)
- The correct pattern (`p["gain_db"] if p.get("gain_db") is not None else np.nan`) produces `float64` arrays
- The buggy pattern (`.get("gain_db", np.nan)`) produces `object` arrays — confirming why the gain line vanished

## Known limitations

### Spectrum downsampling (display only)

`_downsample_spectrum()` uses geomspace point-sampling to reduce ~24000 FFT bins to ~1000 for UI display. Narrow peaks at exact FFT bin frequencies can fall between sampled indices and appear as zero. This does NOT affect measurement values (THD, harmonics, noise floor are computed from the full spectrum). Tested in `test_downsample_structure` and `test_downsample_short_spectrum_passthrough`.

### Noise floor algorithm

The time-domain subtraction method (subtract reconstructed sines from waveform) has a measurement floor of approximately -38 dBFS for a clean synthetic sine due to windowing artifacts. Real-world signals with broadband noise are measured correctly relative to each other.

### Fake audio engine (`--fake-audio`)

Tests use the Rust `FakeEngine` which produces synthetic float32 sine waves, not real audio. It does not simulate:
- Actual latency or jitter
- ADC/DAC nonlinearity
- Real noise floors
- Sample rate drift

Integration tests verify the software pipeline is correct; hardware validation requires real equipment — use `ac test hardware`.

## Hardware validation (`ac test hardware`)

Requires two loopback pairs: `output_channel` → `input_channel` (pair A) and a second output → `reference_channel` (pair B). Configure with `ac setup output N input N reference M`. Stimulus is sent to both output ports simultaneously.

| Test | What it measures | Pass criteria |
|------|-----------------|---------------|
| Noise floor | RMS level with silence on both inputs | < -80 dBFS |
| Level linearity | -42 to -6 dBFS in 6 dB steps, check monotonicity | monotonic, step error < 1 dB (1.5 dB top step) |
| THD floor | THD at 1 kHz across levels (-40 to -3 dBFS) | best THD < 0.05% |
| Frequency response | Tone at 50–20kHz, deviation from 1 kHz ref | < 1.0 dB |
| Channel match | Same stimulus on both inputs, compare levels and THD | level delta < 0.5 dB, THD delta < 0.01% |
| Channel isolation | Disconnect ref output, tone on primary, measure ref input | < -60 dBFS (skipped if same output) |
| Repeatability | Same measurement 5x, check variance | level sigma < 0.05 dB, THD sigma < 0.005% |

### DMM cross-check (`ac test hardware dmm`)

Requires `ac setup dmm <ip>` and calibration (`ac calibrate`).

| Test | What it measures | Pass criteria |
|------|-----------------|---------------|
| Absolute level | -10 dBFS vs DMM Vrms vs calibration prediction | < 1% error |
| Level tracking | Sweep -40 to 0 dBFS, DMM vs predicted at each step | < 2% error |
| Freq response | Same level at 100–20kHz, check DMM reads flat | < 1.0 dB deviation |

## DUT characterization (`ac test dut`)

Requires two loopback pairs (same as hardware test). Signal path: `output_channel` → DUT → `input_channel` (measurement), `reference_channel` output → `reference_channel` input (direct loopback reference). Uses `capture_block_stereo()` for simultaneous measurement + reference capture.

| Test | What it measures | Reports |
|------|-----------------|---------|
| Noise floor | DUT output with no stimulus | dBFS |
| Gain | Level difference between measurement and reference at 1 kHz | dB (+ ref/meas levels) |
| THD vs level | THD, THD+N, and gain at 1 kHz across drive levels (-40 to -3 dBFS) | best THD%, per-level breakdown |
| Frequency response | H1 transfer function (pink noise, 4s capture) | deviation range, coherence, delay |
| Clipping point | Level sweep upward until THD > 1% | onset level in dBFS |

### Compare mode (`ac test dut compare`)

Runs the full 5-measurement suite twice: once with DUT in the signal path, then prompts the user to bypass the DUT and runs again. Results are tagged `[With DUT]` and `[Bypass]` for comparison.

### With direct loopback (no DUT)

Expected results: gain ≈ 0 dB, flat frequency response (±0 dB), coherence = 1.000, delay = 0 ms, very low THD. Useful as a baseline sanity check.

## Adding tests

- **Parser tests**: add to the matching module under `ac-rs/crates/ac-cli/src/parse/` (`#[cfg(test)]` module). Pure function input/output.
- **Analysis tests**: add beside the code — `ac-rs/crates/ac-core/src/measurement/` for Tier 1 (THD lives in `thd.rs`), `visualize/` for Tier 2. Build synthetic signals with known properties. Always assert exact numerical values, not just ranges.
- **Displayed-value tests**: add to `ac-rs/crates/ac-scene/`, not `ac-view`. If a number or string can be wrong, it must be assertable without a window.
- **Black-box protocol tests**: add to `tests/test_server_client.py`. Use the session-scoped `server_client` fixture. Must drain to `done`/`error` before returning so the server is idle for the next test.
- **Daemon integration tests**: add to `ac-rs/crates/ac-daemon/tests/it_protocol.rs` for scenarios needing fine-grained control over daemon state.
