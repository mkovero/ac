//! Per-channel monitor state: the analysis rings, the channel's
//! calibration and mic correction, the dual-resolution LF band, and the
//! fractional-octave time integrators.

use ac_core::measurement::loudness::LoudnessState;
use ac_core::shared::calibration::Calibration;
use ac_core::shared::mic_curve_filter::{MicCurveFir, DEFAULT_N_TAPS};
use ac_core::visualize::time_integration::{EmaIntegrator, LeqIntegrator, TAU_FAST_S, TAU_SLOW_S};

// mic-curve helpers live in `handlers::mic` since the Tier 1 handlers also
// need them; see #97 / #98.
use crate::handlers::mic::{
    apply_mic_curve_inplace_f32, apply_mic_curve_inplace_f64, mic_correction_tag,
};

use super::frames::TickCtx;
use super::reconnect::ReconnectState;

/// LF-band FFT overlap fraction (#173). The previous un-overlapped block
/// cadence (~0.7-1.4 s depending on `lf_fft_n`) recomputed the LF spectrum
/// from entirely fresh samples each time; under broadband material each
/// recompute is an independent chi-squared(2) draw per bin (~5.6 dB sigma,
/// matching #173's measurement), so the LF half of the display lurched
/// while the HF band (refreshed every tick) glided smoothly. 90% overlap
/// keeps the 65536-point FFT's frequency resolution (needed to split
/// closely-spaced LF tones, #142) while recomputing every
/// `(1 - LF_OVERLAP) * lf_fft_n / sr` seconds instead of the full block —
/// ~136 ms at the N=65536/48kHz default, under the ~170 ms cadence target.
pub(super) const LF_OVERLAP: f64 = 0.9;

/// Power-domain EMA time constant applied to each newly-recomputed LF
/// spectrum (#173), reusing `EmaIntegrator`'s existing fast/slow/Leq
/// convention at per-bin scale. Tuned in
/// `ac_core::visualize::time_integration::tests::lf_ema_brings_variance_within_2x_of_hf_target`
/// to bring the raw ~5.6 dB chi-squared sigma down to ~2.2-2.4 dB, within
/// 2x of the HF band's measured 0.7-2.4 dB range, without smearing tone
/// levels (EMA is power-domain, unbiased at steady state).
pub(super) const LF_AVG_TAU_S: f64 = 0.25;

/// The LF band is only worth running when its FFT is genuinely longer
/// than the live one — otherwise the live spectrum already has
/// equal-or-finer Δf everywhere and the splice would be a no-op that
/// still costs a 65536-point FFT.
pub(super) fn lf_band_enabled(lf_fft_n: u32, fft_n: u32) -> bool {
    lf_fft_n > fft_n
}

/// Ticks between LF recomputes, for a given LF FFT length, sample rate
/// and refresh interval (#173).
///
/// The LF spectrum advances one overlap-hop at a time —
/// `(1 - LF_OVERLAP) * lf_fft_n / sr` seconds — rather than once per full
/// block. The result is clamped to at least 1: a hop shorter than a tick
/// means recompute on every tick, and a zero here would make the
/// `counter >= recompute_every` test always true *and* leave the counter
/// pinned at zero, which reads the same but is one bad rounding away from
/// meaning "never". The 4096 ceiling bounds the other direction.
pub(super) fn lf_recompute_every(lf_fft_n: u32, sr: u32, interval_s: f64) -> u32 {
    ((lf_fft_n as f64 * (1.0 - LF_OVERLAP) / sr as f64) / interval_s.max(1e-6))
        .round()
        .clamp(1.0, 4096.0) as u32
}

/// A channel's mic-curve correction resolved for one tick: the curve, if
/// one is loaded, and whether the global toggle is on.
///
/// It borrows the curve field alone rather than the whole `ChannelState`,
/// so the FFT path can build one while still holding a mutable borrow of
/// the same channel's sliding ring. Keeping the tag and the two `apply_*`
/// calls on one type is the point: a frame can never come out corrected
/// but tagged `"off"`, or tagged `"on"` without the curve having run.
#[derive(Clone, Copy)]
pub(super) struct MicCorrection<'a> {
    pub(super) curve: Option<&'a ac_core::shared::calibration::MicResponse>,
    pub(super) enabled: bool,
}

impl<'a> MicCorrection<'a> {
    /// Built from the curve field alone so the caller keeps the rest of
    /// its `ChannelState` free to borrow mutably.
    pub(super) fn new(
        curve: Option<&'a ac_core::shared::calibration::MicResponse>,
        ctx: &TickCtx,
    ) -> Self {
        Self {
            curve,
            enabled: ctx.mic_corr_enabled,
        }
    }
}

