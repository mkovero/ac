//! Tier 2 — Time integration on per-band dBFS values.
//!
//! Exponentially-weighted energy averaging (IEC 61672 fast/slow time
//! constants) and unbounded equivalent-level integration (Leq), applied
//! per band to the live fractional-octave output. All integration runs
//! in the linear-power domain; inputs and outputs are in dBFS.
//!
//! **Display-only.** Same caveat as the underlying
//! `visualize::fractional_octave` path — the per-band levels come from a
//! Morlet CWT aggregation, not from IEC 61260 filters, so these
//! integrators must not be quoted as IEC 61672 SPL readings. The time
//! constants and formulas match the standard; the upstream band
//! energies do not.
//!
//! # Modes
//!
//! | Mode  | τ       | Module type          |
//! |-------|---------|----------------------|
//! | Fast  | 125 ms  | [`EmaIntegrator`]    |
//! | Slow  | 1 s     | [`EmaIntegrator`]    |
//! | Leq   | —       | [`LeqIntegrator`]    |
//!
//! # EMA formula
//!
//! For each band, with linear power `p = 10^(dBFS/10)`, `dt` the interval
//! since the previous update, and `α = exp(-dt/τ)`:
//!
//! ```text
//! state = state * α + p * (1 - α)
//! output_dBFS = 10 * log10(state)
//! ```
//!
//! At steady-state input, `state → p`. Starting from silence, after
//! `dt = τ` the response reaches `1 - 1/e ≈ 0.632` of the step.
//!
//! # Leq formula
//!
//! Cumulative energy, reset on demand:
//!
//! ```text
//! sum_pow += p * dt
//! total_s += dt
//! Leq_dBFS = 10 * log10(sum_pow / total_s)
//! ```

/// IEC 61672-1 "fast" time constant.
pub const TAU_FAST_S: f64 = 0.125;

/// IEC 61672-1 "slow" time constant.
pub const TAU_SLOW_S: f64 = 1.0;

/// Floor for dB output when a band has accumulated no energy.
const MIN_DBFS: f64 = -200.0;

fn db_to_pow(db: f64) -> f64 {
    10.0_f64.powf(db / 10.0)
}

fn pow_to_db(p: f64) -> f64 {
    if p.is_nan() || p <= 0.0 {
        MIN_DBFS
    } else {
        (10.0 * p.log10()).max(MIN_DBFS)
    }
}

/// Exponentially-weighted running average on per-band power. Construct
/// with [`EmaIntegrator::new`]; feed each fractional-octave frame via
/// [`EmaIntegrator::update`]; call [`EmaIntegrator::reset`] to zero the
/// state without re-allocating.
#[derive(Debug, Clone)]
pub struct EmaIntegrator {
    pub tau_s:    f64,
    /// Internal state: smoothed linear power per band. Public for tests.
    state_pow:    Vec<f64>,
    /// `false` until the first [`update`]. Primes the state with the
    /// first input so callers don't see a spurious startup transient
    /// from the all-zeros initial condition.
    ///
    /// [`update`]: EmaIntegrator::update
    primed:       bool,
}

impl EmaIntegrator {
    pub fn new(tau_s: f64, n_bands: usize) -> Self {
        assert!(tau_s > 0.0, "tau_s must be positive");
        Self {
            tau_s,
            state_pow: vec![0.0; n_bands],
            primed:    false,
        }
    }

    /// Feed one per-band dBFS vector with the elapsed interval since
    /// the previous update. Returns the smoothed per-band dBFS readout.
    ///
    /// `levels_dbfs.len()` must match the `n_bands` passed at
    /// construction; mismatches panic.
    pub fn update(&mut self, levels_dbfs: &[f64], dt_s: f64) -> Vec<f64> {
        assert_eq!(levels_dbfs.len(), self.state_pow.len());
        assert!(dt_s > 0.0, "dt_s must be positive");

        if !self.primed {
            for (s, &db) in self.state_pow.iter_mut().zip(levels_dbfs) {
                *s = db_to_pow(db);
            }
            self.primed = true;
            return levels_dbfs.to_vec();
        }

        let alpha = (-dt_s / self.tau_s).exp();
        for (s, &db) in self.state_pow.iter_mut().zip(levels_dbfs) {
            let p = db_to_pow(db);
            *s = *s * alpha + p * (1.0 - alpha);
        }
        self.state_pow.iter().map(|&p| pow_to_db(p)).collect()
    }

