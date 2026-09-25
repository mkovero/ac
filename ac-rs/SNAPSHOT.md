# `.acsnap` — Snapshot File Format

Authoritative reference for the `.acsnap` binary format (handoff:
snapshot-backend M1, D4/D5). For the 4 CTRL commands that create and
transfer these files, see `ac-rs/ZMQ.md`'s `snapshot` / `snapshot_fetch` /
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
  "format_version": 2,
  "sr": 48000,
  "channel_map": ["meas_0", "ref"],
  "per_channel": [
    {
      "role": "meas_0",
      "input_channel": 0,
      "weighting": "Z",
      "integration": "fast",
      "calibration": { "...": "full Calibration struct, or null" },
      "stream_sha256": "<64 lowercase hex: digest of audio.flac stream 0>"
    },
    {
      "role": "ref",
      "input_channel": 1,
      "weighting": "Z",
      "integration": "fast",
      "calibration": null,
      "stream_sha256": "<64 lowercase hex: digest of audio.flac stream 1>"
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
| `format_version` | int | `write_acsnap` writes `2`; `read_acsnap` reads `1` and `2`. A reader **must refuse** an unrecognised version rather than guess at the schema — bump this on any breaking layout change (e.g. a future 32-bit FLAC path). v2 (#637) added `per_channel[i].stream_sha256`; v1 files are read exactly as before, under the v1 limit in *Reader validation*. |
| `sr` | int | Sample rate, Hz. Also `audio.flac`'s own stream rate — a reader cross-checks the two match. |
| `channel_map` | `[string]` | FLAC stream channel index → session role (`"meas_0"`, `"meas_1"`, `"ref"`, …). The field a reader checks first. |
| `per_channel` | `[ChannelMeta]` | Same order as `channel_map`. |
| `per_channel[i].role` | string | Matches `channel_map[i]`. |
| `per_channel[i].input_channel` | int | Session-level capture-port index (independent of FLAC stream position). |
| `per_channel[i].weighting` | `"A"｜"C"｜"Z"` | String-identical vocabulary to the M0 `transfer_stream` frame's `spl_weighting` tag. |
| `per_channel[i].integration` | `"fast"｜"slow"` | String-identical vocabulary to `spl_integration`. |
| `per_channel[i].calibration` | object or `null` | Full 3-layer `Calibration` (voltage / SPL / mic-curve) in effect at capture time. `null` when the channel had no cal entry. |
| `per_channel[i].stream_sha256` | string | **v2: required. v1: must be absent.** SHA-256, 64 lowercase hex characters, of `audio.flac` stream `i`: every sample on the i24 grid as `i32` little-endian, in stream order over the whole stream. The writer computes it from the audio it encodes. A binding check — which stream this entry describes — not integrity or tamper protection. |
| `session.pairs` | `[[int,int]]` | `(meas_input_channel, ref_input_channel)` per pair, session indices — not FLAC stream positions. |
| `session.delay_samples` | `[int]` | Per-pair ref↔meas delay in samples, same order as `pairs`. |
| `session.nperseg` | int | Welch segment length in effect. `h1_estimate_core` currently pins this to `sr`, but it's recorded explicitly — a future estimator change can't silently break old snapshots. |
| `captured_at_utc` | RFC3339 string | Wall-clock instant `snapshot` was triggered (the ring's *tail* — the ring's start is `ring_duration_s` seconds earlier). |
| `daemon_version` | string | `ac-daemon`'s own version string. |
| `ring_duration_s` | float | Actual captured duration in this file (≤ the session's configured `snapshot_ring_s` — shorter if the session hadn't run that long yet). |

### Reader validation

A FLAC stream is linked to its metadata by position:
`per_channel[i]` ↔ `channel_map[i]` ↔ decoded stream `i`. In format v2
each `per_channel` entry also carries the digest of the stream it
describes, so the reader can check that link. `read_acsnap` refuses a file
that breaks any of these rules, and `write_acsnap` refuses to write one
(`ac_core::snapshot::validate`, shared by both):

1. `per_channel` and `channel_map` have the same length, and it equals
   the number of streams in `audio.flac`.
2. `per_channel[i].role == channel_map[i]` for every `i`.
3. Every `per_channel[*].input_channel` is unique.
4. `session.delay_samples` has exactly one entry per `session.pairs` entry.
5. Both inputs of every pair equal the `input_channel` of some
   `per_channel` entry. Rule 3 makes that entry the only one.
6. A channel whose `voltage_check` is refused carries no
   `vrms_at_0dbfs_*` in its `calibration` (#466).
7. **v2 only.** `per_channel[i].stream_sha256` equals the digest of
   decoded stream `i` (#637). Because the digest travels inside the entry,
   any reorder of `per_channel` against the audio — two same-role entries
   swapped, or `channel_map` and `per_channel` permuted together — moves a
   digest off its stream.

Presence and shape of `stream_sha256` are checked with the metadata: a v2
entry must carry it as 64 lowercase hex characters; a v1 entry must not
carry it at all.

The reader checks `format_version` first (1 and 2 are accepted), then
rules 2–6, the metadata half of rule 1 and the digest's presence and shape
before it decodes the audio, then the stream count, then rule 7. An error
names the fields and indices that disagree and never states a cause. Rule 7
names the entry, its input, and which stream its digest does match:

```
read_acsnap: per_channel[0].stream_sha256 (input 7) matches audio.flac stream 2, not 0
read_acsnap: per_channel[0].stream_sha256 (input 7) matches no audio.flac stream
```

Two things are deliberately **not** rules, because the daemon writes both:

- **Roles need not be unique.** Pairs `[[m,r1],[m,r2]]` produce two
  channels with role `"ref"`. Roles are labels; `input_channel` is the key.
- **A pair may use the same input for meas and ref**, e.g. `[[0,0]]`.

**Not detected** (#637), so a passing read is not read as more than it is:

- **Format v1 archives.** A v1 file carries no digest, so rule 7 cannot
  run. A v1 `per_channel` reordered against the audio without changing a
  role — the two `"ref"` entries of a multi-ref session `[[m,r1],[m,r2]]`
  swapped, or `channel_map` permuted with it — still reads, and binds
  calibration and pair identity to the wrong stream. v1 files are read
  exactly as before this check existed, no better and no worse.
- **Writer-side misbinding.** The digest certifies what the writer encoded
  at each position. If the daemon attaches the wrong `input_channel` to a
  stream before encoding, the digest agrees with the wrong claim (#523's
  class, guarded in the daemon).
- **Sample-identical streams.** They share a digest, so a swap between
  their entries passes. It is also harmless: each entry's calibration
  moves with it, onto identical audio.
- **Tampering.** SHA-256 here is a binding check, not integrity
  protection: anyone editing `meta.json` can recompute the digests.

The daemon names roles by first occurrence: walking `pairs` in order, a
channel takes `"meas_<pair index>"` or `"ref"` from the first leg it
appears on and keeps it. Pairs `[[0,1],[1,2]]` therefore name channel 1
`"ref"`. Channels that appear in no pair are named `"ch_<input>"`.

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

`tests/fixtures/snapshot-fixture-v2.acsnap` (repo root) is a checked-in,
synthetic format-v2 `.acsnap` used by `ac-core`'s self-containment test
(`snapshot::tests::t3_checked_in_fixture_reprocesses_with_no_daemon`) and
by `ac-scene`'s display-truth fixtures. Regenerate via:

```
cargo test -p ac-core --lib snapshot::tests::generate_snapshot_fixture -- --ignored
```

`tests/fixtures/snapshot-fixture-v1.acsnap` holds the same samples as a
format-v1 file. It is **byte-frozen**: nothing regenerates it, since
`write_acsnap` writes only v2, and `snapshot::tests::v1_fixture_is_byte_frozen`
pins its sha256. It is the v1 read path's real archive
(`v1_and_v2_fixtures_derive_bit_identical_h1`).
