//! The wire frames the monitor publishes, and the per-tick values every
//! frame in one channel iteration shares.

use serde_json::{json, Value};

use ac_core::shared::calibration::Calibration;

use crate::handlers::send_pub;

use super::channel::{ChannelState, MicCorrection};

/// Emit a `measurement/loudness` sidecar frame for one channel. Kept
/// out of the worker body so the FFT / CWT / CQT / reassigned analysis
/// paths can share it. The frame's `spl_offset_db` mirrors the offset
/// stamped on the spectrum frame for the same channel; `mic_correction`
/// reflects whether the LKFS values were computed on samples that had
/// already passed through the per-channel mic-curve FIR (#104) — `"on"`
/// means LKFS / LRA / dBTP report the *corrected* (true acoustic)
/// levels.
pub(super) fn emit_loudness_frame(
    ch: &ChannelState,
    ctx: &TickCtx,
    mic_correction: &str,
    ts_ns: u64,
    xruns: u32,
) {
    let loudness = &ch.loudness;
    let frame = json!({
        "type":             "measurement/loudness",
        "cmd":              "monitor_spectrum",
        "channel":          ch.channel,
        "n_channels":       ctx.n_channels,
        "sr":               ctx.sr,
        "momentary_lkfs":   json_finite(loudness.momentary()),
        "short_term_lkfs":  json_finite(loudness.short_term()),
        "integrated_lkfs":  json_finite(loudness.integrated()),
        "lra_lu":           loudness.loudness_range(),
        "true_peak_dbtp":   json_finite(loudness.true_peak_dbtp()),
        "gated_duration_s": loudness.gated_duration_s(),
        "spl_offset_db":    ch.spl_offset,
        "mic_correction":   mic_correction,
        "timestamp":        ts_ns,
        "xruns":            xruns,
    });
    send_pub(ctx.pub_tx, "data", &frame);
}

/// Cap on `samples` per scope frame so the wire payload stays bounded
/// regardless of sample rate / tick budget. 2048 f32 = 8 KB per channel
/// per tick; at 192 kHz × 200 ms the per-tick capture is ~38 k samples,
/// so we truncate to the newest 2048 (≈10 ms @ 192 kHz, plenty for
/// trajectory rendering at 60 fps). Visible aliasing is the failure mode
/// to watch for and would prompt a v2 decimator.
pub(super) const SCOPE_MAX_SAMPLES: usize = 2048;

/// Emit a `visualize/scope` sidecar frame for one channel — raw f32
/// samples (no voltage / SPL / mic-curve calibration applied), used by
/// the UI's Goniometer / PhaseScope3D trajectory views (`unified.md`
/// Phase 0b / OQ7). `frame_idx` is the per-tick monotonic counter
/// shared across both channels of a stereo pair; the UI uses it to
/// confirm L and R came from the same capture before pairing them.
pub(super) fn emit_scope_frame(ch: &ChannelState, ctx: &TickCtx, samples: &[f32], xruns: u32) {
    let tail = if samples.len() > SCOPE_MAX_SAMPLES {
        &samples[samples.len() - SCOPE_MAX_SAMPLES..]
    } else {
        samples
    };
    let frame = json!({
        "type":       "visualize/scope",
        "cmd":        "monitor_spectrum",
        "channel":    ch.channel,
        "n_channels": ctx.n_channels,
        "sr":         ctx.sr,
        "frame_idx":  ctx.frame_idx,
        "samples":    tail,
        "timestamp":  ctx.tick_ts_ns,
        "xruns":      xruns,
    });
    send_pub(ctx.pub_tx, "data", &frame);
}

/// Wall-clock nanoseconds since the epoch, for a frame's `timestamp`.
/// Returns 0 if the clock is before the epoch — a frame with a bogus
/// timestamp still beats dropping the frame.
pub(super) fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Wire identity and per-tick values that every frame in one channel
/// iteration shares. Bundled so the ring-path helpers below stay under a
/// readable argument count.
pub(super) struct TickCtx<'a> {
    pub(super) pub_tx: &'a crossbeam_channel::Sender<Vec<u8>>,
    pub(super) n_channels: u32,
    pub(super) sr: u32,
    /// Per-tick monotonic counter; identical for every channel of a tick
    /// so the UI can pair L and R scope frames.
    pub(super) frame_idx: u64,
    /// Tick-wide capture timestamp, for scope frames only.
    pub(super) tick_ts_ns: u64,
    /// Snapshot of the global mic-correction toggle, read once per tick.
    pub(super) mic_corr_enabled: bool,
    /// Capture-block duration for the ring-buffered modes, already
    /// clamped to [16 ms, 100 ms].
    pub(super) tick_secs: f64,
}