    /// Zero the internal state. The next [`update`] re-primes from its
    /// input, matching fresh-construction semantics.
    ///
    /// [`update`]: EmaIntegrator::update
    pub fn reset(&mut self) {
        for s in self.state_pow.iter_mut() {
            *s = 0.0;
        }
        self.primed = false;
    }

    pub fn is_primed(&self) -> bool {
        self.primed
    }

    pub fn state_len(&self) -> usize {
        self.state_pow.len()
    }
}

/// Unbounded per-band equivalent level (Leq) integrator. Accumulates
/// `power × dt` per band plus total elapsed time; the readout is
/// `10·log10(sum_pow / total_s)`.
#[derive(Debug, Clone)]
pub struct LeqIntegrator {
    sum_pow: Vec<f64>,
    total_s: f64,
}

impl LeqIntegrator {
    pub fn new(n_bands: usize) -> Self {
        Self {
            sum_pow: vec![0.0; n_bands],
            total_s: 0.0,
        }
    }

    /// Accumulate one frame. Returns per-band Leq dBFS so far.
    pub fn update(&mut self, levels_dbfs: &[f64], dt_s: f64) -> Vec<f64> {
        assert_eq!(levels_dbfs.len(), self.sum_pow.len());
        assert!(dt_s > 0.0, "dt_s must be positive");
        for (s, &db) in self.sum_pow.iter_mut().zip(levels_dbfs) {
            *s += db_to_pow(db) * dt_s;
        }
        self.total_s += dt_s;
        self.current()
    }

    /// Current Leq readout without advancing the integrator.
    pub fn current(&self) -> Vec<f64> {
        if self.total_s <= 0.0 {
            return vec![MIN_DBFS; self.sum_pow.len()];
        }
        let inv_t = 1.0 / self.total_s;
        self.sum_pow.iter().map(|&s| pow_to_db(s * inv_t)).collect()
    }

    pub fn duration_s(&self) -> f64 {
        self.total_s
    }

    pub fn reset(&mut self) {
        for s in self.sum_pow.iter_mut() {
            *s = 0.0;
        }
        self.total_s = 0.0;
    }

