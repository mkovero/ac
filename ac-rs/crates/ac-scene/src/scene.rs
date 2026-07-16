//! `Scene` — the type every future view builds on (architect review).
//! Plain data: traces, axes, readouts. No rendering, no GPU, no ZMQ.

use ac_core::visualize::pair_derivation::PairDerivation;
use ac_core::visualize::weighting_curves::WeightingCurve;

use crate::dbfs::linear_to_dbfs;
use crate::readout::{format_cursor_readout, format_spl_readout};
use crate::ticks::{db_axis, db_to_y, freq_axis, freq_to_x, Axis};
use crate::wire::WireFrame;

/// Where a scene's underlying data came from — part of a trace's
/// provenance (D15: a trace is data-with-provenance, never "the live
/// stream").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Live,
    Snapshot,
}

/// Provenance carried by every trace — which session channel, from
/// which kind of input, at what sample rate.
#[derive(Debug, Clone, PartialEq)]
pub struct Provenance {
    pub channel_role: String,
    pub source: Source,
    pub sr: u32,
}

/// One channel's spectrum as a polyline in normalized `[0,1]²`
/// coordinates. Orientation (defined in this crate, structural rule 2):
/// `x=0` = low frequency, `y=0` = bottom = low level.
#[derive(Debug, Clone, PartialEq)]
pub struct Trace {
    pub points: Vec<(f64, f64)>,
    pub provenance: Provenance,
}

/// Formatted readout strings (deliverable 4).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Readouts {
    /// `None` when the meas channel has no SPL calibration layer.
    pub spl: Option<String>,
}

/// The canonical intermediate both wire frames and snapshot derivations
/// funnel through (architect review, decision 2) — the mechanism that
/// makes AC4 (wire/snapshot scene equivalence) hold structurally rather
/// than by coincidence between two independently-written conversions.
pub struct SceneInput {
    pub spec_freqs: Vec<f64>,
    pub meas_spectrum: Vec<f64>,
    pub ref_spectrum: Vec<f64>,
    pub spl: Option<f64>,
    pub spl_weighting: WeightingCurve,
    /// `None` for snapshot-derived input — the offline derivation has
    /// no time-integration concept (architect review, decision 3), not
    /// an omitted field.
    pub spl_integration: Option<&'static str>,
    pub meas_role: String,
    pub ref_role: String,
    pub source: Source,
    pub sr: u32,
}

/// Plain data: traces, axes, readouts. Everything a spectrum view will
/// ever show, with no numeric work left for the renderer.
pub struct Scene {
    pub traces: Vec<Trace>,
    pub freq_axis: Axis,
    pub db_axis: Axis,
    pub readouts: Readouts,

    // Raw (unnormalized) meas-channel data, retained only so
    // `cursor_readout` can look up an actual Hz/dB pair for a
    // caller-given cursor frequency — not part of the trace/axis/
    // readout public contract itself.
    meas_freqs: Vec<f64>,
    meas_level_db: Vec<f64>,
    has_spl_cal: bool,
}

impl Scene {
    /// Build a scene from `input` over the given caller-supplied
    /// frequency and dB ranges (architect review, decision 5 — ranges
    /// are never inferred from data). This is the single construction
    /// path; [`Scene::from_wire_frame`] and [`Scene::from_pair_derivation`]
    /// are thin adapters into `SceneInput` that both call this.
    pub fn from_input(input: SceneInput, freq_range: (f64, f64), db_range: (f64, f64)) -> Scene {
        let (f_min, f_max) = freq_range;
        let (db_min, db_max) = db_range;

        let meas_level_db: Vec<f64> = input
            .meas_spectrum
            .iter()
            .map(|&a| linear_to_dbfs(a))
            .collect();
        let ref_level_db: Vec<f64> = input
            .ref_spectrum
            .iter()
            .map(|&a| linear_to_dbfs(a))
            .collect();

        let meas_trace = Trace {
            points: to_points(
                &input.spec_freqs,
                &meas_level_db,
                f_min,
                f_max,
                db_min,
                db_max,
            ),
            provenance: Provenance {
                channel_role: input.meas_role.clone(),
                source: input.source,
                sr: input.sr,
            },
        };
        let ref_trace = Trace {
            points: to_points(
                &input.spec_freqs,
                &ref_level_db,
                f_min,
                f_max,
                db_min,
                db_max,
            ),
            provenance: Provenance {
                channel_role: input.ref_role.clone(),
                source: input.source,
                sr: input.sr,
            },
        };

        let has_spl_cal = input.spl.is_some();
        let readouts = Readouts {
            spl: format_spl_readout(input.spl, input.spl_weighting, input.spl_integration),
        };

        Scene {
            traces: vec![meas_trace, ref_trace],
            freq_axis: freq_axis(f_min, f_max),
            db_axis: db_axis(db_min, db_max),
            readouts,
            meas_freqs: input.spec_freqs,
            meas_level_db,
            has_spl_cal,
        }
    }