impl MicCorrection<'_> {
    /// `"on"` only when a curve is loaded *and* correction is enabled;
    /// `"off"` when a curve exists but is bypassed; `"none"` with no
    /// curve.
    pub(super) fn tag(&self) -> &'static str {
        mic_correction_tag(self.curve.is_some(), self.enabled)
    }

    /// Correct f32 magnitudes in place.
    pub(super) fn apply_f32(&self, freqs: &[f32], mags: &mut [f32]) {
        if self.enabled {
            if let Some(curve) = self.curve {
                apply_mic_curve_inplace_f32(curve, freqs, mags);
            }
        }
    }

    /// Correct f64 aggregated columns in place.
    pub(super) fn apply_f64(&self, freqs: &[f64], values: &mut [f64]) {
        if self.enabled {
            if let Some(curve) = self.curve {
                apply_mic_curve_inplace_f64(curve, freqs, values);
            }
        }
    }
}

/// Which of a channel's rings a ring-buffered mode fills.
#[derive(Clone, Copy)]
pub(super) enum RingKind {
    Cwt,
    Cqt,
    Reassigned,
}

/// Per-channel time-integrator state for the `fractional_octave_leq`
/// sidecar frame. Re-initialised when the mode changes or when the band
/// count changes (ioct_bpo toggle).
pub(super) enum Integrator {
    Ema(EmaIntegrator),
    Leq(LeqIntegrator),
}

impl Integrator {
    pub(super) fn for_mode(mode: &str, n_bands: usize) -> Option<Self> {
        match mode {
            "fast" => Some(Self::Ema(EmaIntegrator::new(TAU_FAST_S, n_bands))),
            "slow" => Some(Self::Ema(EmaIntegrator::new(TAU_SLOW_S, n_bands))),
            "leq" => Some(Self::Leq(LeqIntegrator::new(n_bands))),
            _ => None,
        }
    }

    pub(super) fn n_bands(&self) -> usize {
        match self {
            Self::Ema(e) => e.state_len(),
            Self::Leq(l) => l.state_len(),
        }
    }

    pub(super) fn update(&mut self, levels_dbfs: &[f64], dt_s: f64) -> Vec<f64> {
        match self {
            Self::Ema(e) => e.update(levels_dbfs, dt_s),
            Self::Leq(l) => l.update(levels_dbfs, dt_s),
        }
    }

    pub(super) fn duration_s(&self) -> f64 {
        match self {
            Self::Ema(_) => f64::NAN,
            Self::Leq(l) => l.duration_s(),
        }
    }

    pub(super) fn reset_if_leq(&mut self) {
        if let Self::Leq(l) = self {
            l.reset();
        }
    }
}

/// Dual-resolution low-frequency FFT state for one channel (#142 / #173).
///
/// A second, longer ring feeds an `lf_fft_n`-point FFT used only below
/// `crossover_hz`; the channel's short `fft_ring` keeps driving the high
/// band at the live refresh rate. The LF spectrum is recomputed every
/// `LF_OVERLAP`-hop (`(1 - LF_OVERLAP) * lf_fft_n / sr`, #173) rather
/// than once per full block, and each new raw recompute is smoothed
/// through a power-domain EMA (`ema`) before caching — see `LF_OVERLAP`
/// / `LF_AVG_TAU_S` above.
///
/// The five members are only ever read and written together — the long
/// ring, the smoothed spectrum it produces, the overlap-hop counter, and
/// the EMA plus the timestamp its `dt` is measured from.
pub(super) struct LfState {
    pub(super) ring: std::collections::VecDeque<f32>,
    /// Most recent smoothed LF linear half-spectrum; `None` until the
    /// ring first fills.
    pub(super) spec_cache: Option<Vec<f64>>,
    pub(super) ticks_since_recompute: u32,
    pub(super) ema: Option<EmaIntegrator>,
    pub(super) ema_last_ts: Option<std::time::Instant>,
}

impl LfState {
    pub(super) fn new() -> Self {
        Self {
            ring: std::collections::VecDeque::with_capacity(131_072),
            spec_cache: None,
            ticks_since_recompute: u32::MAX,
            ema: None,
            ema_last_ts: None,
        }
    }

    /// True when this channel still holds LF state that a disabled LF
    /// band would leave stale.
    pub(super) fn is_stale(&self) -> bool {
        !self.ring.is_empty() || self.spec_cache.is_some()
    }