    pub fn state_len(&self) -> usize {
        self.sum_pow.len()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    // ---- EMA ----

    #[test]
    fn ema_first_update_primes_to_input() {
        let mut ema = EmaIntegrator::new(TAU_FAST_S, 3);
        let out = ema.update(&[-20.0, -40.0, -60.0], 0.050);
        assert!(ema.is_primed());
        assert!(approx(out[0], -20.0, 1e-9));
        assert!(approx(out[1], -40.0, 1e-9));
        assert!(approx(out[2], -60.0, 1e-9));
    }

    #[test]
    fn ema_steady_state_tracks_input() {
        // After many τ worth of constant input, readout should equal input.
        let mut ema = EmaIntegrator::new(TAU_SLOW_S, 1);
        let dt = 0.050;
        ema.update(&[-30.0], dt);
        for _ in 0..500 {
            ema.update(&[-30.0], dt);
        }
        let out = ema.update(&[-30.0], dt);
        assert!(approx(out[0], -30.0, 1e-6));
    }

    #[test]
    fn ema_time_constant_reaches_one_minus_exp_after_tau() {
        // Prime to −∞ (linear 0), then step to 0 dBFS. After exactly
        // τ, the linear state should equal 1 − 1/e; translated back
        // to dB that's 10·log10(1 − 1/e) ≈ −4.343 dB below the step.
        let tau = TAU_FAST_S;
        let mut ema = EmaIntegrator::new(tau, 1);
        // Prime at −∞ via an explicit silence input.
        ema.update(&[MIN_DBFS], 1e-3);
        // Reset to drop the prime and re-prime at −∞ properly.
        ema.reset();
        // Prime at −∞ again but with small dt so the state is ~0 power.
        ema.update(&[MIN_DBFS], 1e-6);
        // Apply a 0 dBFS step across exactly τ seconds.
        let out = ema.update(&[0.0], tau);
        let expected_db = 10.0 * (1.0 - 1.0 / std::f64::consts::E).log10();
        assert!(
            approx(out[0], expected_db, 0.05),
            "expected ~{expected_db:.3} dB, got {:.3}",
            out[0]
        );
    }

    #[test]
    fn ema_fast_decays_quicker_than_slow() {
        // Prime to 0 dBFS, then step down to −60 dBFS and integrate.
        // After 250 ms, fast (τ=125 ms) must sit well below slow (τ=1s).
        let mut fast = EmaIntegrator::new(TAU_FAST_S, 1);
        let mut slow = EmaIntegrator::new(TAU_SLOW_S, 1);
        fast.update(&[0.0], 1e-3);
        slow.update(&[0.0], 1e-3);

        let dt = 0.025;
        for _ in 0..10 {
            fast.update(&[-60.0], dt);
            slow.update(&[-60.0], dt);
        }
        let f_now = fast.update(&[-60.0], dt);
        let s_now = slow.update(&[-60.0], dt);
        assert!(
            f_now[0] < s_now[0] - 5.0,
            "fast ({:.2}) not sufficiently below slow ({:.2})",
            f_now[0], s_now[0],
        );
    }

    #[test]
    fn ema_reset_reprimes_on_next_update() {
        let mut ema = EmaIntegrator::new(TAU_FAST_S, 2);
        ema.update(&[-10.0, -10.0], 0.01);
        assert!(ema.is_primed());
        ema.reset();
        assert!(!ema.is_primed());
        let out = ema.update(&[-50.0, -80.0], 0.01);
        assert!(approx(out[0], -50.0, 1e-9));
        assert!(approx(out[1], -80.0, 1e-9));
    }

    // ---- Leq ----

    #[test]
    fn leq_constant_signal_reads_signal_level() {
        let mut leq = LeqIntegrator::new(1);
        for _ in 0..100 {
            leq.update(&[-20.0], 0.01);
        }
        let out = leq.current();
        assert!(approx(out[0], -20.0, 1e-9));
        assert!(approx(leq.duration_s(), 1.0, 1e-9));
    }

    #[test]
    fn leq_averages_two_halves_in_energy_domain() {
        // 1 s at 0 dBFS then 1 s at −∞ dBFS. Energy domain: Leq =
        // 10·log10((1·1.0 + 1·0.0) / 2) = −3.010 dB.
        let mut leq = LeqIntegrator::new(1);
        for _ in 0..100 {
            leq.update(&[0.0], 0.01);
        }
        for _ in 0..100 {
            leq.update(&[MIN_DBFS], 0.01);
        }
        let out = leq.current();
        assert!(
            approx(out[0], -3.010299956639812, 1e-6),
            "expected −3.01 dB, got {:.4}",
            out[0]
        );
    }

    #[test]
    fn leq_before_any_update_is_floor() {
        let leq = LeqIntegrator::new(3);
        let out = leq.current();
        assert_eq!(out, vec![MIN_DBFS; 3]);
    }

    #[test]
    fn leq_reset_clears_history() {
        let mut leq = LeqIntegrator::new(1);
        for _ in 0..100 {
            leq.update(&[0.0], 0.01);
        }
        assert!(approx(leq.current()[0], 0.0, 1e-9));
        leq.reset();
        assert_eq!(leq.duration_s(), 0.0);
        assert_eq!(leq.current(), vec![MIN_DBFS]);

        leq.update(&[-40.0], 0.1);
        assert!(approx(leq.current()[0], -40.0, 1e-9));
    }

    #[test]
    fn per_band_integration_is_independent() {
        // Feed a band-dependent pattern and check each band tracks
        // independently across many updates.
        let mut ema = EmaIntegrator::new(TAU_FAST_S, 3);
        ema.update(&[-10.0, -20.0, -30.0], 1e-3);
        for _ in 0..200 {
            ema.update(&[-10.0, -20.0, -30.0], 0.01);
        }
        let out = ema.update(&[-10.0, -20.0, -30.0], 0.01);
        assert!(approx(out[0], -10.0, 1e-3));
        assert!(approx(out[1], -20.0, 1e-3));
        assert!(approx(out[2], -30.0, 1e-3));
    }
}
