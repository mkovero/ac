//! One τ reading: play a short ESS, deconvolve, and report where the
//! linear-IR peak sits — or refuse, when the peak is close enough to the
//! window edge that "outside the window" and "at this position" are the
//! same picture (#340).
//!
//! Everything about the *window* lives here — its size in time, the
//! edge margin, and the rig-instrument overrides for both. Whether one
//! reading counts as a measurement is a separate question, answered by
//! the parent module.

use ac_core::measurement::sweep::{
    deconvolve_full, extract_irs, inverse_sweep, ir_peak, log_sweep, pre_impulse_snr_db,
    SweepParams,
};

use crate::audio::AudioEngine;

/// Short-ESS parameters for the τ measurement (#281). Deliberately much
/// shorter than a `plot_ir` sweep — this only has to locate the linear-IR
/// peak, not resolve harmonics or a decay tail, so `n_harmonics = 1` on
/// the `extract_irs` call below. That matters for the window bound: with
/// `n_harmonics == 1` there is no neighbouring order, so
/// `per_order_window_lens`'s harmonic-gap clamp (`sweep/harmonics.rs`) never runs —
/// it does not bind here, only on multi-harmonic callers like `plot_ir`.
///
/// The bound that actually fires is the requested window itself, so it
/// must be sized directly to the round trip this needs to measure rather
/// than left as a fixed sample count (#340): `TAU_WINDOW_LEN` used to be a
/// `usize`, so its *time* value shrank as sample rate rose — 85 ms at
/// 48 kHz (looked generous) but only 42.67 ms at 96 kHz, 1.03× short of
/// this rig's measured 43.75 ms τ, and wrong at every rate in between.
/// `TAU_MIN_HALF_WINDOW_S` fixes that: it is a *time* bound, converted to
/// a sample window at measurement time, so the measurable ceiling is the
/// same 50 ms at every sample rate. `TAU_TAIL_S` (150 ms) stays well
/// above it so the capture always holds the whole window regardless of
/// sample rate.
const TAU_F1_HZ: f64 = 100.0;
const TAU_DURATION_S: f64 = 0.2;
const TAU_TAIL_S: f64 = 0.15;
/// Half-width of the τ measurement window, in seconds (AC5 of #340). The
/// largest round trip `measure_tau` can report is just under this value,
/// less [`TAU_EDGE_MARGIN_FRAC`].
///
/// Provenance: measured (#350). The value came from #340's own number;
/// the 2026-08-22 rig session (`work/rig/rig-2026-08-22-tau-window-350-results.md`,
/// electrical loopback, 96 kHz, period 1024) resized this window around a
/// fixed round trip and located τ exactly at every size tested. Nothing
/// measured argued for a change.
///
/// Policy: if the ceiling binds, raise *this* constant — there is headroom
/// to 75 ms (`TAU_TAIL_S` / 2, enforced by
/// `tau_tail_s_clears_tau_min_half_window_s_with_margin`) — and never lower
/// the margin to buy range. The margin, together with the `<=` edge test in
/// `check_peak_within_window`, is what classifies an arrival past the far
/// edge as an edge refusal: just past it (≲0.5 ms at 96 kHz) the peak pins
/// at the last sample, and further out `ir_peak` picks a skirt peak inside
/// the margin (see [`TAU_EDGE_MARGIN_FRAC`]). Since #494 the edge check is
/// judged first and carries the SNR observation with it, so a skirt peak
/// that also fails [`TAU_SNR_THRESHOLD_DB`] reports both observations
/// instead of a noise-only reason. That classification is bounded
/// (synthetic probe on #494, 96 kHz): noiseless, every arrival up to
/// 14 000 samples past the edge put the peak inside the margin; with white
/// noise up to −20 dBFS, only arrivals up to 400 samples past did. Further
/// out under noise the in-window IR is noise and residue, its argmax lands
/// anywhere, and `peak SNR below threshold` alone is the honest reading.
const TAU_MIN_HALF_WINDOW_S: f64 = 0.05;
/// Fraction of the half-window treated as "too close to the edge to
/// trust" (AC4 of #340). A peak this close to either edge is
/// indistinguishable from one pinned by an arrival outside the window
/// entirely, so it is refused rather than reported.
///
/// When #340 introduced it, this was not derived from rig data and was
/// expected to need revisiting against real noise floors. That
/// measurement has since been made (#350): the 2026-08-22 rig session
/// (`work/rig/rig-2026-08-22-tau-window-350-results.md`, electrical
/// loopback, 96 kHz, period 1024) located τ exactly down to an edge
/// clearance of f ≈ 0.01, twice, and `peak_abs` held constant to 5 s.f.
/// across 84 readings. Its peak-to-floor range was 33.8–83.5 dB, where
/// the statistic is peak over max|x| of the leading eighth of the window
/// (the `19604fc` probe) — *not* the [`TAU_SNR_THRESHOLD_DB`] gate
/// statistic, which on this stimulus sits at ≈26–28.5 dB for any clean
/// reading (#471).
///
/// So 0.10 is a measured-safe value, not a derived optimum. Its cost is
/// range: at 96 kHz it lowers the accepted ceiling from ≈50 ms to 44.98 ms
/// (4318 samples; the rig record rounds these to 50.01 → 44.99 ms). What
/// keeps it from going lower is derived, not measured (synthetic, 96 kHz
/// only; pinned by the synthetic `tau_*` edge tests in `mod tests`): an
/// arrival ~80–90 samples past the edge does not pin, `ir_peak` picks a
/// skirt peak ≈1.5–2.3 ms *inside* the edge, and the SNR gate fails it by
/// as little as ≈1.2 dB (1.6 dB in the noiseless test). The margin is what
/// classifies that skirt as an edge observation — the edge check runs
/// first (#494), and the refusal also reports the failed gate. Below
/// ≈0.046 the margin stops covering the skirt: the gate still refuses it,
/// but only as `peak SNR below threshold`, which sends the operator to the
/// cable and levels instead of the window and the latency.
///
/// Still unmeasured: hardware with capture noise approaching the stimulus
/// level, which an electrical loopback does not reach without deliberate
/// injection. In synthesis, rising noise only moved outcomes from accept or
/// edge refusal to low-SNR refusal — no noise level produced an accepted,
/// off-reference value — so the error that gap can hide points toward
/// false refusal, not false acceptance.
const TAU_EDGE_MARGIN_FRAC: f64 = 0.10;

/// Minimum pre-impulse SNR (dB) a τ lifecycle's deconvolved peak must clear
/// before the reading is trusted at all (#368). This replaces the old
/// pre-attempt `is_loopback` gate, which keyed on a *captured level*
/// against a unity-gain expectation — a proxy that a hot cable (3.01 dB
/// over unity) or a low-gain cable (4.19 dB under) both fail even though
/// both carry a perfectly real, measurable arrival, and that a loud but
/// uncorrelated interferer could still pass. This checks the quantity that
/// actually distinguishes "patched" from "not patched": whether the
/// deconvolution the τ sweep produced finds a peak that stands clear of its
/// own pre-impulse noise floor, measured under the exact drive and gain
/// conditions τ was measured under.
///
/// Provenance: derived, not measured on this exact sweep. Two rig sessions
/// anchor it from different contexts —
/// `work/rig/rig-2026-08-22-tau-window-350-results.md` measured real
/// electrical-loopback τ SNR at 33.8–83.5 dB (the low end a JACK-startup-
/// transient artefact on the first reading after engine start, not a true
/// floor), where that figure is peak over max|x| of the window's leading
/// eighth, not this gate's `pre_impulse_snr_db` statistic (#350); #376's
/// rig session measured a deconvolution noise cliff at ~16 dB pre-impulse
/// SNR on an unrelated (long-ESS, acoustic) path. 24 dB
/// splits that gap, rounded toward the reject side rather than the
/// midpoint — a false accept (a spurious peak silently stored in
/// `tau_history`) is more expensive than a false refuse (operator sees
/// "not measured" and re-runs). Wired through the same `tau-window-
/// override` env-override mechanism as `TAU_EDGE_MARGIN_FRAC` so a rig
/// session can correct it without a rebuild.
///
/// Not to be confused with `report::ir_stats`'s `PRE_IMPULSE_SNR_MIN_DB`
/// (18.0 dB, #376): that one gates a long-ESS *acoustic* capture's IR
/// read-out, this one gates a short-ESS *electrical* τ lifecycle. Same
/// quantity (`sweep::pre_impulse_snr_db`), different path, different
/// evidence — which is why they are two constants and not one.
pub(super) const TAU_SNR_THRESHOLD_DB: f64 = 24.0;

