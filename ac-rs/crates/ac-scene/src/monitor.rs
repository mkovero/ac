//! The live input monitor's scene: one trace, one input meter and one
//! readout line per monitored channel, built from `monitor_spectrum`'s
//! `visualize/spectrum` frames. Plain data like every other scene here;
//! `ac-view`'s monitor window maps and paints it.
//!
//! `monitor_spectrum` sends one frame per channel per tick, so the
//! channels of one window are never updated together. The state keeps the
//! newest frame per channel and the scene is built from whatever each
//! channel last delivered, with a channel that has gone quiet marked stale
//! rather than drawn as if it were current.

use std::collections::BTreeMap;

use ac_core::wire::SpectrumFrame;

use crate::dbfs::linear_to_dbfs;
use crate::scene::{Provenance, Source, Trace};
use crate::ticks::{db_axis, db_to_y, freq_axis, freq_to_x, Axis};
use crate::transfer::{Meter, MeterState};

/// A channel whose newest frame is older than this (scene seconds) reads
/// stale: its trace is withheld and its meter falls to zero. Several ticks
/// at any `monitor_spectrum` interval the window asks for, so one late
/// frame does not blink the trace.
pub const STALE_AFTER_S: f64 = 1.0;

/// One monitored channel, ready to paint.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorChannel {
    /// Input channel index, as on the wire.
    pub channel: u32,
    /// The spectrum, normalized to the scene's ranges. `None` until the
    /// first frame and while stale.
    pub trace: Option<Trace>,
    pub meter: Meter,
    /// Short meter label: the channel index.
    pub label: String,
    /// One line: level, dominant tone and THD, or why there is none.
    pub readout: String,
}

/// Everything the monitor window draws.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorScene {
    /// In request order.
    pub channels: Vec<MonitorChannel>,
    pub freq_axis: Axis,
    pub db_axis: Axis,
}

/// Newest frame and meter state per requested channel, carried across
/// window passes.
#[derive(Debug, Clone, Default)]
pub struct MonitorState {
    channels: Vec<u32>,
    latest: BTreeMap<u32, (SpectrumFrame, f64)>,
    meters: BTreeMap<u32, MeterState>,
}

impl MonitorState {
    /// State for the channels the window asked the daemon for, in the
    /// order they were asked.
    pub fn new(channels: &[u32]) -> MonitorState {
        MonitorState {
            channels: channels.to_vec(),
            ..MonitorState::default()
        }
    }

    /// Fold in one frame received at scene time `now_s`. A frame for a
    /// channel this window did not ask for is ignored; returns whether the
    /// frame was kept.
    pub fn push(&mut self, frame: SpectrumFrame, now_s: f64) -> bool {
        if !self.channels.contains(&frame.channel) {
            return false;
        }
        if let Some(m) = self.meters.get_mut(&frame.channel) {
            m.update(frame.peak_dbfs, now_s);
        } else {
            let mut m = MeterState::default();
            m.update(frame.peak_dbfs, now_s);
            self.meters.insert(frame.channel, m);
        }
        self.latest.insert(frame.channel, (frame, now_s));
        true
    }

    /// The scene at scene time `now_s` over the caller's ranges.
    pub fn scene(
        &mut self,
        now_s: f64,
        freq_range: (f64, f64),
        db_range: (f64, f64),
    ) -> MonitorScene {
        let (f_min, f_max) = freq_range;
        let (db_min, db_max) = db_range;
        let channels = self
            .channels
            .iter()
            .map(|&channel| {
                let latest = self.latest.get(&channel);
                let age_s = latest.map(|(_, t)| now_s - t);
                let fresh = age_s.is_some_and(|a| a < STALE_AFTER_S);
                // Read, not update: a repaint between frames must age the
                // hold and clip latch, not replay the last frame's peak
                // (which would keep re-arming the latch). A stale channel
                // reads silent.
                let peak = latest.filter(|_| fresh).and_then(|(f, _)| f.peak_dbfs);
                let meter = self
                    .meters
                    .get(&channel)
                    .copied()
                    .unwrap_or_default()
                    .read(peak, now_s);
                let trace = latest.filter(|_| fresh).map(|(f, _)| {
                    let points = f
                        .freqs
                        .iter()
                        .zip(&f.spectrum)
                        .map(|(&hz, &amp)| {
                            (
                                freq_to_x(hz, f_min, f_max),
                                db_to_y(linear_to_dbfs(amp), db_min, db_max),
                            )
                        })
                        .collect();
                    Trace::single(
                        points,
                        Provenance {
                            channel_role: format!("in_{channel}"),
                            source: Source::Live,
                            sr: f.sr,
                        },
                    )
                });
                let readout = match (latest, age_s) {
                    (Some((f, _)), Some(_)) if fresh => format_monitor_readout(f),
                    (Some(_), Some(age)) => {
                        format!("CH{channel:<2}  no frame for {age:.1} s")
                    }
                    _ => format!("CH{channel:<2}  waiting for first frame"),
                };
                MonitorChannel {
                    channel,
                    trace,
                    meter,
                    label: channel.to_string(),
                    readout,
                }
            })
            .collect();
        MonitorScene {
            channels,
            freq_axis: freq_axis(f_min, f_max),
            db_axis: db_axis(db_min, db_max),
        }
    }
}

