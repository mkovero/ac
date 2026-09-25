//! The set statistics themselves: the 1/6-octave grid, band clipping,
//! per-band medians, gain spread, tonal stability, flatness and the
//! harmonic drive-tracking slope. Pure functions over numbers; `set`
//! decides which runs feed them.

use std::collections::BTreeMap;

use super::{HARMONIC_TRACK_TOLERANCE_DB, MIN_DRIVE_SPAN_DB, MIN_USED_RUNS, STATS_GRID_BPO};

/// A frequency band, Hz, inclusive at both ends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub lo_hz: f64,
    pub hi_hz: f64,
}

impl Band {
    pub fn new(lo_hz: f64, hi_hz: f64) -> Self {
        Self { lo_hz, hi_hz }
    }

    fn contains(&self, f: f64) -> bool {
        f >= self.lo_hz && f <= self.hi_hz
    }
}

/// Why a figure carries no value. Every variant names the measured
/// condition, so the printed line can say what was missing.
#[derive(Debug, Clone, PartialEq)]
pub enum NotComputed {
    /// Fewer than [`MIN_USED_RUNS`] runs were fit to aggregate.
    TooFewRuns { used: usize },
    /// The band starts at or above the sweep's stop frequency.
    AboveSweepStop { f2_hz: f64 },
    /// The band ends at or below the sweep's start frequency.
    BelowSweepStart { f1_hz: f64 },
    /// The band ends at or below the lowest frequency the gate resolves.
    BelowGateLimit { f_low_hz: f64 },
    /// No 1/6-octave cell with a finite value in every used run falls in
    /// the band.
    NoGridPoints,
    /// The used runs span less than [`MIN_DRIVE_SPAN_DB`] of drive.
    DriveSpan { span_db: f64 },
    /// No finite harmonic level in enough used runs to fit a slope.
    NoLevel,
}

/// A computed figure, or why it could not be computed.
#[derive(Debug, Clone, PartialEq)]
pub enum Figure {
    Value(f64),
    NotComputed(NotComputed),
}

impl Figure {
    pub fn value(&self) -> Option<f64> {
        match self {
            Figure::Value(v) => Some(*v),
            Figure::NotComputed(_) => None,
        }
    }
}

/// A figure with a limit: pass when the value is at or under it.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    /// The band actually used — clipped to the sweep and the gate — or the
    /// nominal band when clipping left nothing.
    pub band: Band,
    pub value: Figure,
    pub limit_db: f64,
}

impl Verdict {
    /// `None` when the value was not computed.
    pub fn passed(&self) -> Option<bool> {
        self.value.value().map(|v| v <= self.limit_db)
    }
}

/// A readout with no limit.
#[derive(Debug, Clone, PartialEq)]
pub struct Flatness {
    pub band: Band,
    pub value: Figure,
}

/// What a harmonic's slope against drive says about its level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HarmonicReading {
    /// Within [`HARMONIC_TRACK_TOLERANCE_DB`] of the order's expectation:
    /// the level is a value.
    TracksDrive,
    /// Below the expectation by more than the tolerance: the term does not
    /// grow with drive, so its level is an upper bound on the floor, not a
    /// measurement of the device.
    FloorLimited,
    /// Above the expectation by more than the tolerance. The level is
    /// real; its growth exceeds what the order predicts. Not a cause.
    RisesFaster,
}

/// One harmonic order across the used runs.
#[derive(Debug, Clone, PartialEq)]
pub struct HarmonicRow {
    pub order: u32,
    /// Level re fundamental at the highest used drive, dB. `None` when
    /// that run carries no finite level for this order.
    pub level_db: Option<f64>,
    /// Least-squares slope of level against drive, dB per 10 dB.
    pub slope: Figure,
    /// `(order − 1)·10` dB per 10 dB: order *n* re fundamental.
    pub expected_db: f64,
    /// `(drive dBFS, level dB)` for every used run with a finite level.
    pub points: Vec<(f64, f64)>,
}

impl HarmonicRow {
    /// The reading the slope supports; `None` when no slope was fitted.
    pub fn reading(&self) -> Option<HarmonicReading> {
        let slope = self.slope.value()?;
        Some(if slope < self.expected_db - HARMONIC_TRACK_TOLERANCE_DB {
            HarmonicReading::FloorLimited
        } else if slope > self.expected_db + HARMONIC_TRACK_TOLERANCE_DB {
            HarmonicReading::RisesFaster
        } else {
            HarmonicReading::TracksDrive
        })
    }
}

