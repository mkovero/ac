//! Tier 1 — Farina exponential-sweep impulse response measurement.
//!
//! Per Farina 2000, *Simultaneous measurement of impulse response and
//! distortion with a swept-sine technique*, AES 108th convention preprint
//! #5093, §2 "Theoretical basis". Verified against the full preprint at
//! `stddocs/iec-full/Simultaneous_Measurement_of_Impulse_Response_and_D.pdf`:
//! the log-sweep `x(t) = sin[K·(e^(t/L) − 1)]` with `K = T·ω1/ln(ω2/ω1)`
//! and `L = T/ln(ω2/ω1)`, the `exp(-t/L)` inverse-filter envelope, and
//! the harmonic offset `Δt_N = T·ln(N)/ln(ω2/ω1)` all match the formulae
//! implemented below.
//!
//! The technique:
//! 1. Drive the DUT with a logarithmic (exponential) sine sweep `x(t)`
//!    covering `[f1, f2]` over `T` seconds.
//! 2. Record the response `y(t)`.
//! 3. Convolve `y` with the time-reversed, amplitude-modulated inverse
//!    filter `x_inv(t)` — Farina's closed-form inverse that makes
//!    `x(t) ∗ x_inv(t) ≈ δ(t−T)`.
//! 4. The linear IR appears centred at the end of the convolution
//!    (offset `≈ N−1` for equal-length sweeps). k-th-order harmonic IRs
//!    appear earlier at known offsets
//!    `Δt_k = (T / ln(f2/f1)) · ln(k)` seconds before the linear IR,
//!    because the k-th harmonic of an exponential sweep is the
//!    fundamental of a time-shifted version of the same sweep.
//!
//! Time-gating the pre-impulse region into windows centred at each
//! `Δt_k` yields per-order harmonic impulse responses, suitable for
//! calculating a frequency-resolved THD curve.
//!
//! The module is split by stage: `deconv` generates the sweep and its
//! inverse filter and convolves them, `harmonics` cuts the result into
//! per-order impulse responses, `tail_decay` runs the ISO 18233 §6.3.2
//! capture-adequacy check on it, and `gated` turns the linear IR into a
//! quasi-anechoic frequency response. Everything is re-exported here, so
//! callers keep using `measurement::sweep::<item>` paths.

use anyhow::{bail, Result};

use crate::measurement::report::StandardsCitation;

mod deconv;
mod gated;
mod harmonics;
mod tail_decay;

pub use deconv::{deconvolve_full, inverse_sweep, log_sweep};
pub use gated::{gated_frequency_response, tukey_window, GatedResponsePoint};
pub use harmonics::{
    extract_irs, pre_impulse_region_len, pre_impulse_snr_db, DeconvolvedIrs, HarmonicIr,
};
pub use tail_decay::{check_tail_decay, TailDecayCheck};

/// Parameters for a Farina log sweep.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SweepParams {
    pub f1_hz: f64,
    pub f2_hz: f64,
    pub duration_s: f64,
    pub sample_rate: u32,
}

impl SweepParams {
    pub fn validate(&self) -> Result<()> {
        if self.sample_rate == 0 {
            bail!("sample_rate must be positive");
        }
        if !self.f1_hz.is_finite() || !self.f2_hz.is_finite() || !self.duration_s.is_finite() {
            bail!("non-finite parameter");
        }
        if self.f1_hz <= 0.0 {
            bail!("f1_hz must be positive (got {})", self.f1_hz);
        }
        if self.f2_hz <= self.f1_hz {
            bail!(
                "f2_hz must exceed f1_hz (got f1={}, f2={})",
                self.f1_hz,
                self.f2_hz
            );
        }
        if self.duration_s <= 0.0 {
            bail!("duration_s must be positive (got {})", self.duration_s);
        }
        if self.f2_hz >= self.sample_rate as f64 * 0.5 {
            bail!(
                "f2_hz must be below Nyquist ({} Hz); got {}",
                self.sample_rate as f64 * 0.5,
                self.f2_hz
            );
        }
        Ok(())
    }

    pub fn n_samples(&self) -> usize {
        (self.duration_s * self.sample_rate as f64).round() as usize
    }

    /// `L = T / ln(f2/f1)` — the exponential-sweep time constant.
    /// Instantaneous frequency is `f1 · exp(t / L)`.
    pub fn time_constant(&self) -> f64 {
        self.duration_s / (self.f2_hz / self.f1_hz).ln()
    }