/// How far below its own stimulus's noiseless floor a *reference* leg's peak
/// may sit before the reading is refused (#471).
///
/// [`TAU_SNR_THRESHOLD_DB`] above is a constant because `calibrate` fixes its
/// own stimulus. The #460 reference leg does not: it carries whatever sweep the
/// operator asked `plot ir` for, and the pre-impulse figure is a property of
/// that sweep rather than of the capture's noise. Measured on pupu 2026-09-16
/// and reproduced synthetically within 0.5 dB: white noise from −120 through
/// −20 dBFS moves it by 0.0 dB, a route attenuated 40 dB reads like a good
/// cable, and the value ranges 17.2 dB (20–20000 Hz) to 35.1 dB (500–4000 Hz).
/// So the reference gate compares against
/// [`ac_core::measurement::sweep::pre_impulse_snr_floor_db`] for the sweep in
/// hand, and this is the only free parameter left.
///
/// Provenance: derived. The shipped 24.0 dB sits 2.8 dB under `calibrate`'s own
/// ESS floor of 26.8 dB, so 3 dB is that same allowance rounded toward the
/// reject side. It leaves at least 3.6 dB of separation from a disconnected
/// input on every characterised shape (the no-cable reading is a noise draw,
/// ~10–17 dB depending on the seed), and it is six times the 0.5 dB spread
/// between the rig's measured readings and their synthetic floors. Wired
/// through the same `tau-window-override` env mechanism as the other two
/// constants so a rig session can widen it without a rebuild.
///
/// What this gate still cannot do, stated so it is not re-derived: it does not
/// detect wrong or attenuated routing. Peak and deconvolution residue scale
/// together, so a 40 dB-down path reads exactly like a correct one. Routing is
/// caught by port resolution (#225) and the level read-out.
pub(super) const REF_SNR_MARGIN_DB: f64 = 3.0;

/// Rig-instrument overrides for the two τ window constants (#350).
///
/// Compiled in only under the `tau-window-override` feature, which is off
/// by default, so a production daemon cannot be perturbed by its
/// environment. The rig needs them because the only lever hardware has on
/// edge proximity is τ itself, and τ moves in period-sized steps
/// (44.5 %, 33.8 %, 12.5 %, then off the end of the window) — there is no
/// way to sample between 0 % and the shipped 10 % margin that way. Moving
/// the *window* while the round trip stays fixed samples it continuously,
/// inside one JACK client lifetime, which also keeps #347's one-period
/// jump out of the comparison.
#[cfg(feature = "tau-window-override")]
fn tau_env_f64(key: &str, default: f64) -> f64 {
    let raw = match std::env::var(key) {
        Ok(v) => v,
        Err(_) => return default,
    };
    match raw.trim().parse::<f64>() {
        Ok(x) if x.is_finite() && x >= 0.0 => x,
        _ => {
            eprintln!(
                "calibrate: {key}={raw:?} is not a finite non-negative number —                  ignoring it and using {default}"
            );
            default
        }
    }
}

#[cfg(feature = "tau-window-override")]
fn tau_half_window_s() -> f64 {
    tau_env_f64("AC_TAU_HALF_WINDOW_S", TAU_MIN_HALF_WINDOW_S)
}

#[cfg(not(feature = "tau-window-override"))]
fn tau_half_window_s() -> f64 {
    TAU_MIN_HALF_WINDOW_S
}

#[cfg(feature = "tau-window-override")]
fn tau_edge_margin_frac() -> f64 {
    tau_env_f64("AC_TAU_EDGE_MARGIN_FRAC", TAU_EDGE_MARGIN_FRAC)
}

#[cfg(not(feature = "tau-window-override"))]
fn tau_edge_margin_frac() -> f64 {
    TAU_EDGE_MARGIN_FRAC
}

#[cfg(feature = "tau-window-override")]
pub(super) fn tau_snr_threshold_db() -> f64 {
    tau_env_f64("AC_TAU_SNR_THRESHOLD_DB", TAU_SNR_THRESHOLD_DB)
}

#[cfg(not(feature = "tau-window-override"))]
pub(super) fn tau_snr_threshold_db() -> f64 {
    TAU_SNR_THRESHOLD_DB
}

#[cfg(feature = "tau-window-override")]
pub(crate) fn ref_snr_margin_db() -> f64 {
    tau_env_f64("AC_REF_SNR_MARGIN_DB", REF_SNR_MARGIN_DB)
}

#[cfg(not(feature = "tau-window-override"))]
pub(crate) fn ref_snr_margin_db() -> f64 {
    REF_SNR_MARGIN_DB
}

/// Which threshold a τ reading's SNR is judged against (#471).
///
/// One [`analyse_tau_leg`], two policies — `calibrate` fixes its stimulus and
/// keeps the constant; the #460 reference leg derives its threshold from the
/// sweep it actually carried. Splitting the *function* instead would have let
/// the two definitions of "a τ reading" drift, which #460 exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SnrGate {
    /// [`TAU_SNR_THRESHOLD_DB`], possibly overridden on a rig.
    Constant,
    /// This stimulus's own noiseless floor, less `margin_db`. Falls back to
    /// [`SnrGate::Constant`] when no floor can be established, so the gate can
    /// never become unclearable.
    DerivedFloor { margin_db: f64 },
}

/// One reference or calibration leg, analysed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TauLegReading {
    pub(crate) tau_s: f64,
    pub(crate) snr_db: f64,
    /// The derived floor this reading was judged against, when one applied —
    /// carried out so the caller can archive it without recomputing it
    /// (#471 schema v8).
    pub(crate) snr_floor_db: Option<f64>,
}

/// Per-reading τ diagnostic (#350). `snr_db` is the real gate value —
/// `sweep::pre_impulse_snr_db` on this same peak, computed once by the
/// caller and passed in rather than recomputed here (#368: this used to
/// carry its own separate, leading-eighth-window SNR calculation, which
/// became a second implementation of "is this peak real" once the actual
/// gate needed the same number). `floor`/`far_end` below are a distinct,
/// unrelated diagnostic — max |x| over the leading eighth of the window,
/// defined exactly as `it_loopback_ir` and `ir_probe` define it, so those
/// numbers still compare directly against #277's record.
#[cfg(feature = "tau-window-override")]
fn tau_probe_log(
    ir: &[f64],
    peak_idx: usize,
    peak_abs: f64,
    window_len: usize,
    half: usize,
    sr: u32,
    snr_db: f64,
) {
    let far_end = (ir.len() / 8).max(1);
    let floor = ir[..far_end]
        .iter()
        .map(|v| v.abs())
        .fold(0.0_f64, f64::max);
    let margin_frac = tau_edge_margin_frac();
    let margin = (margin_frac * half as f64).round() as usize;
    let dist_from_end = window_len.saturating_sub(1).saturating_sub(peak_idx);
    let edge_frac = dist_from_end as f64 / half as f64;
    let offset = peak_idx as i64 - half as i64;
    eprintln!("--- tau probe (#350) ---");
    eprintln!("sample_rate:   {sr} Hz");
    eprintln!(
        "half_window:   {half} samples = {:.4} ms",
        half as f64 * 1000.0 / sr as f64
    );
    eprintln!("window_len:    {window_len} samples");
    eprintln!("peak_index:    {peak_idx}");
    eprintln!("peak_abs:      {peak_abs:.6e}");
    eprintln!("floor_abs:     {floor:.6e}  (max |x| over leading {far_end} samples)");
    eprintln!("snr_db:        {snr_db:.2}");
    eprintln!("dist_from_end: {dist_from_end} samples");
    eprintln!("edge_frac:     {edge_frac:.4}  (margin_frac {margin_frac} = {margin} samples)");
    eprintln!(
        "tau:           {offset:+} samples = {:+.4} ms",
        offset as f64 * 1000.0 / sr as f64
    );
    eprintln!("------------------------");
}

