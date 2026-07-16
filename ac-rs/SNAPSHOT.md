# `.acsnap` — Snapshot File Format

Authoritative reference for the `.acsnap` binary format (handoff:
snapshot-backend M1, D4/D5). For the 4 CTRL commands that create and
transfer these files, see `ZMQ.md`'s `snapshot` / `snapshot_fetch` /
`snapshot_list` / `snapshot_delete` sections.

---

## What a snapshot is

A snapshot is **raw pre-processing capture plus full provenance** — not a
saved display. It captures every session channel's raw samples exactly as
delivered by the audio backend, before any gain, calibration, weighting,
or DSP touches them. Every calibrated or derived quantity a live
`transfer_stream` session ships on the wire (H1, calibrated spectra, SPL)
is re-derivable offline from a `.acsnap`'s raw samples, using the
identical `ac-core` functions the daemon's live path calls — see
`ac_core::visualize::pair_derivation` and `ac_core::snapshot::Snapshot::derive_pair`.

**Self-containment is a hard requirement.** Reading and reprocessing a
`.acsnap` needs no daemon, no audio backend, and no external config
file — everything required lives in the file's own bytes. A `.acsnap`
written today must reprocess identically on another machine, with a
future version of this same code, years from now.

## Container

A `.acsnap` file is a **zip archive with exactly two entries**:

| Entry | Contents |
|-------|----------|
| `meta.json` | Full provenance — see schema below |
| `audio.flac` | Raw multichannel audio, 24-bit, one FLAC stream |

Both entries are required; a reader must reject a file missing either one.

## Audio: `audio.flac`

- **One multichannel FLAC stream**, 24-bit signed samples, interleaved
  across all channels in `meta.json`'s `channel_map` order.
- **`f32 → i24` conversion**: scale by `2²³` (8,388,608), round to
  nearest, saturate to `[-2²³, 2²³-1]`. Samples that already sit on the
  i24 grid (real 24-bit ADC hardware) round-trip **bit-exact**.
  Synthetic/fake-audio `f32` that doesn't sit on the grid quantizes at
  the 1-LSB floor, `20·log10(1/2²³) ≈ -138.99 dBFS` — any tolerance
  comparing live vs. reprocessed values must account for exactly this
  floor and nothing more (see `it_snapshot.rs`'s I-B parity test for the
  worked derivation).
- Encoded via `flacenc` (pure Rust, no system library — required since
  `ac-view`, D8, links `ac-core` directly and must build on whatever
  platform it ships on). Decoded via `claxon` (also pure Rust), not
  `flacenc`'s own `decode` feature — that feature is explicitly marked
  experimental upstream and isn't used here.
- Below FLAC's minimum block size (32 frames in the encoder used here),
  `write_acsnap` refuses to encode rather than emit an undecodable
  stream — a `snapshot` requested moments after a transfer session
  starts, before the ring has meaningfully filled, fails clearly instead
  of producing a broken file.

## Provenance: `meta.json`

```json
{
  "format_version": 1,
  "sr": 48000,
  "channel_map": ["meas_0", "ref"],
  "per_channel": [
    {
      "role": "meas_0",
      "input_channel": 0,
      "weighting": "Z",
      "integration": "fast",
      "calibration": { "...": "full Calibration struct, or null" }
    },
    {
      "role": "ref",
      "input_channel": 1,
      "weighting": "Z",
      "integration": "fast",
      "calibration": null
    }
  ],
  "session": {
    "pairs": [[0, 1]],
    "delay_samples": [0],
    "nperseg": 48000
  },
  "captured_at_utc": "2026-07-16T00:00:00Z",
  "daemon_version": "0.2.0",
  "ring_duration_s": 30.0
}
```