/// A per-run chart series, `(x, y)`.
#[derive(Debug, Clone, PartialEq)]
pub struct RunSeries {
    /// 1-based run number in set order.
    pub run: usize,
    pub points: Vec<(f64, f64)>,
}

/// A run's response on the 1/6-octave grid, keyed by the cell index `k`
/// of the centre `1000·2^(k/6)` Hz.
pub(super) type Grid = BTreeMap<i64, f64>;

/// Centre of 1/6-octave cell `k`.
pub(super) fn centre_hz(k: i64) -> f64 {
    1_000.0 * 2f64.powf(k as f64 / STATS_GRID_BPO as f64)
}

/// Power means of `(freq, dB)` points in 1/6-octave cells
/// `[fc·2^(−1/12), fc·2^(1/12))`. A cell with no finite bin is absent,
/// never zero: a gap is not a reading.
pub(super) fn sixth_octave_means(points: &[(f64, f64)]) -> Grid {
    let bpo = STATS_GRID_BPO as f64;
    let mut acc: BTreeMap<i64, (f64, usize)> = BTreeMap::new();
    for &(f, db) in points {
        if !(f > 0.0 && f.is_finite() && db.is_finite()) {
            continue;
        }
        let k = (bpo * (f / 1_000.0).log2() + 0.5).floor() as i64;
        let e = acc.entry(k).or_insert((0.0, 0));
        e.0 += 10f64.powf(db / 10.0);
        e.1 += 1;
    }
    acc.into_iter()
        .filter(|(_, (_, n))| *n > 0)
        .map(|(k, (sum, n))| (k, 10.0 * (sum / n as f64).log10()))
        .filter(|(_, v)| v.is_finite())
        .collect()
}

/// Clip a nominal band to what the sweep and the gate resolve.
pub(super) fn clip_band(
    nominal: Band,
    f1_hz: f64,
    f2_hz: f64,
    f_low_hz: Option<f64>,
) -> Result<Band, NotComputed> {
    if nominal.lo_hz >= f2_hz {
        return Err(NotComputed::AboveSweepStop { f2_hz });
    }
    if nominal.hi_hz <= f1_hz {
        return Err(NotComputed::BelowSweepStart { f1_hz });
    }
    let f_low = f_low_hz.unwrap_or(0.0);
    if nominal.hi_hz <= f_low {
        return Err(NotComputed::BelowGateLimit { f_low_hz: f_low });
    }
    Ok(Band::new(
        nominal.lo_hz.max(f1_hz).max(f_low),
        nominal.hi_hz.min(f2_hz),
    ))
}

/// Cells inside `band` where every grid carries a value.
fn band_keys(grids: &[&Grid], band: Band) -> Vec<i64> {
    let Some(first) = grids.first() else {
        return Vec::new();
    };
    first
        .keys()
        .copied()
        .filter(|k| band.contains(centre_hz(*k)))
        .filter(|k| grids.iter().all(|g| g.contains_key(k)))
        .collect()
}