    /// Build a scene from a deserialized live `transfer_stream` v2 frame.
    pub fn from_wire_frame(
        frame: &WireFrame,
        freq_range: (f64, f64),
        db_range: (f64, f64),
    ) -> Scene {
        let spl_weighting = WeightingCurve::from_tag(&frame.spl_weighting)
            .unwrap_or_else(|| panic!("unknown spl_weighting tag: {}", frame.spl_weighting));
        let spl_integration: &'static str = match frame.spl_integration.as_str() {
            "fast" => "fast",
            "slow" => "slow",
            other => panic!("unknown spl_integration tag: {other}"),
        };
        let input = SceneInput {
            spec_freqs: frame.spec_freqs.clone(),
            meas_spectrum: frame.meas_spectrum.clone(),
            ref_spectrum: frame.ref_spectrum.clone(),
            spl: frame.spl,
            spl_weighting,
            spl_integration: Some(spl_integration),
            meas_role: format!("meas_{}", frame.meas_channel),
            ref_role: format!("ref_{}", frame.ref_channel),
            source: Source::Live,
            sr: frame.sr,
        };
        Scene::from_input(input, freq_range, db_range)
    }

    /// Build a scene from an offline snapshot derivation. `meas_role`/
    /// `ref_role`/`sr` are caller-supplied — `PairDerivation` itself
    /// carries no channel-naming or sample-rate context (that lives in
    /// `SnapshotMeta`, one layer up).
    pub fn from_pair_derivation(
        d: &PairDerivation,
        meas_role: &str,
        ref_role: &str,
        sr: u32,
        freq_range: (f64, f64),
        db_range: (f64, f64),
    ) -> Scene {
        let input = SceneInput {
            spec_freqs: d.spec_freqs.clone(),
            meas_spectrum: d.meas_spectrum.clone(),
            ref_spectrum: d.ref_spectrum.clone(),
            spl: d.spl,
            spl_weighting: d.spl_weighting,
            spl_integration: None,
            meas_role: meas_role.to_string(),
            ref_role: ref_role.to_string(),
            source: Source::Snapshot,
            sr,
        };
        Scene::from_input(input, freq_range, db_range)
    }

    /// The nearest column's frequency and level as a formatted string
    /// (deliverable 4). `None` when the scene has no columns (empty
    /// input).
    pub fn cursor_readout(&self, cursor_freq_hz: f64) -> Option<String> {
        let idx = nearest_index(&self.meas_freqs, cursor_freq_hz)?;
        Some(format_cursor_readout(
            self.meas_freqs[idx],
            self.meas_level_db[idx],
            self.has_spl_cal,
        ))
    }
}

fn to_points(
    freqs: &[f64],
    levels_db: &[f64],
    f_min: f64,
    f_max: f64,
    db_min: f64,
    db_max: f64,
) -> Vec<(f64, f64)> {
    freqs
        .iter()
        .zip(levels_db.iter())
        .map(|(&f, &db)| (freq_to_x(f, f_min, f_max), db_to_y(db, db_min, db_max)))
        .collect()
}

fn nearest_index(freqs: &[f64], target: f64) -> Option<usize> {
    freqs
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (*a - target)
                .abs()
                .partial_cmp(&(*b - target).abs())
                .unwrap()
        })
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_input(source: Source) -> SceneInput {
        SceneInput {
            spec_freqs: vec![100.0, 1_000.0, 10_000.0],
            meas_spectrum: vec![0.1, 0.5, 0.05],
            ref_spectrum: vec![0.05, 0.25, 0.025],
            spl: Some(72.3),
            spl_weighting: WeightingCurve::A,
            spl_integration: if source == Source::Live {
                Some("fast")
            } else {
                None
            },
            meas_role: "meas_0".to_string(),
            ref_role: "ref".to_string(),
            source,
            sr: 48_000,
        }
    }

    // AC2: orientation invariant, in pure code — higher level -> larger
    // y, higher frequency -> larger x.
    #[test]
    fn orientation_higher_level_yields_larger_y_higher_freq_yields_larger_x() {
        let scene = Scene::from_input(sample_input(Source::Live), (20.0, 20_000.0), (-80.0, 0.0));
        let meas = &scene.traces[0];
        // freqs ascending -> x must be strictly ascending.
        assert!(meas.points[0].0 < meas.points[1].0);
        assert!(meas.points[1].0 < meas.points[2].0);
        // levels: 0.1 -> -20dB, 0.5 -> -6dB, 0.05 -> -26dB — so y order
        // should be point[2] < point[0] < point[1].
        assert!(meas.points[2].1 < meas.points[0].1);
        assert!(meas.points[0].1 < meas.points[1].1);
    }

    // AC4 mechanism check: identical SceneInput content (differing only
    // in the fields that legitimately differ between the two paths)
    // produces identical trace coordinates, since both paths reduce to
    // one from_input call.
    #[test]
    fn wire_and_snapshot_paths_share_one_construction_function() {
        let live = Scene::from_input(sample_input(Source::Live), (20.0, 20_000.0), (-80.0, 0.0));
        let snap = Scene::from_input(
            sample_input(Source::Snapshot),
            (20.0, 20_000.0),
            (-80.0, 0.0),
        );
        assert_eq!(live.traces[0].points, snap.traces[0].points);
        assert_eq!(live.traces[1].points, snap.traces[1].points);
        // spl value survives on both paths (architect review 3b) —
        // only the integration clause differs (3a).
        assert_eq!(
            live.readouts.spl,
            Some("72.30 dB SPL (A, fast)".to_string())
        );
        assert_eq!(snap.readouts.spl, Some("72.30 dB SPL (A)".to_string()));
    }

    #[test]
    fn cursor_readout_snaps_to_nearest_column() {
        let scene = Scene::from_input(sample_input(Source::Live), (20.0, 20_000.0), (-80.0, 0.0));
        // 900 Hz is nearest to the 1000 Hz column.
        let readout = scene.cursor_readout(900.0).unwrap();
        assert_eq!(readout, "1000 Hz: -6.02 dB SPL");
    }
}