    /// Point the EMA at `n_bins` bins, rebuilding it when the bin count
    /// changed (an `lf_fft_n` change) or it has not been built yet, so
    /// stale state is never fed a mismatched vector length. Returns true
    /// when it rebuilt.
    pub(super) fn ensure_ema(&mut self, n_bins: usize) -> bool {
        let stale = self
            .ema
            .as_ref()
            .map(|e| e.state_len() != n_bins)
            .unwrap_or(true);
        if stale {
            self.ema = Some(EmaIntegrator::new(LF_AVG_TAU_S, n_bins));
            self.ema_last_ts = None;
        }
        stale
    }

    /// Append `new` to the long ring, trimmed to `lf_fft_n` from the
    /// front, and recompute the cached LF half-spectrum when the ring is
    /// full and the overlap hop has elapsed.
    ///
    /// `now` is passed in rather than read here so the EMA's `dt` is
    /// testable. On the first recompute after a rebuild there is no
    /// previous timestamp, so `dt` falls back to the nominal hop.
    pub(super) fn push_and_maybe_recompute(
        &mut self,
        new: &[f32],
        lf_fft_n: u32,
        sr: u32,
        recompute_every: u32,
        now: std::time::Instant,
    ) {
        self.ring.extend(new.iter());
        while self.ring.len() > lf_fft_n as usize {
            self.ring.pop_front();
        }
        if self.ring.len() < lf_fft_n as usize {
            return;
        }
        if self.ticks_since_recompute < recompute_every {
            self.ticks_since_recompute = self.ticks_since_recompute.saturating_add(1);
            return;
        }
        let buf = self.ring.make_contiguous();
        let (spec, _) = ac_core::visualize::spectrum::spectrum_only(buf, sr);
        // Power-domain EMA smoothing (#173).
        self.ensure_ema(spec.len());
        let nominal_hop_s = lf_fft_n as f64 * (1.0 - LF_OVERLAP) / sr as f64;
        let dt = self
            .ema_last_ts
            .map(|t| now.duration_since(t).as_secs_f64())
            .unwrap_or(nominal_hop_s)
            .max(1e-6);
        self.ema_last_ts = Some(now);
        let raw_db: Vec<f64> = spec.iter().map(|&a| 20.0 * a.log10()).collect();
        let smoothed_db = self
            .ema
            .as_mut()
            .expect("ensure_ema just populated it")
            .update(&raw_db, dt);
        self.spec_cache = Some(
            smoothed_db
                .iter()
                .map(|&db| 10f64.powf(db / 20.0))
                .collect(),
        );
        self.ticks_since_recompute = 0;
    }

    /// Drop everything so a later re-enable rebuilds from fresh capture.
    pub(super) fn clear(&mut self) {
        self.ring.clear();
        self.spec_cache = None;
        self.ticks_since_recompute = u32::MAX;
        self.ema = None;
        self.ema_last_ts = None;
    }
}

/// Everything the monitor worker tracks for one monitored channel.
///
/// This used to be eighteen `Vec`s walked by a shared `idx`. Holding one
/// struct per channel means a channel cannot go half-initialised, a new
/// piece of per-channel state cannot be added at one construction site
/// and forgotten at another, and `idx` can no longer address two
/// different channels within one tick.
pub(super) struct ChannelState {
    pub(super) channel: u32,
    /// Resolved input port; used only by the multi-channel
    /// `reconnect_input` path.
    pub(super) in_port: String,
    pub(super) cal: Option<Calibration>,
    /// Per-channel SPL offset (= 94 - mic_sens_dbfs); `None` when the
    /// channel hasn't been pistonphone-calibrated. Cached once at start
    /// — re-running `calibrate_spl` requires a `monitor` restart, same
    /// as voltage cal changes need today.
    pub(super) spl_offset: Option<f64>,
    /// Mic frequency-response curve, cloned out of `cal` for cheap
    /// per-tick lookup. Same staleness caveat as `spl_offset`.
    pub(super) mic_curve: Option<ac_core::shared::calibration::MicResponse>,
    /// Mic-curve FIR for the loudness path (#104), built once at start
    /// when the curve is loaded, bypassed when no curve or when the
    /// global toggle is off. Runs *before* K-weighting / dBTP so LKFS
    /// reflects the mic-corrected acoustic level.
    pub(super) loudness_fir: Option<MicCurveFir>,
    pub(super) current_freq: f64,
    /// #93: reconnect-failure state for the multi-channel path.
    /// Single-channel never touches `eng.reconnect_input()` and this
    /// stays zeroed.
    pub(super) reconnect: ReconnectState,
    pub(super) cwt_ring: std::collections::VecDeque<f32>,
    pub(super) cqt_ring: std::collections::VecDeque<f32>,
    pub(super) reass_ring: std::collections::VecDeque<f32>,
    /// Sliding ring for the FFT path so refresh cadence (`cur_interval`)
    /// is decoupled from capture-window duration (`cur_fft_n / sr`).
    pub(super) fft_ring: std::collections::VecDeque<f32>,
    pub(super) lf: LfState,
    /// Time-integration state for the `fractional_octave_leq` sidecar
    /// frame. `None` until the first fractional_octave frame at the
    /// current mode + band count arrives.
    pub(super) integrator: Option<Integrator>,
    pub(super) last_frac_ts: Option<std::time::Instant>,
    /// BS.1770-5 / R128 mono-weighted loudness, emitted as a
    /// `measurement/loudness` sidecar frame each tick.
    pub(super) loudness: LoudnessState,
}