/// Distinguishes a τ lifecycle's low-SNR refusal ([`check_peak_snr`]) from
/// a genuine measurement failure (#368), so `measure_tau_twice` can report
/// a distinct `cal_done.tau_state` (`"not_measured_low_snr"`) instead of
/// folding it into the generic `"error"` state a real engine/deconvolution
/// failure produces. Carried as a typed `anyhow::Error` payload,
/// downcast-recovered by `measure_tau_twice`, rather than a string match on
/// the message — a message wording change must not silently break the
/// state split.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LowSnrRefusal {
    pub(crate) snr_db: f64,
    pub(crate) threshold_db: f64,
}

impl std::fmt::Display for LowSnrRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "\u{3c4} peak pre-impulse SNR {:.2} dB is below the {:.2} dB threshold; no value \
             reported",
            self.snr_db, self.threshold_db
        )
    }
}

impl std::error::Error for LowSnrRefusal {}

/// The SNR observation an [`EdgeRefusal`] carries (#494): the refused
/// peak's pre-impulse SNR, the threshold it was judged against, and whether
/// it fell below — computed here, by [`snr_below_threshold`], so no reader
/// of the refusal re-derives the gate's comparison.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct EdgeSnr {
    pub(crate) snr_db: f64,
    pub(crate) threshold_db: f64,
    pub(crate) below_threshold: bool,
}

/// A τ peak within the edge margin of its window (#340), typed so a
/// same-capture reference reading (#460) can say "peak at reference window
/// edge" without matching message text, and so `calibrate` can report it as
/// its own `tau_state` with numeric fields (#494).
///
/// `snr` is the same peak's SNR observation, attached by [`analyse_tau_leg`]
/// because the edge check is judged first: a peak that fails both gates is
/// reported as an edge refusal *with* its failed SNR, never as a low-SNR
/// refusal that drops the edge observation. `None` when the lifecycle
/// crossed an xrun, whose SNR is not evaluated (#368/#369), and on the bare
/// result of [`check_peak_within_window`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct EdgeRefusal {
    pub(crate) peak_idx: usize,
    pub(crate) window_len: usize,
    pub(crate) margin: usize,
    pub(crate) snr: Option<EdgeSnr>,
}

impl EdgeRefusal {
    fn half(&self) -> i64 {
        (self.window_len / 2) as i64
    }

    /// The peak's offset from the window centre, in samples — the value
    /// that would have been the τ, had it been accepted.
    pub(crate) fn peak_offset_samples(&self) -> i64 {
        self.peak_idx as i64 - self.half()
    }

    /// The window's first sample, as an offset from its centre.
    pub(crate) fn window_first_offset_samples(&self) -> i64 {
        -self.half()
    }

    /// The window's last sample, as an offset from its centre. One less in
    /// magnitude than [`Self::window_first_offset_samples`]: the window is
    /// asymmetric by one sample, and a pinned peak sits exactly here.
    pub(crate) fn window_last_offset_samples(&self) -> i64 {
        self.window_len as i64 - 1 - self.half()
    }
}

impl std::fmt::Display for EdgeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "\u{3c4} peak at offset {:+} samples is within the {}-sample edge margin of a {} to \
             {:+} sample window; no value reported",
            self.peak_offset_samples(),
            self.margin,
            self.window_first_offset_samples(),
            self.window_last_offset_samples()
        )
    }
}

impl std::error::Error for EdgeRefusal {}

/// The capture tail cannot hold the τ window. Typed for the same reason as
/// [`EdgeRefusal`]: a same-capture reference (#460) reads `plot_ir`'s own
/// tail, which the operator sets, so this case is reachable there and needs
/// its own operator-facing reason.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TailTooShort {
    pub(crate) half_window_s: f64,
    pub(crate) tail_s: f64,
    pub(crate) needed_s: f64,
}

impl std::fmt::Display for TailTooShort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "\u{3c4} half-window {} s needs a {:.4} s gate but the capture tail is only {} s \
             \u{2014} the window would run off the end of the capture",
            self.half_window_s, self.needed_s, self.tail_s
        )
    }
}

impl std::error::Error for TailTooShort {}

/// Refuse a τ lifecycle whose deconvolved peak sits below `threshold_db`
/// pre-impulse SNR — the peak cannot be trusted as a real arrival rather
/// than noise (#368, replacing the old pre-attempt `is_loopback` level
/// gate). Modeled on [`check_peak_within_window`]'s shape — a small pure
/// function over already-computed values, unit-testable without an
/// `AudioEngine`.
///
/// Called *after* the edge check in [`analyse_tau_leg`] (#494). #368 ran it
/// first, on the argument that a peak which isn't real shouldn't be judged
/// against the edge margin at all. That argument fails on the case the
/// margin exists for: an arrival past the far edge leaves a skirt peak
/// inside the margin whose SNR sits a dB or two under the gate, so the
/// SNR-first order reported a clean but out-of-window loopback as noise.
/// A skirt peak and a noise argmax that lands in the margin cannot be told
/// apart from these observations, so the edge refusal reports both and
/// asserts neither cause; this gate alone refuses only a peak clear of the
/// margin.
///
/// Skipped by `measure_tau` entirely when this lifecycle's own capture
/// crossed an xrun (#368/#369 merge precedence) — see its doc comment.
fn check_peak_snr(snr_db: f64, threshold_db: f64) -> anyhow::Result<()> {
    if snr_below_threshold(snr_db, threshold_db) {
        return Err(LowSnrRefusal {
            snr_db,
            threshold_db,
        }
        .into());
    }
    Ok(())
}

/// The SNR gate's one comparison, shared by [`check_peak_snr`] and the
/// [`EdgeSnr`] flag, so the two cannot disagree about the same peak.
fn snr_below_threshold(snr_db: f64, threshold_db: f64) -> bool {
    snr_db < threshold_db
}

/// Refuse a peak sitting within `margin_frac` of the half-window of
/// either edge of a `window_len`-sample gate. Pulled out of `measure_tau`
/// so the edge case can be driven directly in tests without an
/// `AudioEngine` (#340 AC4). The refusal carries no SNR observation;
/// [`analyse_tau_leg`] attaches it.
fn check_peak_within_window(
    peak_idx: usize,
    window_len: usize,
    margin_frac: f64,
) -> Result<(), EdgeRefusal> {
    let half = window_len / 2;
    let margin = (margin_frac * half as f64).round() as usize;
    let dist_from_start = peak_idx;
    let dist_from_end = window_len.saturating_sub(1).saturating_sub(peak_idx);
    if dist_from_start <= margin || dist_from_end <= margin {
        return Err(EdgeRefusal {
            peak_idx,
            window_len,
            margin,
            snr: None,
        });
    }
    Ok(())
}