/// Mic-correct `mags` in place, then emit the mode's `visualize/*` frame
/// and the channel's `measurement/loudness` sidecar.
///
/// Returns the frame timestamp and mic-correction tag, which the CWT
/// path reuses for its fractional-octave frames so every frame built from
/// one column agrees.
pub(super) fn emit_ring_frames(
    ch: &ChannelState,
    ctx: &TickCtx,
    frame_type: &str,
    freqs: &[f32],
    mags: &mut [f32],
    extra: &[(&str, Value)],
    xruns: u32,
) -> (u64, &'static str) {
    let mc = ch.mic_correction(ctx);
    mc.apply_f32(freqs, mags);
    let mc_tag = mc.tag();
    let ts_ns = now_ns();
    let mags: &[f32] = mags;
    let mut frame = json!({
        "type":           frame_type,
        "cmd":            "monitor_spectrum",
        "channel":        ch.channel,
        "n_channels":     ctx.n_channels,
        "sr":             ctx.sr,
        "magnitudes":     mags,
        "frequencies":    freqs,
        "spl_offset_db":  ch.spl_offset,
        "mic_correction": mc_tag,
        "timestamp":      ts_ns,
        "xruns":          xruns,
    });
    if let Some(obj) = frame.as_object_mut() {
        for (k, v) in extra {
            obj.insert((*k).to_string(), v.clone());
        }
    }
    send_pub(ctx.pub_tx, "data", &frame);
    emit_loudness_frame(ch, ctx, mc_tag, ts_ns, xruns);
    (ts_ns, mc_tag)
}

/// Per-bin dBFS → dBu conversion offset:
///   analog_vrms = sample_peak × cal_in / sqrt(2)   (sine assumption)
///   dBu = dbfs_peak + 20·log10(cal_in / (sqrt(2)·dbu_ref))
///
/// The UI overlays this on hover readouts so any cursor position shows
/// dBFS / dBu / dBV without a round-trip. `None` when the channel has no
/// input voltage calibration.
pub(super) fn dbu_offset_db(cal: Option<&Calibration>) -> Option<f64> {
    cal.and_then(|c| c.vrms_at_0dbfs_in).map(|v| {
        20.0 * (v / (std::f64::consts::SQRT_2 * ac_core::shared::conversions::get_dbu_ref()))
            .log10()
    })
}

/// Aggregate a linear half-spectrum onto the wire's log-spaced columns,
/// splicing the long-N LF spectrum in below `crossover_hz` when one is
/// cached (#142), then apply the channel's mic curve.
///
/// Returns `(columns, column centre frequencies)`. Both the THD path and
/// the fallback path below feed this; they differ only in where their
/// input spectrum came from.
pub(super) fn spectrum_columns(
    spec: &[f64],
    lf: Option<&[f64]>,
    crossover_hz: f32,
    mc: MicCorrection<'_>,
    ctx: &TickCtx,
) -> (Vec<f64>, Vec<f64>) {
    let sr_f = ctx.sr as f64;
    let (mut columns, freqs) = match lf {
        Some(lf) => ac_core::visualize::aggregate::spectrum_to_columns_multiband_wire(
            lf,
            spec,
            sr_f,
            crossover_hz as f64,
            20.0,
            (sr_f / 2.0).max(21.0),
            ac_core::visualize::aggregate::DEFAULT_WIRE_COLUMNS,
        ),
        None => ac_core::visualize::aggregate::spectrum_to_columns_wire(
            spec,
            sr_f,
            20.0,
            (sr_f / 2.0).max(21.0),
            ac_core::visualize::aggregate::DEFAULT_WIRE_COLUMNS,
        ),
    };
    mc.apply_f64(&freqs, &mut columns);
    (columns, freqs)
}

/// Convert a possibly-infinite `f64` to JSON — `null` when not finite,
/// real number otherwise. Keeps the sidecar frame JSON-parseable; `-inf`
/// would otherwise fail `serde_json`'s finite-value check.
pub(super) fn json_finite(v: f64) -> Value {
    if v.is_finite() {
        json!(v)
    } else {
        Value::Null
    }
}