/// Ring capacities for one channel. They come from the worker rather
/// than being recomputed here because the same values also drive the
/// per-tick trim conditions, and each is derived from the engine's `sr`,
/// which is only known after `eng.start()`.
pub(super) struct RingCaps {
    pub(super) cwt: usize,
    pub(super) cqt: usize,
    pub(super) reass: usize,
}

impl ChannelState {
    /// This channel's mic correction for the current tick. Callers that
    /// need to borrow other fields mutably at the same time should build
    /// it with `MicCorrection::new(ch.mic_curve.as_ref(), ctx)` instead,
    /// which borrows only the curve.
    pub(super) fn mic_correction(&self, ctx: &TickCtx) -> MicCorrection<'_> {
        MicCorrection::new(self.mic_curve.as_ref(), ctx)
    }

    /// The ring a given ring-buffered mode fills.
    pub(super) fn ring_mut(&mut self, kind: RingKind) -> &mut std::collections::VecDeque<f32> {
        match kind {
            RingKind::Cwt => &mut self.cwt_ring,
            RingKind::Cqt => &mut self.cqt_ring,
            RingKind::Reassigned => &mut self.reass_ring,
        }
    }

    pub(super) fn new(
        channel: u32,
        in_port: String,
        cal: Option<Calibration>,
        sr: u32,
        freq_hz: f64,
        caps: &RingCaps,
    ) -> Self {
        let spl_offset = cal.as_ref().and_then(Calibration::spl_offset_db);
        let mic_curve = cal.as_ref().and_then(|c| c.mic_response.clone());
        let loudness_fir = mic_curve
            .as_ref()
            .map(|curve| MicCurveFir::new(curve, sr, DEFAULT_N_TAPS));
        Self {
            channel,
            in_port,
            cal,
            spl_offset,
            mic_curve,
            loudness_fir,
            current_freq: freq_hz,
            reconnect: ReconnectState::new(),
            cwt_ring: std::collections::VecDeque::with_capacity(caps.cwt),
            cqt_ring: std::collections::VecDeque::with_capacity(caps.cqt),
            reass_ring: std::collections::VecDeque::with_capacity(caps.reass),
            fft_ring: std::collections::VecDeque::with_capacity(131_072),
            lf: LfState::new(),
            integrator: None,
            last_frac_ts: None,
            loudness: LoudnessState::new_mono(sr)
                .expect("sample_rate > 0 guaranteed by engine.sample_rate()"),
        }
    }
}

#[cfg(test)]
mod lf_band_tests {
    use super::{lf_band_enabled, lf_recompute_every, LfState, LF_OVERLAP};

    /// A zero here would pin `ticks_since_recompute` at zero forever: the
    /// `counter >= every` test would always fire, which happens to look
    /// like "recompute every tick" but is one rounding away from a
    /// division that never advances. Swept over every rate the daemon
    /// runs and the full legal interval range.
    #[test]
    fn recompute_cadence_is_never_zero() {
        for sr in [44_100u32, 48_000, 88_200, 96_000, 176_400, 192_000] {
            for lf_fft_n in [8192u32, 16_384, 32_768, 65_536, 131_072] {
                for interval in [0.002f64, 0.016, 0.05, 0.2, 1.0, 60.0] {
                    let every = lf_recompute_every(lf_fft_n, sr, interval);
                    assert!(
                        every >= 1,
                        "sr={sr} lf_fft_n={lf_fft_n} interval={interval} gave {every}"
                    );
                    assert!(every <= 4096);
                }
            }
        }
    }

    /// A degenerate interval must not divide by zero or produce a NaN
    /// that `as u32` would silently turn into 0.
    #[test]
    fn a_zero_interval_still_yields_a_usable_cadence() {
        let every = lf_recompute_every(65_536, 48_000, 0.0);
        assert!((1..=4096).contains(&every));
    }