/// Play a short ESS, deconvolve it, and return the interface round-trip
/// delay in seconds (peak of the linear IR, converted from samples)
/// alongside that peak's pre-impulse SNR in dB (#368) and the xrun count
/// `AudioEngine::xruns()` reported across the `play_and_capture` call
/// specifically (#369) — the caller needs both even on success, since
/// `cal_done` reports the SNR on every state that reached deconvolution
/// (not only a refusal) and the xrun count on every state where both
/// lifecycles ran.
///
/// A capture that crossed an xrun skips the SNR gate entirely
/// (#368/#369 merge precedence): its SNR figure is meaningless — the
/// contamination, not the noise floor it produced, is what `tau_result`
/// reports (`refused_xrun`) once both lifecycles are in. A lifecycle
/// with no xrun has both gates judged on the same peak, edge first (#494):
/// see [`analyse_tau_leg`].
///
/// Reuses the Farina machinery from `ac_core::measurement::sweep` exactly
/// as `plot_ir` does — see `handlers/audio/plot.rs` for the longer-form
/// version of the same technique.
/// `calibrate`'s own τ stimulus, as one definition.
///
/// Extracted from [`measure_tau`] because #471 made the *shape* of this sweep
/// load-bearing outside the measurement itself: [`REF_SNR_MARGIN_DB`]'s
/// provenance is that this stimulus's noiseless floor, less that margin,
/// reproduces [`TAU_SNR_THRESHOLD_DB`]. A test asserts that, and it has to
/// judge the sweep the daemon actually plays — a second copy of the
/// expression here would keep passing after someone edited the first.
fn tau_sweep_params(sample_rate: u32) -> SweepParams {
    SweepParams {
        f1_hz: TAU_F1_HZ,
        // Nyquist-limited, capped at the top of the audio band.
        f2_hz: (sample_rate as f64 * 0.45).min(20_000.0),
        duration_s: TAU_DURATION_S,
        sample_rate,
    }
}

pub(super) fn measure_tau(eng: &mut dyn AudioEngine, amp: f64) -> anyhow::Result<(f64, f64, u32)> {
    let sr = eng.sample_rate();
    let params = tau_sweep_params(sr);
    let sweep = log_sweep(&params)?;
    let amp = amp as f32;
    let scaled: Vec<f32> = sweep.iter().map(|&s| s * amp).collect();
    let xruns_before = eng.xruns();
    let captured = eng.play_and_capture(&scaled, TAU_TAIL_S)?;
    let xruns = eng.xruns().saturating_sub(xruns_before);
    let TauLegReading { tau_s, snr_db, .. } =
        analyse_tau_leg(&captured, &params, TAU_TAIL_S, xruns, SnrGate::Constant)?;
    Ok((tau_s, snr_db, xruns))
}