/// One channel's readout line: sample peak, then the dominant tone and
/// THD when the daemon resolved one, then clipping.
pub fn format_monitor_readout(f: &SpectrumFrame) -> String {
    let mut s = format!("CH{:<2}  peak ", f.channel);
    match f.peak_dbfs {
        Some(p) if p.is_finite() => s.push_str(&format!("{p:>6.1} dBFS")),
        _ => s.push_str("silent"),
    }
    if let (Some(hz), Some(db)) = (f.freq_hz, f.fundamental_dbfs) {
        s.push_str(&format!("   tone {hz:>8.1} Hz {db:>6.1} dBFS"));
        if let Some(thd) = f.thd_pct {
            s.push_str(&format!("   THD {thd:.4} %"));
        }
    }
    if f.clipping == Some(true) {
        s.push_str("   CLIPPING");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(channel: u32, peak_dbfs: Option<f64>) -> SpectrumFrame {
        SpectrumFrame {
            frame_type: "visualize/spectrum".into(),
            cmd: "monitor_spectrum".into(),
            wire_version: None,
            channel,
            n_channels: 2,
            sr: 48_000,
            freqs: vec![100.0, 1_000.0, 10_000.0],
            spectrum: vec![0.01, 0.1, 0.001],
            dbu_offset_db: None,
            voltage_check: None,
            spl_offset_db: None,
            mic_correction: "none".into(),
            xruns: 0,
            backend: "fake".into(),
            peak_dbfs,
            freq_hz: None,
            peaks: None,
            fundamental_dbfs: None,
            thd_pct: None,
            thdn_pct: None,
            in_dbu: None,
            clipping: None,
        }
    }

    const F: (f64, f64) = (20.0, 20_000.0);
    const D: (f64, f64) = (-140.0, 0.0);

    #[test]
    fn every_requested_channel_gets_a_row_in_request_order() {
        let mut st = MonitorState::new(&[3, 0, 5]);
        assert!(st.push(frame(0, Some(-20.0)), 0.0));
        assert!(st.push(frame(5, Some(-30.0)), 0.0));
        let sc = st.scene(0.1, F, D);
        let order: Vec<u32> = sc.channels.iter().map(|c| c.channel).collect();
        assert_eq!(order, vec![3, 0, 5]);
        assert!(sc.channels[0].trace.is_none());
        assert!(sc.channels[0].readout.contains("waiting for first frame"));
        assert!(sc.channels[1].trace.is_some());
        assert!(sc.channels[2].trace.is_some());
    }

    #[test]
    fn a_channel_not_requested_is_ignored() {
        let mut st = MonitorState::new(&[0]);
        assert!(!st.push(frame(7, Some(-20.0)), 0.0));
        assert_eq!(st.scene(0.0, F, D).channels.len(), 1);
    }

    /// A channel that stops delivering must not keep showing its last
    /// spectrum as if live: past `STALE_AFTER_S` the trace goes and the
    /// meter falls, and the readout says for how long.
    #[test]
    fn a_quiet_channel_goes_stale() {
        let mut st = MonitorState::new(&[0]);
        st.push(frame(0, Some(-6.0)), 0.0);
        let live = st.scene(0.5, F, D);
        assert!(live.channels[0].trace.is_some());
        assert!(live.channels[0].meter.height > 0.8);
        let stale = st.scene(0.5 + STALE_AFTER_S + 0.5, F, D);
        assert!(stale.channels[0].trace.is_none());
        assert_eq!(stale.channels[0].meter.height, 0.0);
        assert!(stale.channels[0].readout.contains("no frame for 2.0 s"));
    }

    #[test]
    fn meter_follows_the_frame_peak_and_latches_clip() {
        let mut st = MonitorState::new(&[0]);
        st.push(frame(0, Some(-30.0)), 0.0);
        let m = st.scene(0.0, F, D).channels[0].meter;
        assert!((m.height - 0.5).abs() < 1e-9, "{m:?}");
        st.push(frame(0, Some(0.0)), 0.1);
        st.push(frame(0, Some(-30.0)), 0.2);
        assert!(st.scene(0.2, F, D).channels[0].meter.clip_latch);
    }

    /// A clipped last frame latches from when it arrived, not from the
    /// last repaint: repainting a stalled stream must not keep the latch up.
    #[test]
    fn repaints_do_not_rearm_the_clip_latch() {
        let mut st = MonitorState::new(&[0]);
        st.push(frame(0, Some(0.0)), 0.0);
        let mut t = 0.0;
        while t < 2.9 {
            assert!(st.scene(t, F, D).channels[0].meter.clip_latch, "t={t}");
            t += 0.016;
        }
        assert!(!st.scene(3.1, F, D).channels[0].meter.clip_latch);
    }

    #[test]
    fn readout_names_level_tone_and_thd() {
        let mut f = frame(2, Some(-12.34));
        assert_eq!(format_monitor_readout(&f), "CH2   peak  -12.3 dBFS");
        f.freq_hz = Some(997.0);
        f.fundamental_dbfs = Some(-15.0);
        f.thd_pct = Some(0.0031);
        assert_eq!(
            format_monitor_readout(&f),
            "CH2   peak  -12.3 dBFS   tone    997.0 Hz  -15.0 dBFS   THD 0.0031 %"
        );
        f.peak_dbfs = None;
        assert!(format_monitor_readout(&f).starts_with("CH2   peak silent"));
    }
}
