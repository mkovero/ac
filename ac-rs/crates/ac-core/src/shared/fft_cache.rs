//! Thread-local FFT plan / Hann window / axis caches.
//!
//! Used by both the Tier 1 THD analyzer (`measurement::thd`) and the
//! Tier 2 spectrum path (`visualize::spectrum`). Keeping the caches
//! here means a single source of truth per worker thread — the UI FFT
//! ladder has ~7 entries, so the plan cache is bounded.

use std::cell::RefCell;
use std::collections::HashMap;
use std::f64::consts::PI;
use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex};

thread_local! {
    static REAL_FFT_PLANS: RefCell<HashMap<usize, Arc<dyn RealToComplex<f64>>>> =
        RefCell::new(HashMap::new());
    /// Keyed by length, like the plans: one entry per N would rebuild the
    /// window on every call of a thread that alternates sizes, as a
    /// monitor channel does between its 8192-point THD FFT and its
    /// 65536-point LF band (73k cosines a tick per channel).
    static HANN_CACHE: RefCell<HashMap<usize, HannWindow>> = RefCell::new(HashMap::new());
    static AXES_CACHE: RefCell<AxesCache> = RefCell::new(AxesCache::default());
}

struct HannWindow {
    win: Vec<f64>,
    wc: f64,
}

#[derive(Default)]
struct AxesCache {
    n: usize,
    sr: u32,
    freqs: Vec<f64>,
    t: Vec<f64>,
}

/// Return (clone of) the forward real FFT plan for length `n`.
pub(crate) fn real_fft_plan(n: usize) -> Arc<dyn RealToComplex<f64>> {
    REAL_FFT_PLANS.with(|cell| {
        cell.borrow_mut()
            .entry(n)
            .or_insert_with(|| RealFftPlanner::<f64>::new().plan_fft_forward(n))
            .clone()
    })
}

/// Run `f` with borrowed access to the cached Hann window for length `n`
/// and its **coherent-gain** normalization constant `wc = mean(w[i])`.
///
/// Callers normalize their FFT magnitudes as `|FFT[k]| / ((n/2) · wc)` so
/// that an integer-bin sine of peak amplitude `A` reads back as `A` at the
/// peak bin — i.e. a 0 dBFS digital sine produces `fundamental_dbfs = 0`.
///
/// (Pre-2026-05 the cache stored the window RMS `sqrt(mean(w²)) ≈ 0.6124`
/// instead, inherited from a Python reference. With that value every FFT
/// magnitude — `fundamental_dbfs`, `spectrum`, harmonic levels — read
/// `20·log10(0.5/0.6124) ≈ 1.78 dB` low, regardless of N. THD/THD+N were
/// unaffected because they're amplitude ratios; `linear_rms` was unaffected
/// because it's time-domain. See the FF400 loopback verification on
/// 2026-05-01: a 0 dBu/-18.6 dBFS played sine read as -12.6 dBFS captured
/// where -10.8 was expected after the cal-modelled hardware gain plus
/// scallop, with the residual ≈1.8 dB tracking exactly to this constant.)
pub(crate) fn with_hann_window<R>(n: usize, f: impl FnOnce(&[f64], f64) -> R) -> R {
    HANN_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        let w = cache.entry(n).or_insert_with(|| {
            let win: Vec<f64> = (0..n)
                .map(|i| 0.5 * (1.0 - (2.0 * PI * i as f64 / (n - 1) as f64).cos()))
                .collect();
            let wc = win.iter().sum::<f64>() / n as f64;
            HannWindow { win, wc }
        });
        f(&w.win, w.wc)
    })
}

/// Return a copy of the cached frequency axis (`k · sr / n`, `k = 0..n/2`).
pub(crate) fn freq_axis(n: usize, sr: u32) -> Vec<f64> {
    AXES_CACHE.with(|cell| {
        let mut c = cell.borrow_mut();
        if c.n != n || c.sr != sr {
            let half = n / 2 + 1;
            c.freqs.clear();
            c.freqs.reserve(half);
            for k in 0..half {
                c.freqs.push(k as f64 * sr as f64 / n as f64);
            }
            c.t.clear();
            c.t.reserve(n);
            for i in 0..n {
                c.t.push(i as f64 / sr as f64);
            }
            c.n = n;
            c.sr = sr;
        }
        c.freqs.clone()
    })
}