/// Analyse one captured τ leg: deconvolve with `params`' inverse sweep, find
/// the linear-IR peak inside a `2 × TAU_MIN_HALF_WINDOW_S` window, and apply
/// the single-reading gates — capture tail long enough to hold the window,
/// the window-edge margin, and pre-impulse SNR (skipped when `xruns > 0`,
/// per the #368/#369 merge precedence). Returns the round trip in seconds
/// and the peak's pre-impulse SNR in dB.
///
/// Both peak gates are evaluated on the one peak, and the edge refusal wins
/// (#494): it carries the SNR observation, so a peak failing both is
/// reported with both, and [`LowSnrRefusal`] is returned only for a peak
/// clear of the margin. See [`check_peak_snr`] for why the #368 order was
/// reversed.
///
/// Split out of [`measure_tau`] (#460) so a same-capture reference leg in
/// `plot_ir` is judged by exactly the gates `calibrate` applies — one
/// definition of what a τ reading is, not two that can drift. Refusals are
/// typed ([`LowSnrRefusal`], [`EdgeRefusal`], [`TailTooShort`]) so each caller
/// can phrase its own reason without matching message text.
///
/// The peak itself comes from [`ac_core::measurement::sweep::ir_peak`]
/// (#351) — the same picker `MeasurementReport::ir_stats` uses for an IR's
/// arrival, rather than a separate inline maximum. Before #351 this used a
/// `max_by` that kept the *latest* index on a tie and panicked on NaN,
/// while `ir_stats`'s picker kept the earliest and skipped NaN; the two
/// halves of `ir_arrival_distance()`'s subtraction now share one rule
/// structurally, not by coincidence of how each was written.
pub(crate) fn analyse_tau_leg(
    captured: &[f32],
    params: &SweepParams,
    tail_s: f64,
    xruns: u32,
    gate: SnrGate,
) -> anyhow::Result<TauLegReading> {
    let sr = params.sample_rate;
    let half_window_s = tau_half_window_s();
    if 2.0 * half_window_s > tail_s {
        return Err(TailTooShort {
            half_window_s,
            tail_s,
            needed_s: 2.0 * half_window_s,
        }
        .into());
    }
    let inv = inverse_sweep(params)?;
    let full = deconvolve_full(captured, &inv);
    let half = (half_window_s * sr as f64).ceil() as usize;
    let window_len = 2 * half;
    let irs = extract_irs(&full, params, 1, window_len)?;
    if irs.linear.is_empty() {
        // `ir_peak` returns `(0, 0.0)` on an empty slice, which would
        // otherwise read as a legitimate zero-index peak (#351) — refuse
        // explicitly before it can become a τ.
        return Err(anyhow::anyhow!("empty IR from τ sweep"));
    }
    let (peak_idx, peak_val) = ir_peak(&irs.linear);
    let snr_db = pre_impulse_snr_db(&irs.linear, peak_idx);
    #[cfg(feature = "tau-window-override")]
    tau_probe_log(
        &irs.linear,
        peak_idx,
        peak_val.abs(),
        window_len,
        half,
        sr,
        snr_db,
    );
    #[cfg(not(feature = "tau-window-override"))]
    let _ = peak_val;
    // #471: the reference leg's threshold comes from its own stimulus. A
    // non-finite floor (guard band eats the pre-peak region — reachable on a
    // noise-only leg that argmaxes near index 0) falls back to the constant
    // rather than producing a gate no reading could clear.
    let snr_floor_db = match gate {
        SnrGate::Constant => None,
        SnrGate::DerivedFloor { .. } => {
            ac_core::measurement::sweep::pre_impulse_snr_floor_db(params, window_len, peak_idx)
        }
    };
    let threshold_db = match (gate, snr_floor_db) {
        (SnrGate::DerivedFloor { margin_db }, Some(floor)) => floor - margin_db,
        _ => tau_snr_threshold_db(),
    };
    // #368/#369 merge precedence: a lifecycle that crossed an xrun skips
    // its own SNR gate — that reading's SNR is not evaluated at all, and
    // `tau_result` reports `refused_xrun` for the run once both lifecycles
    // are in, regardless of what this figure would have said.
    let snr_observation = (xruns == 0).then(|| EdgeSnr {
        snr_db,
        threshold_db,
        below_threshold: snr_below_threshold(snr_db, threshold_db),
    });
    // #494: edge first, carrying the SNR observation, so a peak that fails
    // both gates never loses its edge observation to the SNR refusal.
    if let Err(mut edge) = check_peak_within_window(peak_idx, window_len, tau_edge_margin_frac()) {
        edge.snr = snr_observation;
        return Err(edge.into());
    }
    if xruns == 0 {
        check_peak_snr(snr_db, threshold_db)?;
    }
    let offset_samples = peak_idx as i64 - half as i64;
    Ok(TauLegReading {
        tau_s: offset_samples as f64 / sr as f64,
        snr_db,
        snr_floor_db,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #340 AC4/AC-test: a peak pinned at the window's far edge — exactly
    /// the shape #277 measured — must be refused, not converted into the
    /// plausible-looking offset the old, unguarded code would have
    /// returned. Compute that pinned value here, the way `measure_tau`
    /// used to unconditionally, and confirm the guard fires before it
    /// would ever reach the caller.
    #[test]
    fn check_peak_within_window_refuses_peak_pinned_at_edge() {
        let window_len = 9600; // 2 * 4800, i.e. a 50 ms half-window @ 96 kHz
        let half = window_len / 2;
        let peak_idx = window_len - 1; // pinned against the far edge
        let pinned_offset_samples = peak_idx as i64 - half as i64; // what the old code returned
        assert_eq!(pinned_offset_samples, half as i64 - 1);

        let result = check_peak_within_window(peak_idx, window_len, TAU_EDGE_MARGIN_FRAC);
        assert!(
            result.is_err(),
            "peak pinned at the window edge must be refused, not silently reported as offset {pinned_offset_samples}"
        );
    }

    /// A peak exactly `margin_frac` of the half-window from the start edge
    /// is refused — the margin boundary itself counts as "too close", per
    /// the architect note's instruction to test the boundary directly.
    #[test]
    fn check_peak_within_window_refuses_at_margin_boundary() {
        let window_len = 100;
        let half = window_len / 2;
        let margin = (TAU_EDGE_MARGIN_FRAC * half as f64).round() as usize;

        assert!(check_peak_within_window(margin, window_len, TAU_EDGE_MARGIN_FRAC).is_err());
        assert!(check_peak_within_window(
            window_len - 1 - margin,
            window_len,
            TAU_EDGE_MARGIN_FRAC
        )
        .is_err());
    }

    /// A peak safely inside the window, away from either edge, is
    /// accepted — the guard must not refuse the ordinary, correctly
    /// measured case.
    #[test]
    fn check_peak_within_window_accepts_interior_peak() {
        let window_len = 9600;
        let half = window_len / 2;
        assert!(check_peak_within_window(half, window_len, TAU_EDGE_MARGIN_FRAC).is_ok());
    }

    /// One sample outside the margin boundary must be accepted — pairs
    /// with `check_peak_within_window_refuses_at_margin_boundary` to pin
    /// the guard as off-by-zero (refuses exactly at the boundary, accepts
    /// exactly past it) rather than off-by-one in either direction.
    #[test]
    fn check_peak_within_window_accepts_one_sample_past_margin_boundary() {
        let window_len = 100;
        let half = window_len / 2;
        let margin = (TAU_EDGE_MARGIN_FRAC * half as f64).round() as usize;

        assert!(check_peak_within_window(margin + 1, window_len, TAU_EDGE_MARGIN_FRAC).is_ok());
        assert!(check_peak_within_window(
            window_len - 1 - margin - 1,
            window_len,
            TAU_EDGE_MARGIN_FRAC
        )
        .is_ok());
    }

    /// #340's own motivating number: the rig's measured 43.75 ms round trip
    /// (4200 samples at 96 kHz) must clear the edge guard, not just a
    /// dead-centre synthetic peak. Locks in the margin the fix actually
    /// buys at the operating point that motivated the issue (QA correctness
    /// issue 2).
    #[test]
    fn check_peak_within_window_accepts_the_rigs_measured_round_trip() {
        let window_len = 9600; // 50 ms half-window @ 96 kHz
        let half = window_len / 2;
        let offset_samples = 4200i64; // rig's measured tau, 96 kHz (#340/#277)
        let peak_idx = (half as i64 + offset_samples) as usize;
        assert!(
            check_peak_within_window(peak_idx, window_len, TAU_EDGE_MARGIN_FRAC).is_ok(),
            "the rig's own measured round trip must be inside the accepted window"
        );
    }

    /// Coupled-constant guard (QA correctness issue 1): `TAU_TAIL_S` must
    /// stay ahead of `TAU_MIN_HALF_WINDOW_S` with margin, or the capture
    /// can no longer hold the whole gate at high sample rates. Fails on a
    /// wrong pair (constants inverted) and on an unscored gap (no
    /// headroom) rather than passing silently either way.
    // Both bounds below compare two `const`s, so clippy sees a
    // compile-time-knowable value and suggests a `const {}` assertion
    // block instead. That would turn this into a build-time check instead
    // of a `cargo test` result — deliberately kept as a runtime test (the
    // shape the coupled-constants rule asks for) so it shows up in test
    // output like the rest of the suite, not just as a build failure.
    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn tau_tail_s_clears_tau_min_half_window_s_with_margin() {
        assert!(
            TAU_TAIL_S > TAU_MIN_HALF_WINDOW_S,
            "capture tail ({TAU_TAIL_S}s) must exceed the half-window \
             ({TAU_MIN_HALF_WINDOW_S}s) or the gate can run off the end of the capture"
        );
        // Headroom bound: the doc comment on TAU_TAIL_S claims it "stays
        // well above" the half-window — fail if a future edit erodes that
        // below the 2x this test treats as the floor for "well above" (a
        // future edit to either constant must re-justify the number, not
        // silently drift under it).
        assert!(
            TAU_TAIL_S >= 2.0 * TAU_MIN_HALF_WINDOW_S,
            "tail no longer clears the half-window with the margin the doc comment assumed"
        );
    }

    /// The rig override parser (#350) accepts a plain number and refuses
    /// anything else — a typo must not silently become a different window
    /// than the one the run sheet says was used, which would make the
    /// recorded edge fraction wrong rather than missing. Uses keys no
    /// other test reads so the process-wide environment stays shared-safe.
    #[cfg(feature = "tau-window-override")]
    #[test]
    fn tau_env_override_parses_or_falls_back_to_the_compiled_constant() {
        assert_eq!(
            tau_env_f64("AC_TAU_TEST_UNSET_KEY_350", 0.05),
            0.05,
            "an unset variable must leave the compiled-in constant in place"
        );
        std::env::set_var("AC_TAU_TEST_GOOD_KEY_350", " 0.04862 ");
        assert_eq!(tau_env_f64("AC_TAU_TEST_GOOD_KEY_350", 0.05), 0.04862);
        for bad in ["", "48.62ms", "-0.01", "nan", "inf"] {
            std::env::set_var("AC_TAU_TEST_BAD_KEY_350", bad);
            assert_eq!(
                tau_env_f64("AC_TAU_TEST_BAD_KEY_350", 0.05),
                0.05,
                "{bad:?} is not a usable window and must fall back, not be coerced"
            );
        }
    }

    /// `calibrate`'s own τ stimulus and window at `sr`, as `measure_tau`
    /// builds them: sweep parameters, half-window and window length in
    /// samples.
    fn synthetic_tau_window(sr: u32) -> (SweepParams, usize, usize) {
        let params = tau_sweep_params(sr);
        let half = (tau_half_window_s() * sr as f64).ceil() as usize;
        (params, half, 2 * half)
    }

    /// A noiseless synthetic `calibrate` capture (#350): the τ sweep at the
    /// −30 dBFS-ish amplitude the rig drives (0.03), arriving `delay`
    /// samples late, padded to the length `play_and_capture` returns
    /// (sweep plus `TAU_TAIL_S`). Noiseless so the edge tests below are
    /// deterministic — the gate statistic on this stimulus is set by
    /// deconvolution residue, not capture noise (#471).
    fn synthetic_tau_capture(params: &SweepParams, delay: usize) -> Vec<f32> {
        let sweep = log_sweep(params).expect("calibrate's τ sweep is valid");
        let tail = (TAU_TAIL_S * params.sample_rate as f64).round() as usize;
        let mut cap = vec![0.0f32; sweep.len() + tail];
        for (i, &s) in sweep.iter().enumerate() {
            if let Some(slot) = cap.get_mut(i + delay) {
                *slot += s * 0.03;
            }
        }
        cap
    }

    /// The peak index and gate SNR `analyse_tau_leg` sees for `cap`,
    /// computed by the same pipeline. Needed because a `LowSnrRefusal`
    /// does not carry the peak position the skirt test has to judge.
    fn synthetic_tau_peak(cap: &[f32], params: &SweepParams, window_len: usize) -> (usize, f64) {
        let inv = inverse_sweep(params).expect("inverse sweep");
        let full = deconvolve_full(cap, &inv);
        let irs = extract_irs(&full, params, 1, window_len).expect("τ IR");
        let (peak, _) = ir_peak(&irs.linear);
        (peak, pre_impulse_snr_db(&irs.linear, peak))
    }

    /// Coupled-constants guard (QA, PR #473). [`REF_SNR_MARGIN_DB`]'s
    /// provenance is a *relationship*: `calibrate`'s own ESS floors at
    /// ≈26.8 dB, and 26.8 − 3 ≈ the shipped [`TAU_SNR_THRESHOLD_DB`] of 24.0,
    /// which is the evidence that deriving the reference leg's threshold
    /// generalises rather than inventing a new policy. Nothing enforced that
    /// relationship: either constant could move alone and silently falsify the
    /// doc comment on the other.
    ///
    /// Judges the sweep [`measure_tau`] actually plays, via
    /// [`tau_sweep_params`], so an edit to calibrate's stimulus fails here too
    /// — that is the coupling, and a second copy of the expression would hide
    /// exactly the change worth catching.
    #[test]
    fn ref_snr_margin_reproduces_calibrates_shipped_threshold() {
        let sr = 96_000;
        let params = tau_sweep_params(sr);
        let half = (tau_half_window_s() * sr as f64).ceil() as usize;
        let window_len = 2 * half;
        // Any interior peak serves; the floor varies only across the ~27→30 dB
        // range #471 characterised, well inside the 1 dB bar below.
        let peak = half + 1711;
        let floor =
            ac_core::measurement::sweep::pre_impulse_snr_floor_db(&params, window_len, peak)
                .expect("calibrate's own ESS must have a floor");
        let derived_equivalent = floor - REF_SNR_MARGIN_DB;
        assert!(
            (derived_equivalent - TAU_SNR_THRESHOLD_DB).abs() < 1.0,
            "REF_SNR_MARGIN_DB no longer reproduces TAU_SNR_THRESHOLD_DB against calibrate's own \
             stimulus: floor {floor:.1} - margin {REF_SNR_MARGIN_DB} = {derived_equivalent:.1}, \
             shipped constant is {TAU_SNR_THRESHOLD_DB}. If that is intentional, update whichever \
             doc comment still claims the other"
        );
    }

    /// #350: an arrival just past the window edge (10 samples, ≈0.1 ms at
    /// 96 kHz) pins the peak at the last sample, and the *edge check* is the
    /// only thing that refuses it — the pinned peak's gate SNR still clears
    /// [`TAU_SNR_THRESHOLD_DB`]. Fails if the edge check is removed or
    /// bypassed, if the peak stops pinning at the last sample, if
    /// calibrate's stimulus changes so the pinned peak's gate SNR drops
    /// under the threshold, or if the `<=` in `check_peak_within_window` is
    /// weakened to `<` — the last only through the margin-0 assertions,
    /// since at the shipped margin `<` and `<=` both refuse a pinned peak.
    /// The refusal must also carry the SNR observation with the gate
    /// cleared (#494), which is the edge-only headline. Synthetic, per the
    /// architect decision on #350; the indices depend on `extract_irs`'s
    /// rectangular gate and on `ir_peak`.
    #[test]
    fn tau_arrival_just_past_the_edge_is_refused_by_the_edge_check_alone() {
        let (params, half, window_len) = synthetic_tau_window(96_000);
        let cap = synthetic_tau_capture(&params, half + 10);

        let (peak, snr_db) = synthetic_tau_peak(&cap, &params, window_len);
        assert_eq!(
            peak,
            window_len - 1,
            "an arrival past the edge should pin the peak at the last sample (#350)"
        );
        assert!(
            snr_db >= TAU_SNR_THRESHOLD_DB,
            "pinned peak's gate SNR {snr_db:.2} dB fell under {TAU_SNR_THRESHOLD_DB} dB — the \
             SNR gate now refuses this case too, so the edge check is no longer the only net \
             the #350 architect decision says it is"
        );

        let err = analyse_tau_leg(&cap, &params, TAU_TAIL_S, 0, SnrGate::Constant)
            .expect_err("an arrival outside the window must not return a τ");
        let refusal = err
            .downcast_ref::<EdgeRefusal>()
            .unwrap_or_else(|| panic!("expected an EdgeRefusal, got: {err}"));
        assert_eq!(refusal.peak_idx, window_len - 1);
        assert_eq!(
            refusal.snr,
            Some(EdgeSnr {
                snr_db,
                threshold_db: TAU_SNR_THRESHOLD_DB,
                below_threshold: false,
            }),
            "an xrun-free edge refusal carries its own peak's SNR observation (#494)"
        );

        // The pinned peak sits at dist 0 from the edge. At the shipped
        // margin `<` and `<=` both refuse that; only margin 0 separates them.
        assert!(
            check_peak_within_window(window_len - 1, window_len, 0.0).is_err(),
            "with the margin at 0, only the `<=` edge test refuses a pinned peak (#350)"
        );
        assert!(
            check_peak_within_window(0, window_len, 0.0).is_err(),
            "with the margin at 0, only the `<=` edge test refuses a peak pinned at the start (#350)"
        );
    }

    /// #350: an arrival a little further past the edge (85 samples) does not
    /// pin — `ir_peak` picks a skirt peak 100–300 samples *inside* the edge,
    /// which as a τ would read ≈2 ms short and look plausible. Both gates
    /// fail it: the shipped edge margin, and the SNR gate (only by a dB or
    /// two). This test records both, and computes the rejected alternative —
    /// a 2 % margin — to show that shrinking [`TAU_EDGE_MARGIN_FRAC`] loses
    /// the edge observation.
    ///
    /// #494 changed what `analyse_tau_leg` returns here, deliberately: #495
    /// pinned a `LowSnrRefusal` (the #368 SNR-first order), which told the
    /// operator to look at noise for an arrival that is past the window. It
    /// now returns an `EdgeRefusal` whose SNR observation is below the
    /// threshold, so the operator sees both. This assertion fails against the
    /// SNR-first order. Synthetic, per the architect decision on #350; if the
    /// skirt moves out of the asserted band, `extract_irs`'s gate or
    /// `ir_peak` changed and the margin's provenance note needs re-deriving.
    #[test]
    fn tau_skirt_peak_past_the_edge_is_inside_the_shipped_margin() {
        let (params, half, window_len) = synthetic_tau_window(96_000);
        let cap = synthetic_tau_capture(&params, half + 85);

        let (peak, snr_db) = synthetic_tau_peak(&cap, &params, window_len);
        let dist_from_end = window_len - 1 - peak;
        assert!(
            (100..=300).contains(&dist_from_end),
            "skirt peak sits {dist_from_end} samples inside the edge, outside the 100–300 band \
             the #350 architect decision characterised"
        );
        let shipped_margin = (TAU_EDGE_MARGIN_FRAC * half as f64).round() as usize;
        assert!(dist_from_end <= shipped_margin);
        assert!(
            check_peak_within_window(peak, window_len, TAU_EDGE_MARGIN_FRAC).is_err(),
            "the shipped margin must refuse the skirt peak"
        );
        assert!(
            check_peak_within_window(peak, window_len, 0.02).is_ok(),
            "a 2 % margin would let this skirt peak through — the reason not to lower the margin"
        );
        assert!(
            snr_db < TAU_SNR_THRESHOLD_DB,
            "skirt peak's gate SNR {snr_db:.2} dB now clears {TAU_SNR_THRESHOLD_DB} dB — the \
             margin is the only net left here"
        );

        let err = analyse_tau_leg(&cap, &params, TAU_TAIL_S, 0, SnrGate::Constant)
            .expect_err("a skirt peak must not return a τ");
        let refusal = err.downcast_ref::<EdgeRefusal>().unwrap_or_else(|| {
            panic!("expected the edge refusal to win over the SNR gate (#494), got: {err}")
        });
        assert_eq!(refusal.peak_idx, peak);
        assert_eq!(
            refusal.snr,
            Some(EdgeSnr {
                snr_db,
                threshold_db: TAU_SNR_THRESHOLD_DB,
                below_threshold: true,
            }),
            "the edge refusal must report the failed SNR gate too (#494)"
        );
    }

    /// Uniform white noise at `dbfs` RMS added to `cap`, from a seeded
    /// xorshift so a sweep over it is deterministic.
    fn add_white_noise(cap: &mut [f32], dbfs: f64, seed: u64) {
        let peak = 10f64.powf(dbfs / 20.0) * 3f64.sqrt();
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        for x in cap.iter_mut() {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let u = (state >> 11) as f64 / (1u64 << 53) as f64;
            *x += ((2.0 * u - 1.0) * peak) as f32;
        }
    }

    /// `analyse_tau_leg`'s refusal for `cap`, which must be an
    /// [`EdgeRefusal`] carrying an SNR observation. `what` names the case in
    /// the failure message.
    fn expect_edge_refusal(cap: &[f32], params: &SweepParams, what: &str) -> EdgeRefusal {
        let err = match analyse_tau_leg(cap, params, TAU_TAIL_S, 0, SnrGate::Constant) {
            Ok(r) => panic!("{what}: returned τ {} s, expected an edge refusal", r.tau_s),
            Err(e) => e,
        };
        let refusal = *err
            .downcast_ref::<EdgeRefusal>()
            .unwrap_or_else(|| panic!("{what}: expected an EdgeRefusal (#494), got: {err}"));
        assert!(
            refusal.snr.is_some(),
            "{what}: an xrun-free edge refusal must carry its SNR observation"
        );
        refusal
    }

    /// #494 AC1: an arrival 60–400 samples past the far edge (d = 4860–5200
    /// at 96 kHz) is refused with the edge reason, not the low-SNR reason,
    /// with capture noise off, at −40 dBFS and at −20 dBFS RMS against the
    /// 0.03 stimulus (three seeds each). Plus a noiseless arrival 5000
    /// samples out. Under the #368 SNR-first order most of these points
    /// returned a `LowSnrRefusal`, so this sweep goes red against it.
    ///
    /// The range is bounded on purpose — the architect decision on #494
    /// records it. Under noise, beyond ≈400 samples out the in-window IR is
    /// noise and residue, its argmax lands anywhere, and a low-SNR reason is
    /// the honest observation. Synthetic, 96 kHz only.
    #[test]
    fn tau_arrival_past_the_far_edge_is_refused_with_the_edge_reason() {
        let (params, half, _) = synthetic_tau_window(96_000);
        let mut combined = 0usize;
        for past_edge in 60..=400usize {
            let base = synthetic_tau_capture(&params, half + past_edge);
            let noise_levels: [Option<f64>; 3] = [None, Some(-40.0), Some(-20.0)];
            for noise_dbfs in noise_levels {
                let seeds = if noise_dbfs.is_some() {
                    1..=3u64
                } else {
                    1..=1u64
                };
                for seed in seeds {
                    let mut cap = base.clone();
                    if let Some(dbfs) = noise_dbfs {
                        add_white_noise(&mut cap, dbfs, seed);
                    }
                    let what = format!(
                        "arrival {past_edge} past the edge, noise {noise_dbfs:?} dBFS, seed {seed}"
                    );
                    let refusal = expect_edge_refusal(&cap, &params, &what);
                    if refusal.snr.is_some_and(|s| s.below_threshold) {
                        combined += 1;
                    }
                }
            }
        }
        assert!(
            combined > 0,
            "no point in the sweep failed the SNR gate too — the sweep no longer exercises \
             the precedence #494 is about"
        );

        let cap = synthetic_tau_capture(&params, half + 5000);
        let refusal = expect_edge_refusal(&cap, &params, "noiseless arrival 5000 past the edge");
        assert!(
            refusal.snr.is_some_and(|s| s.below_threshold),
            "a noiseless arrival 5000 samples out fails the SNR gate too: {refusal:?}"
        );
    }

    /// #494 AC2 and AC3, the issue's own delays. d = 4799–4850: the peak
    /// pins at the last sample and the edge reason stays. d = 4880–4890: a
    /// skirt peak lands inside the margin with a sub-threshold SNR, and the
    /// reason is the edge one, with the failed gate reported beside it.
    #[test]
    fn tau_issue_494_delays_are_refused_with_the_edge_reason() {
        let (params, half, window_len) = synthetic_tau_window(96_000);
        for d in 4799..=4850usize {
            let cap = synthetic_tau_capture(&params, d);
            let refusal = expect_edge_refusal(&cap, &params, &format!("d = {d}"));
            assert_eq!(
                refusal.peak_idx,
                window_len - 1,
                "d = {d}: the peak should pin at the last sample"
            );
            assert_eq!(refusal.window_last_offset_samples(), half as i64 - 1);
            assert_eq!(refusal.peak_offset_samples(), half as i64 - 1);
        }
        for d in 4880..=4890usize {
            let cap = synthetic_tau_capture(&params, d);
            let refusal = expect_edge_refusal(&cap, &params, &format!("d = {d}"));
            let dist_from_end = window_len - 1 - refusal.peak_idx;
            assert!(
                dist_from_end > 0 && dist_from_end <= refusal.margin,
                "d = {d}: skirt peak {dist_from_end} samples inside the edge, outside the margin"
            );
            assert!(
                refusal.snr.is_some_and(|s| s.below_threshold),
                "d = {d}: the skirt peak's SNR is below the gate: {refusal:?}"
            );
        }
    }

    /// #494 AC4: a noise-only capture whose argmax lands well clear of the
    /// margin is still refused by the SNR gate alone. The seed search keeps
    /// the precondition honest — if no seed puts the argmax clear of the
    /// margin, the test fails rather than asserting nothing.
    #[test]
    fn tau_noise_only_capture_clear_of_the_edge_is_refused_for_low_snr() {
        let (params, half, window_len) = synthetic_tau_window(96_000);
        let sweep_len = log_sweep(&params).expect("sweep").len();
        let tail = (TAU_TAIL_S * params.sample_rate as f64).round() as usize;
        let mut checked = 0;
        for seed in 1..=10u64 {
            let mut cap = vec![0.0f32; sweep_len + tail];
            add_white_noise(&mut cap, -40.0, seed);
            let (peak, snr_db) = synthetic_tau_peak(&cap, &params, window_len);
            let margin = (TAU_EDGE_MARGIN_FRAC * half as f64).round() as usize;
            if peak <= 2 * margin || window_len - 1 - peak <= 2 * margin {
                continue;
            }
            let err = analyse_tau_leg(&cap, &params, TAU_TAIL_S, 0, SnrGate::Constant)
                .expect_err("noise must not return a τ");
            let refusal = err
                .downcast_ref::<LowSnrRefusal>()
                .unwrap_or_else(|| panic!("seed {seed}: expected a LowSnrRefusal, got: {err}"));
            assert_eq!(refusal.snr_db, snr_db);
            assert_eq!(refusal.threshold_db, TAU_SNR_THRESHOLD_DB);
            checked += 1;
        }
        assert!(
            checked > 0,
            "no seed put the noise argmax clear of the margin"
        );
    }

    /// #494 AC5: a clean in-window arrival still passes, at exactly its
    /// delay, with the SNR the gate judged — the accept path is unchanged by
    /// the reorder.
    #[test]
    fn tau_clean_in_window_arrival_is_accepted_at_its_delay() {
        let (params, _, window_len) = synthetic_tau_window(96_000);
        for delay in [32usize, 1711, 4200] {
            let cap = synthetic_tau_capture(&params, delay);
            let (_, snr_db) = synthetic_tau_peak(&cap, &params, window_len);
            let reading = analyse_tau_leg(&cap, &params, TAU_TAIL_S, 0, SnrGate::Constant)
                .unwrap_or_else(|e| panic!("delay {delay}: clean arrival refused: {e}"));
            assert_eq!(
                reading,
                TauLegReading {
                    tau_s: delay as f64 / 96_000.0,
                    snr_db,
                    snr_floor_db: None,
                }
            );
        }
    }

    /// #494: a lifecycle that crossed an xrun skips the SNR gate, so its
    /// edge refusal carries no SNR observation — the `refused_xrun` absence
    /// rule, not a figure that could read as a scored floor.
    #[test]
    fn tau_edge_refusal_after_an_xrun_carries_no_snr() {
        let (params, half, _) = synthetic_tau_window(96_000);
        let cap = synthetic_tau_capture(&params, half + 85);
        let err = analyse_tau_leg(&cap, &params, TAU_TAIL_S, 1, SnrGate::Constant)
            .expect_err("a skirt peak must not return a τ");
        let refusal = err
            .downcast_ref::<EdgeRefusal>()
            .unwrap_or_else(|| panic!("expected an EdgeRefusal, got: {err}"));
        assert_eq!(refusal.snr, None);
    }

    /// UX's daemon-side strings (#494): the observation and "no value
    /// reported", no asserted cause.
    #[test]
    fn tau_refusal_messages_state_observations_only() {
        let edge = EdgeRefusal {
            peak_idx: 9599,
            window_len: 9600,
            margin: 480,
            snr: None,
        };
        assert_eq!(
            edge.to_string(),
            "\u{3c4} peak at offset +4799 samples is within the 480-sample edge margin of a \
             -4800 to +4799 sample window; no value reported"
        );
        let low = LowSnrRefusal {
            snr_db: 21.34,
            threshold_db: 24.0,
        };
        assert_eq!(
            low.to_string(),
            "\u{3c4} peak pre-impulse SNR 21.34 dB is below the 24.00 dB threshold; no value \
             reported"
        );
    }

    /// #350, QA follow-up: the ≈0.046 floor in [`TAU_EDGE_MARGIN_FRAC`]'s
    /// provenance is a claim about the *deepest* skirt peak, so it is pinned
    /// by a maximum over a seeded-noise sweep, not by the one noiseless
    /// index above. Arrivals 80–92 samples past the edge, white noise off
    /// and at −80 / −60 / −40 / −30 dBFS RMS against the 0.03 stimulus, five
    /// seeds each. Every skirt peak (not pinned) must sit no deeper than
    /// `round(0.046 · half)` samples inside the edge, and every peak in the
    /// sweep must fail the SNR gate. The rejected alternative — "the skirt
    /// can land deeper than the floor" — is what the first assertion
    /// measures; it fails if it becomes true. Noise at −20 dBFS is left
    /// out: there a skirt peak can land ~290 samples in, past the floor, but
    /// at a gate SNR ≈10 dB under the threshold, so the floor's argument
    /// (the gate clears it by a dB or two) does not apply there.
    /// Synthetic, 96 kHz only.
    #[test]
    fn tau_skirt_peak_never_lands_deeper_than_the_margin_floor() {
        const MARGIN_FLOOR_FRAC: f64 = 0.046;
        let (params, half, window_len) = synthetic_tau_window(96_000);
        let floor = (MARGIN_FLOOR_FRAC * half as f64).round() as usize;
        let shipped_margin = (TAU_EDGE_MARGIN_FRAC * half as f64).round() as usize;
        assert!(
            floor < shipped_margin,
            "the shipped margin must stay above the ≈0.046 floor (#350)"
        );

        let mut deepest = 0usize;
        for past_edge in 80..=92usize {
            let base = synthetic_tau_capture(&params, half + past_edge);
            let noise_levels: [Option<f64>; 5] =
                [None, Some(-80.0), Some(-60.0), Some(-40.0), Some(-30.0)];
            for noise_dbfs in noise_levels {
                let seeds = if noise_dbfs.is_some() {
                    1..=5u64
                } else {
                    1..=1u64
                };
                for seed in seeds {
                    let mut cap = base.clone();
                    if let Some(dbfs) = noise_dbfs {
                        add_white_noise(&mut cap, dbfs, seed);
                    }
                    let (peak, snr_db) = synthetic_tau_peak(&cap, &params, window_len);
                    let dist_from_end = window_len - 1 - peak;
                    assert!(
                        dist_from_end <= floor,
                        "arrival {past_edge} past the edge, noise {noise_dbfs:?} dBFS, seed \
                         {seed}: skirt peak {dist_from_end} samples inside the edge, deeper \
                         than the {floor}-sample (0.046) floor TAU_EDGE_MARGIN_FRAC's \
                         provenance cites — re-derive it (#350)"
                    );
                    assert!(
                        snr_db < TAU_SNR_THRESHOLD_DB,
                        "arrival {past_edge} past the edge, noise {noise_dbfs:?} dBFS, seed \
                         {seed}: gate SNR {snr_db:.2} dB clears {TAU_SNR_THRESHOLD_DB} dB"
                    );
                    deepest = deepest.max(dist_from_end);
                }
            }
        }
        assert!(
            deepest > 0,
            "no skirt peak in the sweep — every arrival pinned, so the floor went untested"
        );
    }

    /// #350, cost side: an arrival 100 samples *inside* the edge is located
    /// exactly, and still refused because it sits within the margin. These
    /// are correct values given up for the second net above — at 96 kHz the
    /// margin moves the ceiling from ≈50 ms to 44.98 ms (4318 samples; the
    /// rig record rounds these to 50.01 → 44.99 ms).
    #[test]
    fn tau_arrival_inside_the_margin_is_located_exactly_and_refused() {
        let (params, half, window_len) = synthetic_tau_window(96_000);
        let delay = half - 100;
        let cap = synthetic_tau_capture(&params, delay);

        let (peak, _) = synthetic_tau_peak(&cap, &params, window_len);
        assert_eq!(
            peak,
            half + delay,
            "in-window arrival must be located exactly"
        );

        let err = analyse_tau_leg(&cap, &params, TAU_TAIL_S, 0, SnrGate::Constant)
            .expect_err("a peak within the margin is refused, even when correct");
        let refusal = err
            .downcast_ref::<EdgeRefusal>()
            .unwrap_or_else(|| panic!("expected an EdgeRefusal, got: {err}"));
        assert_eq!(refusal.peak_idx, half + delay);
    }

    /// #368: `check_peak_snr` mirrors `check_peak_within_window`'s shape —
    /// pin its boundary the same way (refuses strictly below, accepts at
    /// and above).
    #[test]
    fn check_peak_snr_refuses_below_threshold() {
        assert!(check_peak_snr(23.99, 24.0).is_err());
    }

    #[test]
    fn check_peak_snr_accepts_at_and_above_threshold() {
        assert!(check_peak_snr(24.0, 24.0).is_ok());
        assert!(check_peak_snr(83.5, 24.0).is_ok());
    }

    /// The rig's own measured muted-route reading (#368 triage: drive
    /// -30 dBFS, captured -83.8 dBFS) — a concrete refusal, not just a
    /// boundary probe.
    #[test]
    fn check_peak_snr_refuses_the_rigs_measured_muted_route() {
        assert!(check_peak_snr(-3.45, TAU_SNR_THRESHOLD_DB).is_err());
    }

    /// The refusal must stay recoverable *by type* — `measure_tau_twice`
    /// downcasts to split `not_measured_low_snr` from the generic `error`
    /// state, so an `anyhow!`-flavoured rewording of the message here must
    /// not quietly collapse the two.
    #[test]
    fn check_peak_snr_refusal_is_downcastable_to_its_own_type() {
        let err = check_peak_snr(-3.45, TAU_SNR_THRESHOLD_DB).expect_err("refused");
        let refusal = err
            .downcast_ref::<LowSnrRefusal>()
            .expect("refusal carries its typed payload, not just a message");
        assert_eq!(refusal.snr_db, -3.45);
        assert_eq!(refusal.threshold_db, TAU_SNR_THRESHOLD_DB);
    }
}