| Field | Type | Notes |
|-------|------|-------|
| `format_version` | int | Currently `1`. A reader **must refuse** an unrecognised version rather than guess at the schema — bump this on any breaking layout change (e.g. a future 32-bit FLAC path). |
| `sr` | int | Sample rate, Hz. Also `audio.flac`'s own stream rate — a reader cross-checks the two match. |
| `channel_map` | `[string]` | FLAC stream channel index → session role (`"meas_0"`, `"meas_1"`, `"ref"`, …). The field a reader checks first. |
| `per_channel` | `[ChannelMeta]` | Same order as `channel_map`. |
| `per_channel[i].role` | string | Matches `channel_map[i]`. |
| `per_channel[i].input_channel` | int | Session-level capture-port index (independent of FLAC stream position). |
| `per_channel[i].weighting` | `"A"｜"C"｜"Z"` | String-identical vocabulary to the M0 `transfer_stream` frame's `spl_weighting` tag. |
| `per_channel[i].integration` | `"fast"｜"slow"` | String-identical vocabulary to `spl_integration`. |
| `per_channel[i].calibration` | object or `null` | Full 3-layer `Calibration` (voltage / SPL / mic-curve) in effect at capture time. `null` when the channel had no cal entry. |
| `session.pairs` | `[[int,int]]` | `(meas_input_channel, ref_input_channel)` per pair, session indices — not FLAC stream positions. |
| `session.delay_samples` | `[int]` | Per-pair ref↔meas delay in samples, same order as `pairs`. |
| `session.nperseg` | int | Welch segment length in effect. `h1_estimate_core` currently pins this to `sr`, but it's recorded explicitly — a future estimator change can't silently break old snapshots. |
| `captured_at_utc` | RFC3339 string | Wall-clock instant `snapshot` was triggered (the ring's *tail* — the ring's start is `ring_duration_s` seconds earlier). |
| `daemon_version` | string | `ac-daemon`'s own version string. |
| `ring_duration_s` | float | Actual captured duration in this file (≤ the session's configured `snapshot_ring_s` — shorter if the session hadn't run that long yet). |

## Offline derivation

`ac_core::snapshot::read_acsnap(bytes) -> Result<Snapshot>` decodes a
`.acsnap`'s bytes into raw per-channel samples plus the parsed
`SnapshotMeta`. `Snapshot::derive_pair(pair_idx, weighting, sample_range)`
then reproduces one pair's H1, calibrated `meas_spectrum`/`ref_spectrum`,
and `spl` — under a **caller-chosen** weighting curve and, via
`sample_range`, a caller-chosen sub-window of the capture (FFT/Welch
params are edit-time choices on a snapshot, D11 — the live session's own
choices are recorded in `meta.json` but not binding on reprocessing).

This calls the exact same low-level functions the live daemon path calls
(`h1_estimate_with_delay`, `spectrum_to_columns_wire`,
`weighted_broadband_dbfs`) — see `ac_core::visualize::pair_derivation`.

**I-B parity — what's actually verified, honestly.** Exact per-frame H1
parity (magnitude, phase, coherence — not just `meas_spectrum`) *is*
tested, under a correlated stimulus: `full_ib_parity_under_correlated_stimulus`
(`it_snapshot.rs`, handoff: parity-completion M1.5) drives
`transfer_stream`'s `fake_correlated_pair` mode (a seeded broadband
source on ref; meas is the same source scaled by a known `gain` and
delayed by a known `delay_samples` — a fake DUT with real ground truth),
and asserts live-vs-snapshot-reprocessed agreement on `meas_spectrum`,
`ref_spectrum`, `spl`, `|H1|`, phase, and coherence, plus both sides
independently against the ground truth (`|H1| = gain`, coherence ≥ 0.99)
— measured within ~0.1 dB / ~0.0001 coherence at the seed/gain/delay
that test uses.

**Under uncorrelated or low-coherence signals — including the daemon's
default passive `--fake-audio` stimulus, which puts two clean but
*uncorrelated* deterministic tones on meas vs. ref (different
frequencies per channel, `audio/fake.rs`) — snapshot-derived H1 matches
live statistically, not per-frame.** With no true underlying transfer
function to converge to, H1's magnitude is a noise/noise ratio,
sensitive to exact sample-window alignment in a way a wall-clock-
correlated snapshot trigger can't guarantee frame-for-frame (a QA pass
measured ~7 dB live-vs-reprocessed drift under exactly this condition,
traced to the stimulus property, not a reprocessing defect —
`snapshot_reprocessing_matches_live_frame_within_tolerance` covers this
case using `meas_spectrum` instead, which only depends on meas's own
signal and isn't affected). Use `fake_correlated_pair` (or a real
loopback) whenever H1/coherence reprocessing needs to be trusted
per-frame; under any other stimulus, only spectra/SPL parity is
verified.

## Fixture

`tests/fixtures/snapshot-fixture-v1.acsnap` (repo root) is a checked-in,
synthetic `.acsnap` used by `ac-core`'s self-containment test
(`snapshot::tests::t3_checked_in_fixture_reprocesses_with_no_daemon`) and
reserved as the substrate for M2's display-truth fixtures. Regenerate via:

```
cargo test -p ac-core --lib snapshot::tests::generate_snapshot_fixture -- --ignored
```