    /// #173's target: the LF band recomputes on roughly a 136 ms hop at
    /// the 65536 / 48 kHz default, which is under one 0.2 s tick — so at
    /// the shipped defaults it recomputes every tick, and at a 16 ms tick
    /// it waits about eight.
    #[test]
    fn default_cadence_matches_the_overlap_hop() {
        let hop_s = 65_536.0 * (1.0 - LF_OVERLAP) / 48_000.0;
        assert!(
            (0.130..0.140).contains(&hop_s),
            "hop drifted from #173's ~136 ms: {hop_s}"
        );
        assert_eq!(lf_recompute_every(65_536, 48_000, 0.2), 1);
        assert_eq!(lf_recompute_every(65_536, 48_000, 0.016), 9);
    }

    /// The band buys nothing when the live FFT already has equal or finer
    /// resolution — running it there costs a 65536-point FFT for a splice
    /// that changes nothing.
    #[test]
    fn the_band_is_off_unless_its_fft_is_strictly_longer() {
        assert!(lf_band_enabled(65_536, 8192));
        assert!(!lf_band_enabled(8192, 8192));
        assert!(!lf_band_enabled(8192, 65_536));
    }

    /// A bin-count change (an `lf_fft_n` change mid-run) must rebuild the
    /// EMA, because feeding a mismatched vector length is the failure
    /// this guard exists to prevent. An unchanged count must not rebuild,
    /// or the smoothing would restart on every tick and #173's variance
    /// reduction would never accumulate.
    #[test]
    fn the_ema_rebuilds_only_when_the_bin_count_changes() {
        let mut lf = LfState::new();
        assert!(lf.ensure_ema(129), "first call must build");
        assert!(!lf.ensure_ema(129), "same bin count must reuse");
        lf.ema_last_ts = Some(std::time::Instant::now());
        assert!(lf.ensure_ema(257), "changed bin count must rebuild");
        assert!(
            lf.ema_last_ts.is_none(),
            "a rebuild must drop the stale timestamp, or the next dt is measured against the old EMA"
        );
    }

    /// Nothing is cached until the ring holds a full `lf_fft_n` block —
    /// a partial block would be a spectrum of mostly silence spliced in
    /// under the crossover.
    #[test]
    fn no_spectrum_is_cached_until_the_ring_fills() {
        let (sr, n) = (48_000u32, 256u32);
        let mut lf = LfState::new();
        let now = std::time::Instant::now();
        let chunk = vec![0.5f32; 64];
        for _ in 0..3 {
            lf.push_and_maybe_recompute(&chunk, n, sr, 1, now);
            assert!(lf.spec_cache.is_none(), "cached from a partial ring");
        }
        lf.push_and_maybe_recompute(&chunk, n, sr, 1, now);
        assert!(lf.spec_cache.is_some(), "a full ring must produce a column");
    }

    /// With a cadence of N ticks, a full ring recomputes once and then
    /// waits N ticks before the next one.
    #[test]
    fn the_cadence_counter_paces_recomputes() {
        let (sr, n) = (48_000u32, 256u32);
        let mut lf = LfState::new();
        let now = std::time::Instant::now();
        let chunk = vec![0.25f32; 256];
        lf.push_and_maybe_recompute(&chunk, n, sr, 3, now);
        assert_eq!(lf.ticks_since_recompute, 0, "first full ring recomputes");
        for expected in 1..=3 {
            lf.push_and_maybe_recompute(&chunk, n, sr, 3, now);
            assert_eq!(lf.ticks_since_recompute, expected);
        }
        lf.push_and_maybe_recompute(&chunk, n, sr, 3, now);
        assert_eq!(lf.ticks_since_recompute, 0, "counter reaching 3 recomputes");
    }

    /// Disabling the band drops every piece of state together. A partial
    /// clear would let a re-enable splice a stale spectrum computed at the
    /// old `lf_fft_n`.
    #[test]
    fn clearing_drops_all_five_pieces_of_state() {
        let (sr, n) = (48_000u32, 256u32);
        let mut lf = LfState::new();
        lf.push_and_maybe_recompute(&vec![0.1f32; 256], n, sr, 1, std::time::Instant::now());
        assert!(lf.is_stale(), "a fed band holds state");
        lf.clear();
        assert!(!lf.is_stale());
        assert!(lf.ring.is_empty());
        assert!(lf.spec_cache.is_none());
        assert!(lf.ema.is_none());
        assert!(lf.ema_last_ts.is_none());
        assert_eq!(lf.ticks_since_recompute, u32::MAX);
    }
}