    /// Time offset at which the k-th harmonic IR appears BEFORE the
    /// linear IR in a Farina deconvolution, in seconds.
    ///
    /// `Δt_k = L · ln(k)`. `k = 1` returns 0.
    pub fn harmonic_time_offset_s(&self, k: u32) -> f64 {
        if k == 0 {
            return 0.0;
        }
        self.time_constant() * (k as f64).ln()
    }
}

/// Citation for a `MeasurementReport` emitted from a Farina-sweep run.
///
/// Two standards apply, for two different things. The theoretical basis —
/// the log sweep, the closed-form inverse filter, the harmonic-order
/// offsets — is Farina's; it is not covered by an IEC or AES standard, so
/// the canonical reference is the AES 108th Convention preprint #5093 by
/// Angelo Farina, "Simultaneous measurement of impulse response and
/// distortion with a swept-sine technique" (Paris, 2000). Verified against
/// the full preprint PDF under `stddocs/iec-full/`.
///
/// The swept-sine method itself, separately, is now covered by a normative
/// standard: ISO 18233:2006 Annex B (normative), "Swept-sine method". This
/// issue adds that reference; it does not replace the preprint, which
/// remains the correct citation for the theoretical basis.
///
/// `verified` covers the whole citation. It stays `false` until a human
/// has cross-checked the Annex B text against `stddocs/iso-full/` — an
/// agent may prepare this citation but must not flip that flag.
pub fn citation() -> StandardsCitation {
    // Built by extending `farina_citation` rather than restating it, so
    // the preprint half cannot drift between the two functions — the
    // whole point of `farina_citation` is that it is the same reference
    // with the ISO half withheld.
    let farina = farina_citation();
    StandardsCitation {
        standard: format!("{}; ISO 18233:2006 Annex B (normative)", farina.standard),
        clause: format!("{}; Annex B (normative) Swept-sine method", farina.clause),
        verified: false,
    }
}

/// `deconvolve_full` recovers the IR via `fft_linear_convolve` — a
/// *linear* deconvolution. ISO 18233 §B.5 documents the consequence: the
/// tail past the peak is a decaying noise floor, increasingly low-pass
/// filtered toward its end, and the standard requires callers state this
/// "so as not to confuse the decreasing noise floor with the reverberant
/// tail of the room". Any reader of the linear IR (printed summary or
/// persisted [`crate::measurement::report::MeasurementReport::notes`])
/// needs this stated, since nothing else in the report tells them.
pub const LINEAR_DECONV_TAIL_NOTE: &str =
    "The decaying tail after the peak is a linear-deconvolution artefact \
     (fft_linear_convolve), increasingly low-pass filtered toward its end \
     — not the measured system's reverberant decay. See ISO 18233 §B.5.";

/// The instant, measured from the linear-IR peak, past which the captured
/// `full` deconvolution can only carry noise smeared by the inverse
/// filter's own kernel — never real system response. The Farina inverse
/// filter (`inverse_sweep`) is `duration_s` long, so any true system
/// response has been fully convolved out by `duration_s` past the peak;
/// content past that point is background noise passed through a kernel
/// whose own frequency content narrows toward the end of the sweep. This
/// is derived from the sweep parameters directly, not estimated —
/// see [`MeasurementReport::ir_stats`] and issue #284, acceptance
/// criterion 5.
///
/// [`MeasurementReport::ir_stats`]: crate::measurement::report::MeasurementReport::ir_stats
pub fn noise_tail_start_s(p: &SweepParams) -> f64 {
    p.duration_s
}

/// Farina-preprint-only citation for the theoretical basis (log sweep,
/// closed-form inverse filter, harmonic-order offsets) — same text as the
/// preprint half of [`citation`], deliberately *without* the ISO
/// 18233:2006 Annex B half.
///
/// Scoped to [`crate::measurement::report::MeasurementData::GatedFrequencyResponse`]
/// (#284): ISO 18233 §1 restricts its own scope to substituting for
/// classical-method standards (ISO 140, ISO 3382, ISO 17497-1) and §9(c)
/// requires the test report to additionally name that classical
/// counterpart whenever ISO 18233 is cited — a quasi-anechoic
/// loudspeaker/system capture has no classical counterpart, so it cannot
/// carry that citation. [`citation`] itself is unchanged (still packs both
/// standards into one string) since the pre-existing `ImpulseResponse`
/// payload's use of it is out of scope here — see PR #305 review.
pub fn farina_citation() -> StandardsCitation {
    StandardsCitation {
        standard: "Farina, AES 108th Convention preprint #5093 (2000)".into(),
        clause: "§2 Theoretical basis (log sweep, inverse filter, harmonic offsets)".into(),
        verified: false,
    }
}