fn median(values: &[f64]) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n == 0 {
        f64::NAN
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

fn band_median(grid: &Grid, keys: &[i64]) -> f64 {
    median(&keys.iter().map(|k| grid[k]).collect::<Vec<_>>())
}

/// The shared preamble of every band statistic: enough runs, a band that
/// survives clipping, and at least one cell in it.
fn prepare(grids: &[&Grid], band: Result<Band, NotComputed>) -> Result<Vec<i64>, NotComputed> {
    if grids.len() < MIN_USED_RUNS {
        return Err(NotComputed::TooFewRuns { used: grids.len() });
    }
    let keys = band_keys(grids, band?);
    if keys.is_empty() {
        return Err(NotComputed::NoGridPoints);
    }
    Ok(keys)
}

fn figure(r: Result<f64, NotComputed>) -> Figure {
    match r {
        Ok(v) => Figure::Value(v),
        Err(n) => Figure::NotComputed(n),
    }
}

/// Each run's gain: the median of its grid inside `band`, over the cells
/// every run carries. `None` per run when the band gives nothing.
pub(super) fn gains(grids: &[&Grid], band: Result<Band, NotComputed>) -> Vec<Option<f64>> {
    let keys = match band {
        Ok(b) => band_keys(grids, b),
        Err(_) => Vec::new(),
    };
    grids
        .iter()
        .map(|g| (!keys.is_empty()).then(|| band_median(g, &keys)))
        .collect()
}

/// `max g_r − min g_r`. Nothing is subtracted for drive: `plot_ir` already
/// divides the IR by the stimulus amplitude, so an unchanged device reads
/// an unchanged gain at every drive.
pub(super) fn gain_spread(grids: &[&Grid], band: Result<Band, NotComputed>) -> Figure {
    figure(prepare(grids, band).map(|keys| {
        let g: Vec<f64> = grids.iter().map(|g| band_median(g, &keys)).collect();
        let max = g.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let min = g.iter().copied().fold(f64::INFINITY, f64::min);
        max - min
    }))
}

/// Each run normalised to its own band median, `n_r(f)`, and their mean
/// `m(f)`, over `keys`.
fn normalised(grids: &[&Grid], keys: &[i64]) -> (Vec<Vec<f64>>, Vec<f64>) {
    let n: Vec<Vec<f64>> = grids
        .iter()
        .map(|g| {
            let med = band_median(g, keys);
            keys.iter().map(|k| g[k] - med).collect()
        })
        .collect();
    let m = (0..keys.len())
        .map(|i| n.iter().map(|r| r[i]).sum::<f64>() / n.len() as f64)
        .collect();
    (n, m)
}

/// `max_{r, f∈B} |n_r(f) − m(f)|`: how far any run's shape strays from the
/// series mean inside the band.
pub(super) fn tonal_stability(grids: &[&Grid], band: Result<Band, NotComputed>) -> Figure {
    figure(prepare(grids, band).map(|keys| {
        let (n, m) = normalised(grids, &keys);
        n.iter()
            .flat_map(|r| r.iter().zip(&m).map(|(a, b)| (a - b).abs()))
            .fold(0.0, f64::max)
    }))
}

/// `n_r(f) − m(f)` per run over the band, for the level-deviation chart.
/// Empty when the band gives nothing.
pub(super) fn deviations(grids: &[&Grid], band: Result<Band, NotComputed>) -> Vec<Vec<(f64, f64)>> {
    let Ok(keys) = prepare(grids, band) else {
        return vec![Vec::new(); grids.len()];
    };
    let (n, m) = normalised(grids, &keys);
    n.iter()
        .map(|r| {
            keys.iter()
                .zip(r.iter().zip(&m))
                .map(|(k, (a, b))| (centre_hz(*k), a - b))
                .collect()
        })
        .collect()
}

/// The mean of the runs' grids, `M(f)`, over every cell all of them carry.
pub(super) fn mean_grid(grids: &[&Grid]) -> Grid {
    let Some(first) = grids.first() else {
        return Grid::new();
    };
    first
        .keys()
        .filter(|k| grids.iter().all(|g| g.contains_key(k)))
        .map(|k| {
            (
                *k,
                grids.iter().map(|g| g[k]).sum::<f64>() / grids.len() as f64,
            )
        })
        .collect()
}

/// `max_{f∈B} |M(f) − median_B(M)|`: the mean response's excursion from
/// its own median inside the band. Never normalised against a median
/// shared with another band — that median moves when the other band
/// moves.
pub(super) fn flatness(grids: &[&Grid], band: Result<Band, NotComputed>) -> Figure {
    figure(prepare(grids, band).map(|keys| {
        let mean = mean_grid(grids);
        let med = band_median(&mean, &keys);
        keys.iter()
            .map(|k| (mean[k] - med).abs())
            .fold(0.0, f64::max)
    }))
}

/// Harmonic order `n` re fundamental: `10·log10(Σ h_n² / Σ h_1²)`, dB.
/// `None` when either IR carries no energy.
pub(super) fn harmonic_level_db(harmonic: &[f64], linear: &[f64]) -> Option<f64> {
    let e_n: f64 = harmonic.iter().map(|v| v * v).sum();
    let e_1: f64 = linear.iter().map(|v| v * v).sum();
    let db = 10.0 * (e_n / e_1).log10();
    (e_n > 0.0 && e_1 > 0.0 && db.is_finite()).then_some(db)
}

/// Least-squares slope of `(drive, level)`, scaled to dB per 10 dB.
fn slope_per_10db(points: &[(f64, f64)]) -> Option<f64> {
    let n = points.len() as f64;
    if points.len() < 2 {
        return None;
    }
    let mx = points.iter().map(|p| p.0).sum::<f64>() / n;
    let my = points.iter().map(|p| p.1).sum::<f64>() / n;
    let sxx: f64 = points.iter().map(|p| (p.0 - mx).powi(2)).sum();
    let sxy: f64 = points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    (sxx > 0.0).then(|| 10.0 * sxy / sxx)
}

/// One harmonic order's row. `drives` are the used runs' drives in set
/// order; `levels[i]` is run `i`'s level for this order.
pub(super) fn harmonic_row(order: u32, drives: &[f64], levels: &[Option<f64>]) -> HarmonicRow {
    let points: Vec<(f64, f64)> = drives
        .iter()
        .zip(levels)
        .filter_map(|(d, l)| l.map(|l| (*d, l)))
        .collect();
    let span = drives.iter().copied().fold(f64::NEG_INFINITY, f64::max)
        - drives.iter().copied().fold(f64::INFINITY, f64::min);
    let slope = if drives.len() < MIN_USED_RUNS {
        Figure::NotComputed(NotComputed::TooFewRuns { used: drives.len() })
    } else if span < MIN_DRIVE_SPAN_DB {
        Figure::NotComputed(NotComputed::DriveSpan { span_db: span })
    } else {
        match slope_per_10db(&points) {
            Some(s) => Figure::Value(s),
            None => Figure::NotComputed(NotComputed::NoLevel),
        }
    };
    HarmonicRow {
        order,
        level_db: levels.last().copied().flatten(),
        slope,
        expected_db: (order as f64 - 1.0) * 10.0,
        points,
    }
}

#[cfg(test)]
mod tests {
    use super::super::testkit::Noise;
    use super::*;

    fn grid_of(f: impl Fn(f64) -> f64, df: f64) -> Grid {
        let pts: Vec<(f64, f64)> = super::super::testkit::bins(df)
            .into_iter()
            .map(|x| (x, f(x)))
            .collect();
        sixth_octave_means(&pts)
    }

    #[test]
    fn grid_cells_are_centred_on_the_sixth_octave_ladder() {
        let g = grid_of(|_| -20.0, 10.0);
        assert!(g.contains_key(&0), "1 kHz cell");
        assert!((centre_hz(6) - 2_000.0).abs() < 1e-9);
        assert!((centre_hz(24) - 16_000.0).abs() < 1e-9);
        // A flat response averages to itself.
        assert!(g.values().all(|v| (v + 20.0).abs() < 1e-9), "{g:?}");
    }

    #[test]
    fn a_cell_with_no_finite_bin_is_absent_not_zero() {
        let g = sixth_octave_means(&[(1_000.0, f64::NEG_INFINITY), (2_000.0, -3.0)]);
        assert!(!g.contains_key(&0));
        assert!((g[&6] + 3.0).abs() < 1e-12, "{g:?}");
    }

    #[test]
    fn clipping_names_why_a_band_is_empty() {
        let b = Band::new(5_000.0, 16_000.0);
        assert_eq!(
            clip_band(b, 20.0, 4_000.0, Some(200.0)),
            Err(NotComputed::AboveSweepStop { f2_hz: 4_000.0 })
        );
        assert_eq!(
            clip_band(b, 20.0, 12_000.0, Some(200.0)),
            Ok(Band::new(5_000.0, 12_000.0))
        );
        assert_eq!(
            clip_band(Band::new(1_500.0, 5_000.0), 20.0, 20_000.0, Some(2_000.0)),
            Ok(Band::new(2_000.0, 5_000.0))
        );
    }

    /// Design point 3, built synthetically: lowering the treble band
    /// uniformly (what a microphone correction can do) must not move the
    /// lower band's flatness. Normalised against a median shared across
    /// 1.5–16 kHz it does move, because that median drops and leaves the
    /// lower band's peak standing proud — the ±3.32 → ±4.99 dB false
    /// finding the issue records.
    #[test]
    fn per_band_median_holds_where_a_shared_median_moves() {
        let peak = |f: f64| {
            let x = (f / 2_460.0).log2() * 6.0;
            4.0 * (-x * x).exp()
        };
        let before = peak;
        let after = |f: f64| peak(f) + if f > 5_000.0 { -3.0 } else { 0.0 };
        let lower = Band::new(1_500.0, 5_000.0);
        let whole = Band::new(1_500.0, 16_000.0);

        let flat = |r: &dyn Fn(f64) -> f64| {
            let (a, b) = (grid_of(r, 50.0), grid_of(r, 50.0));
            flatness(&[&a, &b], Ok(lower)).value().unwrap()
        };
        assert!((flat(&before) - flat(&after)).abs() < 1e-9);

        // The rejected implementation: one median over 1.5–16 kHz.
        let shared = |r: &dyn Fn(f64) -> f64| {
            let g = grid_of(r, 50.0);
            let all: Vec<i64> = band_keys(&[&g], whole);
            let med = band_median(&g, &all);
            band_keys(&[&g], lower)
                .iter()
                .map(|k| (g[k] - med).abs())
                .fold(0.0, f64::max)
        };
        assert!(
            shared(&after) - shared(&before) > 1.0,
            "shared-median flatness should move: {} -> {}",
            shared(&before),
            shared(&after)
        );
    }

    /// Tonal stability on the 1/6-octave grid does not grow as the gate
    /// lengthens (finer bins, more of them per band). The rejected
    /// statistic — the same maximum over raw bins — does: a maximum grows
    /// with the number of samples it is taken over.
    #[test]
    fn grid_statistic_does_not_grow_with_gate_length_where_raw_bins_do() {
        let band = Band::new(5_000.0, 16_000.0);
        let run = |seed: u64, df: f64| -> Vec<(f64, f64)> {
            let mut n = Noise::new(seed);
            super::super::testkit::bins(df)
                .into_iter()
                .map(|f| (f, -20.0 + n.next()))
                .collect()
        };
        let raw = |a: &[(f64, f64)], b: &[(f64, f64)]| {
            let sel = |r: &[(f64, f64)]| -> Vec<f64> {
                r.iter()
                    .filter(|p| band.contains(p.0))
                    .map(|p| p.1)
                    .collect()
            };
            let (a, b) = (sel(a), sel(b));
            let (ma, mb) = (median(&a), median(&b));
            a.iter()
                .zip(&b)
                .map(|(x, y)| {
                    let (nx, ny) = (x - ma, y - mb);
                    (nx - (nx + ny) / 2.0).abs()
                })
                .fold(0.0, f64::max)
        };
        let grid = |a: &[(f64, f64)], b: &[(f64, f64)]| {
            let (ga, gb) = (sixth_octave_means(a), sixth_octave_means(b));
            tonal_stability(&[&ga, &gb], Ok(band)).value().unwrap()
        };
        // Gate 5 ms (200 Hz bins) against 40 ms (25 Hz bins): three
        // doublings of the gate.
        let (short_a, short_b) = (run(1, 200.0), run(2, 200.0));
        let (long_a, long_b) = (run(3, 25.0), run(4, 25.0));
        let (raw_s, raw_l) = (raw(&short_a, &short_b), raw(&long_a, &long_b));
        let (grid_s, grid_l) = (grid(&short_a, &short_b), grid(&long_a, &long_b));
        assert!(raw_l > raw_s, "raw max should grow: {raw_s} -> {raw_l}");
        assert!(
            grid_l <= grid_s,
            "grid max must not grow: {grid_s} -> {grid_l}"
        );
        // And every doubling in between keeps the grid statistic bounded by
        // the shortest gate's.
        for (i, df) in [100.0, 50.0].into_iter().enumerate() {
            let (a, b) = (run(10 + i as u64, df), run(20 + i as u64, df));
            assert!(grid(&a, &b) <= grid_s, "df {df}");
        }
    }

    #[test]
    fn slope_is_in_db_per_ten_db_of_drive() {
        let s = slope_per_10db(&[(-40.0, -70.0), (-30.0, -60.0), (-20.0, -50.0)]).unwrap();
        assert!((s - 10.0).abs() < 1e-9);
        assert!(slope_per_10db(&[(-30.0, -60.0)]).is_none());
        assert!(slope_per_10db(&[(-30.0, -60.0), (-30.0, -50.0)]).is_none());
    }

    #[test]
    fn harmonic_readings_split_at_the_tolerance() {
        let row = |levels: [f64; 2], order: u32| {
            harmonic_row(order, &[-40.0, -30.0], &[Some(levels[0]), Some(levels[1])])
        };
        assert_eq!(
            row([-70.0, -60.0], 2).reading(),
            Some(HarmonicReading::TracksDrive)
        );
        assert_eq!(
            row([-80.0, -80.0], 4).reading(),
            Some(HarmonicReading::FloorLimited)
        );
        assert_eq!(
            row([-80.0, -56.0], 3).reading(),
            Some(HarmonicReading::RisesFaster)
        );
        // Exactly on the tolerance still tracks.
        assert_eq!(
            row([-70.0, -63.0], 2).reading(),
            Some(HarmonicReading::TracksDrive)
        );
    }

    #[test]
    fn a_narrow_drive_span_fits_no_slope_but_keeps_the_level() {
        let r = harmonic_row(2, &[-35.0, -30.0], &[Some(-65.0), Some(-60.0)]);
        assert_eq!(
            r.slope,
            Figure::NotComputed(NotComputed::DriveSpan { span_db: 5.0 })
        );
        assert_eq!(r.level_db, Some(-60.0));
        assert_eq!(r.reading(), None);
    }
}
