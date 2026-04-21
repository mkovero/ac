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
    static HANN_CACHE: RefCell<HannCache> = RefCell::new(HannCache::default());
    static AXES_CACHE: RefCell<AxesCache> = RefCell::new(AxesCache::default());
}

#[derive(Default)]
struct HannCache {
    n: usize,
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
/// and its RMS normalization constant `wc`.
pub(crate) fn with_hann_window<R>(n: usize, f: impl FnOnce(&[f64], f64) -> R) -> R {
    HANN_CACHE.with(|cell| {
        let mut c = cell.borrow_mut();
        if c.n != n {
            c.win.clear();
            c.win.reserve(n);
            for i in 0..n {
                c.win.push(0.5 * (1.0 - (2.0 * PI * i as f64 / (n - 1) as f64).cos()));
            }
            c.wc = (c.win.iter().map(|w| w * w).sum::<f64>() / n as f64).sqrt();
            c.n = n;
        }
        f(&c.win, c.wc)
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