/// Citation for the [`crate::measurement::report::MeasurementData::GatedFrequencyResponse`]
/// payload (#284): a quasi-anechoic frequency response derived by
/// time-gating a Farina-swept-sine impulse response, distinct from the
/// impulse-response payload's own citation ([`citation`]) — both apply,
/// in relevance order. Paired with [`farina_citation`], not [`citation`]
/// — see [`farina_citation`]'s doc for why the ISO 18233 half must not
/// come along for this payload.
///
/// `verified` stays `false` until a human cross-checks Annex A.4.5 against
/// the published AES17-2020 text at
/// `stddocs/iec-full/aes17_2020_aes_standard_method_for_digital_audio_engineering_measurement.pdf`.
pub fn gated_response_citation() -> StandardsCitation {
    StandardsCitation {
        standard: "AES17-2020".into(),
        clause: "Annex A.4.5 (informative) (quasi-anechoic frequency response via time-gated impulse response)"
            .into(),
        verified: false,
    }
}

/// Fixtures shared by every submodule's tests: one sweep parameter set the
/// whole module measures against, so a change to it moves all of them
/// together rather than only the file it happens to live in.
#[cfg(test)]
pub(super) mod testkit {
    use super::SweepParams;

    pub const SR: u32 = 48_000;

    pub fn p_default() -> SweepParams {
        SweepParams {
            f1_hz: 20.0,
            f2_hz: 20_000.0,
            duration_s: 1.0,
            sample_rate: SR,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement::sweep::testkit::*;

    #[test]
    fn params_validate() {
        assert!(p_default().validate().is_ok());
        let mut p = p_default();
        p.f1_hz = 0.0;
        assert!(p.validate().is_err());
        let mut p = p_default();
        p.f2_hz = p.f1_hz;
        assert!(p.validate().is_err());
        let mut p = p_default();
        p.duration_s = 0.0;
        assert!(p.validate().is_err());
        let mut p = p_default();
        p.f2_hz = 30_000.0; // above Nyquist/2
        assert!(p.validate().is_err());
    }

    #[test]
    fn harmonic_time_offsets_are_log_spaced() {
        let p = p_default();
        let dt2 = p.harmonic_time_offset_s(2);
        let dt3 = p.harmonic_time_offset_s(3);
        let dt4 = p.harmonic_time_offset_s(4);
        // ln(4) = 2·ln(2)
        assert!((dt4 - 2.0 * dt2).abs() < 1e-12);
        // ln(3) / ln(2) ≈ 1.585
        let ratio = dt3 / dt2;
        assert!((ratio - 3f64.ln() / 2f64.ln()).abs() < 1e-12);
    }

    #[test]
    fn citation_shape() {
        let c = citation();
        // Preprint: theoretical basis. Must not be dropped by this change.
        assert!(c.standard.contains("Farina"));
        assert!(c.clause.contains("§2"));
        // ISO 18233:2006 Annex B: normative swept-sine method, added
        // alongside the preprint (#291).
        assert!(c.standard.contains("ISO 18233:2006"));
        assert!(c.standard.contains("Annex B"));
        assert!(c.clause.contains("Annex B"));
        // Human gate: Annex B text not yet cross-checked, so the combined
        // citation is not `verified` yet. An agent must not flip this.
        assert!(!c.verified);
    }

    // ─── check_tail_decay (#282 acceptance criterion 6) ────────────

    #[test]
    fn noise_tail_start_s_is_the_sweep_duration() {
        let p = p_default();
        assert_eq!(noise_tail_start_s(&p), p.duration_s);
        let mut p2 = p_default();
        p2.duration_s = 2.5;
        assert_eq!(noise_tail_start_s(&p2), 2.5);
    }

    #[test]
    fn gated_frequency_response_citation_shape() {
        let c = gated_response_citation();
        assert!(c.standard.contains("AES17"));
        assert!(c.clause.contains("A.4"));
        assert!(!c.verified);
    }

    /// PR #305 review, correctness issue 1: the gated-response payload
    /// must not end up citing ISO 18233 by way of reusing `citation()`
    /// wholesale — `farina_citation()` carries the preprint only.
    #[test]
    fn farina_citation_excludes_iso_18233() {
        let c = farina_citation();
        assert!(c.standard.contains("Farina"));
        assert!(
            !c.standard.contains("ISO 18233"),
            "gated payload's citation must not carry ISO 18233: {c:?}"
        );
        assert!(!c.verified);
    }
}
