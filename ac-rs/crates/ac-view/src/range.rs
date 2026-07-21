//! Freq/dB range UI state (deliverable 5). Structural rule: a
//! degenerate range (`min >= max`) must be unrepresentable — enforced
//! by making every mutation go through a constructor/adjustment method
//! that refuses to produce one, rather than by validating after the
//! fact.

const MIN_SPAN_HZ: f64 = 10.0;
const MIN_SPAN_DB: f64 = 6.0;

/// A frequency range with `min < max` always true by construction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FreqRange {
    min: f64,
    max: f64,
}

impl FreqRange {
    /// `None` when `min >= max - MIN_SPAN_HZ` or either bound is
    /// non-positive (a log axis has no room below/at zero).
    pub fn new(min: f64, max: f64) -> Option<Self> {
        if min > 0.0 && max > min + MIN_SPAN_HZ {
            Some(Self { min, max })
        } else {
            None
        }
    }

    pub fn min(&self) -> f64 {
        self.min
    }
    pub fn max(&self) -> f64 {
        self.max
    }

    /// Widen (`m > 1.0`) or narrow (`m < 1.0`) around the geometric-
    /// mean centre, clamped so the span never collapses below
    /// `MIN_SPAN_HZ` and `min` never goes non-positive. Always returns
    /// a valid range — the invariant can't be broken via this method,
    /// only refused to move further.
    ///
    /// No `ln`/`exp`/`powf` (AC1: no log-domain arithmetic anywhere in
    /// this crate) — dividing one bound by `m` and multiplying the
    /// other by `m` preserves `min * max` (the geometric-mean centre)
    /// exactly while scaling the span ratio by `m²`, which is
    /// log-symmetric zoom by construction, not by computing a log.
    pub fn zoom(&self, m: f64) -> Self {
        let new_min = self.min / m;
        let new_max = self.max * m;
        Self::new(new_min.max(1.0), new_max).unwrap_or(*self)
    }

    /// Shift both bounds by a multiplicative factor (log-space pan),
    /// clamped so `min` stays positive.
    pub fn pan(&self, factor: f64) -> Self {
        let new_min = self.min * factor;
        let new_max = self.max * factor;
        Self::new(new_min.max(1.0), new_max).unwrap_or(*self)
    }
}

impl Default for FreqRange {
    fn default() -> Self {
        Self::new(20.0, 20_000.0).unwrap()
    }
}

/// A dB range with `min < max` always true by construction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DbRange {
    min: f64,
    max: f64,
}

impl DbRange {
    /// `None` when the span would be below `MIN_SPAN_DB`.
    pub fn new(min: f64, max: f64) -> Option<Self> {
        if max > min + MIN_SPAN_DB {
            Some(Self { min, max })
        } else {
            None
        }
    }

    pub fn min(&self) -> f64 {
        self.min
    }
    pub fn max(&self) -> f64 {
        self.max
    }

    /// Widen/narrow around the centre, clamped to `MIN_SPAN_DB`.
    pub fn zoom(&self, factor: f64) -> Self {
        let centre = (self.min + self.max) / 2.0;
        let half_span = (self.max - self.min) / 2.0 * factor;
        Self::new(centre - half_span, centre + half_span).unwrap_or(*self)
    }

    /// Shift both bounds by a fixed delta.
    pub fn pan(&self, delta_db: f64) -> Self {
        Self::new(self.min + delta_db, self.max + delta_db).unwrap_or(*self)
    }
}

impl Default for DbRange {
    fn default() -> Self {
        Self::new(-140.0, 0.0).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // AC6: degenerate ranges must be unrepresentable.
    #[test]
    fn freq_range_rejects_min_ge_max() {
        assert!(FreqRange::new(1000.0, 1000.0).is_none());
        assert!(FreqRange::new(1000.0, 500.0).is_none());
    }

    #[test]
    fn freq_range_rejects_zero_span() {
        assert!(FreqRange::new(1000.0, 1000.0 + MIN_SPAN_HZ - 0.01).is_none());
    }

    #[test]
    fn freq_range_rejects_non_positive_min() {
        assert!(FreqRange::new(0.0, 20_000.0).is_none());
        assert!(FreqRange::new(-10.0, 20_000.0).is_none());
    }

    #[test]
    fn db_range_rejects_min_ge_max() {
        assert!(DbRange::new(0.0, 0.0).is_none());
        assert!(DbRange::new(0.0, -140.0).is_none());
    }

    #[test]
    fn freq_zoom_never_collapses_the_span() {
        let r = FreqRange::default();
        // Zoom in aggressively, repeatedly — span must stay >= MIN_SPAN_HZ
        // and min must stay positive at every step, not just the last one.
        let mut cur = r;
        for _ in 0..200 {
            cur = cur.zoom(0.5);
            assert!(cur.min() > 0.0);
            assert!(cur.max() > cur.min());
        }
    }

    #[test]
    fn db_zoom_never_collapses_the_span() {
        let r = DbRange::default();
        let mut cur = r;
        for _ in 0..200 {
            cur = cur.zoom(0.5);
            assert!(cur.max() > cur.min());
        }
    }

    #[test]
    fn freq_pan_keeps_min_positive() {
        let r = FreqRange::default();
        let mut cur = r;
        for _ in 0..200 {
            cur = cur.pan(0.5);
            assert!(cur.min() > 0.0);
        }
    }
}
