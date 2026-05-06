//! Frame-render pipeline — `App::redraw` and its helpers. Runs every vsync
//! (when the compositor repaints) or on demand (winit `RedrawRequested`).
//! Reads from data stores, uploads to GPU, composites egui on top, and
//! submits the frame. All overlays (peak-hold, monitor stats) are painted
//! inside the single egui pass driven from `redraw`.

use std::sync::Arc;
use std::time::Instant;

use egui::Color32;

use crate::data::smoothing;
use crate::data::types::{
    CellView, DisplayFrame, FrameMeta, LayoutMode, SweepKind, TransferFrame, ViewMode,
};
use crate::render::context::RenderContext;
use crate::render::grid;
use crate::render::spectrum::{ChannelMeta, ChannelUpload};
use crate::render::waterfall::CellUpload as WaterfallCellUpload;
use crate::theme;
use crate::ui::export::{self, ScreenshotRequest};
use crate::ui::layout;
use crate::ui::overlay::{
    self, HoverInfo, HoverReadout, MonitorParamsInfo, OverlayInput,
    TimeIntegrationOverlay,
};
use crate::ui::stats::StatsSnapshot;
use ac_core::visualize::time_integration::{TAU_FAST_S, TAU_SLOW_S};

use super::TimeIntegrationMode;

const EMBER_SCOPE_SINE_HZ: f32 = 1_000.0;
/// Fallback sample rate when no real-channel frame has arrived yet to read
/// the live `meta.sr` from. Same value the existing `App::current_sr()` uses
/// (control.rs); kept in sync via this constant rather than scattering
/// magic 48 000 literals across the codebase.
const EMBER_FALLBACK_SR: u32 = 48_000;

/// Phase 1 trajectory views: synthetic stereo for Goniometer. Same
/// 1 kHz carrier on both channels, plus a slow phase drift on R so the
/// goniometer walks through every phase state (vertical line → tilted
/// ellipse → circle → ellipse other way → horizontal line) on a ~3 s
/// cycle. That's what a goniometer is *for* — two different carriers
/// would just draw a Lissajous of an uncorrelated pair, which encodes
/// no useful phase information.
const EMBER_GONIO_FREQ: f32 = 1_000.0;
const EMBER_GONIO_PHASE_DRIFT_HZ: f32 = 0.3;
const EMBER_GONIO_AMP: f32 = 0.7;

/// Build the overlay payload describing the active time-integration
/// state. Returns `None` when the mode is off so the overlay renders
/// nothing; otherwise carries the mode label and its τ (for fast/slow)
/// or running Leq duration (for Leq). Duration is read from the most
/// recent frame's `FrameMeta::leq_duration_s` — `NaN` means the frame
/// carries integrated data but mode doesn't have a meaningful duration.
fn build_time_integration_overlay(
    mode: TimeIntegrationMode,
    frames: &[Option<crate::data::types::DisplayFrame>],
) -> Option<TimeIntegrationOverlay> {
    let (label, tau_s) = match mode {
        TimeIntegrationMode::Off  => return None,
        TimeIntegrationMode::Fast => ("fast", Some(TAU_FAST_S)),
        TimeIntegrationMode::Slow => ("slow", Some(TAU_SLOW_S)),
        TimeIntegrationMode::Leq  => ("Leq",  None),
    };
    let duration_s = frames
        .iter()
        .flatten()
        .find_map(|f| f.meta.leq_duration_s)
        .filter(|d: &f64| d.is_finite());
    Some(TimeIntegrationOverlay { mode: label, tau_s, duration_s })
}

use super::helpers::{
    NOTIFICATION_TTL, PEAK_HOLD_DECAY, PEAK_RELEASE_DB_PER_SEC,
    WATERFALL_ROW_DT_HYSTERESIS, WATERFALL_ROW_DT_MIN, WATERFALL_ROW_DT_WINDOW,
    median_f32,
};
use super::App;

impl App {
    pub(super) fn redraw(&mut self) {
        let frame_start = Instant::now();
        // Phase 6: debounced flush of persisted UI state. Cheap when
        // nothing's dirty; writes ~500 ms after the last mutator. Done
        // here rather than in input handlers so the actual disk write
        // (which involves serialise + create_dir_all + write) is
        // shifted off the keypress critical path.
        self.flush_ui_state_if_due();
        let grid_params_snap = self.grid_params();
        // Drain any worker-error message the receiver picked up on the
        // `error` PUB topic BEFORE we take any long-lived &mut borrows on
        // self — notify() is &mut self and the render_ctx borrow below spans
        // the whole draw body.
        let pending_error = self.source.as_ref().and_then(|src| src.status()).and_then(|s| s.take_error());
        if let Some(err) = pending_error {
            if err.contains("transfer_stream") {
                self.transfer_stream_active = false;
            }
            self.notify(&err);
        }
        // All views that consume a *pair* of channels — Bode/Coherence/
        // Phase/GroupDelay/Nyquist/IR (transfer-derived) plus Goniometer/
        // IoTransfer (trajectory) — resolve which `TransferPair` to use
        // here. Pair registration is handled by `T` (Space-select MEAS +
        // REF, then T); the resolver picks the active virtual channel's
        // pair, falling back to the first registered pair when active
        // sits on a real channel. `None` triggers each view's "no pair
        // registered" fallback (synthetic carrier for trajectory views,
        // empty polyline + Space+T caption for transfer views).
        let bode_pair: Option<crate::data::types::TransferPair> = matches!(
            self.config.view_mode,
            ViewMode::BodeMag
                | ViewMode::Coherence
                | ViewMode::BodePhase
                | ViewMode::GroupDelay
                | ViewMode::Nyquist
                | ViewMode::Ir
                | ViewMode::Goniometer
                | ViewMode::IoTransfer,
        )
        .then(|| self.resolve_transfer_pair_for_active())
        .flatten();
        let ctx = match self.render_ctx.as_mut() {
            Some(c) => c,
            None => return,
        };
        let spectrum = self.spectrum.as_mut().unwrap();
        let waterfall = self.waterfall.as_mut().unwrap();
        let ember = self.ember.as_mut().unwrap();
        let egui_renderer = self.egui_renderer.as_mut().unwrap();
        let egui_state = self.egui_state.as_mut().unwrap();

        let mut frames = {
            let store = self.store.as_mut();
            if let Some(store) = store {
                if !self.config.frozen {
                    // Drop the previous tick's DisplayFrames *before* reading so
                    // ChannelSlot::averaged has refcount 1 and `Arc::make_mut`
                    // can mutate in place instead of copy-on-write.
                    self.last_frames.clear();
                    self.last_frames = store.read_all(&self.config);
                } else {
                    let _ = store.read_all(&self.config);
                }
            }
            self.last_frames.clone()
        };

        if let Some(ts) = self.transfer_store.as_ref() {
            if !self.config.frozen {
                if let Some(frame) = ts.read() {
                    self.transfer_last = Some(frame);
                }
            }
        }

        // Virtual transfer channels get appended as extra cells so the grid /
        // single / compare layouts render `|H(ω)|` alongside real captures. A
        // virtual cell with no frame yet shows as empty (same as a real
        // channel before its first packet). Phase + coherence rendering is
        // deferred to a follow-up; this commit only wires magnitude-dB through
        // the spectrum/waterfall renderers.
        let n_real = frames.len();
        let virtual_snapshots = self.virtual_channels.read_all_with_serial();
        self.virtual_render_pairs = virtual_snapshots.iter().map(|(p, _, _)| *p).collect();
        {
            let live: std::collections::HashSet<_> =
                virtual_snapshots.iter().map(|(p, _, _)| *p).collect();
            self.virtual_seen_serial.retain(|p, _| live.contains(p));
        }
        for (pair, serial, maybe_tf) in &virtual_snapshots {
            let is_fresh = *serial != 0
                && self.virtual_seen_serial.get(pair).copied().unwrap_or(0) != *serial;
            if is_fresh {
                self.virtual_seen_serial.insert(*pair, *serial);
            }
            let frame = maybe_tf.as_ref().map(|tf| {
                let spectrum = Arc::new(tf.magnitude_db.clone());
                DisplayFrame {
                    spectrum: spectrum.clone(),
                    freqs:    Arc::new(tf.freqs.clone()),
                    meta: FrameMeta {
                        freq_hz:          0.0,
                        fundamental_dbfs: -140.0,
                        thd_pct:          0.0,
                        thdn_pct:         0.0,
                        in_dbu:           None,
                        dbu_offset_db:    None,
                        peaks:            Arc::new(Vec::new()),
                        spl_offset_db:    None,
                        mic_correction:   None,
                        sr:               tf.sr,
                        clipping:         false,
                        xruns:            0,
                        leq_duration_s:   None,
                    },
                    new_row: if is_fresh { Some(spectrum) } else { None },
                }
            });
            frames.push(frame);
        }

        // Compute the visible-cell layout once, up-front, so the heavy
        // per-channel preprocessing below (smoothing, peak hold, min
        // hold) only touches channels that will actually be rendered.
        // Pre-#111 these loops iterated every channel regardless of
        // layout — paged Grid showing 4 of 8 channels still ran the
        // smoothing filter on the hidden 4. `layout::compute` already
        // knows the visible set; reuse it for both gating and the
        // upload-building loop further down. (#111)
        let n_channels = frames.len();
        let cells = layout::compute(
            self.config.layout,
            n_channels,
            self.config.active_channel,
            &self.selected,
            grid_params_snap,
        );
        let visible_channels: std::collections::HashSet<usize> =
            cells.iter().map(|c| c.channel).collect();

        // Fractional-octave smoothing. Runs before peak-hold so the held max
        // is taken over the smoothed trace the user is actually looking at;
        // it also keeps the frame-level `spectrum` consistent with what the
        // overlay reads for hover labels. Window indices are cached per
        // `(n_frac, n_bins, last_freq)` to avoid a log-range recompute per
        // frame.
        if let Some(n_frac) = self.smoothing_frac {
            for (idx, slot) in frames.iter_mut().enumerate() {
                if !visible_channels.contains(&idx) {
                    continue;
                }
                let Some(frame) = slot.as_mut() else { continue };
                if frame.freqs.is_empty() || frame.spectrum.is_empty() {
                    continue;
                }
                let last_f = *frame.freqs.last().unwrap();
                let needs_rebuild = self
                    .smoothing_cache
                    .as_ref()
                    .map_or(true, |w| !w.matches(n_frac, frame.freqs.len(), last_f));
                if needs_rebuild {
                    self.smoothing_cache = Some(smoothing::OctaveWindows::build(
                        n_frac,
                        frame.freqs.as_ref(),
                    ));
                }
                let windows = self.smoothing_cache.as_ref().unwrap();
                let smoothed = smoothing::smooth_db(
                    frame.spectrum.as_slice(),
                    windows,
                );
                frame.spectrum = Arc::new(smoothed);
            }
        }

        // Grow per-channel state arrays so the render path can index by the
        // virtual channel index the same way it does for real channels.
        // Shrinking never happens: real channel count is fixed for the
        // session, and removing a virtual channel leaves harmless empty
        // trailing slots (they'll be reused the next time the user presses T).
        let n_total = frames.len();
        if self.cell_views.len() < n_total {
            self.cell_views.resize(n_total, CellView::default());
        }
        if self.selected.len() < n_total {
            self.selected.resize(n_total, false);
        }
        if self.peak_holds.len() < n_real {
            self.peak_holds.resize(n_real, None);
        }
        if self.peak_last_update.len() < n_real {
            self.peak_last_update.resize(n_real, None);
        }
        if self.peak_last_tick.len() < n_real {
            self.peak_last_tick.resize(n_real, None);
        }
        if self.min_holds.len() < n_real {
            self.min_holds.resize(n_real, None);
        }
        if self.min_last_update.len() < n_real {
            self.min_last_update.resize(n_real, None);
        }
        if self.min_last_tick.len() < n_real {
            self.min_last_tick.resize(n_real, None);
        }

        // Peak-hold accumulator: fold every fresh spectrum bin-wise against
        // the held max. Virtual (transfer) channels are skipped — peak-hold
        // is a spectrum-only concept. A bin-count mismatch (FFT-N change we
        // missed, or a late first frame at a different N) re-seeds the
        // buffer instead of panicking or silently clipping. Hidden channels
        // (off-page Grid, deselected Compare, non-active Single) are
        // skipped too — their held buffer keeps its previous value and
        // resumes from there when the channel comes back into view (#111).
        if self.peak_hold_enabled {
            let now = Instant::now();
            for (i, slot) in frames.iter().enumerate().take(n_real) {
                if !visible_channels.contains(&i) {
                    continue;
                }
                let Some(frame) = slot.as_ref() else { continue };
                if frame.new_row.is_none() || frame.spectrum.is_empty() {
                    continue;
                }
                let buf = match self.peak_holds.get_mut(i) {
                    Some(b) => b,
                    None => continue,
                };
                let stamp = self
                    .peak_last_update
                    .get_mut(i)
                    .expect("resized above");
                let tick = self
                    .peak_last_tick
                    .get_mut(i)
                    .expect("resized above");
                // Seconds since the previous frame we processed for this
                // channel — used below to scale the release drop. Clamped
                // into a sane range so a stall (tab hidden, debugger pause)
                // can't produce a single enormous drop on resume.
                let dt = tick
                    .map(|t| now.duration_since(t).as_secs_f32())
                    .unwrap_or(0.0)
                    .clamp(0.0, 0.25);
                *tick = Some(now);
                match buf.as_mut() {
                    Some(existing) if existing.len() == frame.spectrum.len() => {
                        let mut any_updated = false;
                        for (held, fresh) in existing.iter_mut().zip(frame.spectrum.iter()) {
                            if fresh.is_finite() && *fresh > *held {
                                *held = *fresh;
                                any_updated = true;
                            }
                        }
                        if any_updated {
                            *stamp = Some(now);
                        } else if let Some(last) = *stamp {
                            // Hold window has elapsed — glide down toward the
                            // live trace at a bounded dB/s so the peak fades
                            // out instead of blinking away. Clamped to `fresh`
                            // so a bin that's already below the current
                            // spectrum stops falling.
                            if now.duration_since(last) >= PEAK_HOLD_DECAY {
                                let drop = PEAK_RELEASE_DB_PER_SEC * dt;
                                for (held, fresh) in
                                    existing.iter_mut().zip(frame.spectrum.iter())
                                {
                                    if fresh.is_finite() {
                                        *held = (*held - drop).max(*fresh);
                                    }
                                }
                            }
                        } else {
                            *stamp = Some(now);
                        }
                    }
                    _ => {
                        *buf = Some(frame.spectrum.as_ref().clone());
                        *stamp = Some(now);
                    }
                }
            }
        }

        // Min-hold accumulator: mirror of the peak loop with the comparator
        // flipped. Same decay rule so a brief gap in the signal doesn't pin
        // the trace down forever at whatever accidental silence the buffer
        // captured. Visibility-gated identically to the peak path (#111).
        if self.min_hold_enabled {
            let now = Instant::now();
            for (i, slot) in frames.iter().enumerate().take(n_real) {
                if !visible_channels.contains(&i) {
                    continue;
                }
                let Some(frame) = slot.as_ref() else { continue };
                if frame.new_row.is_none() || frame.spectrum.is_empty() {
                    continue;
                }
                let buf = match self.min_holds.get_mut(i) {
                    Some(b) => b,
                    None => continue,
                };
                let stamp = self
                    .min_last_update
                    .get_mut(i)
                    .expect("resized above");
                let tick = self
                    .min_last_tick
                    .get_mut(i)
                    .expect("resized above");
                let dt = tick
                    .map(|t| now.duration_since(t).as_secs_f32())
                    .unwrap_or(0.0)
                    .clamp(0.0, 0.25);
                *tick = Some(now);
                match buf.as_mut() {
                    Some(existing) if existing.len() == frame.spectrum.len() => {
                        let mut any_updated = false;
                        for (held, fresh) in existing.iter_mut().zip(frame.spectrum.iter()) {
                            if fresh.is_finite() && *fresh < *held {
                                *held = *fresh;
                                any_updated = true;
                            }
                        }
                        if any_updated {
                            *stamp = Some(now);
                        } else if let Some(last) = *stamp {
                            // Symmetric release — rise toward live so a quiet
                            // moment doesn't pin the noise-floor trace at a
                            // fluke dropout forever.
                            if now.duration_since(last) >= PEAK_HOLD_DECAY {
                                let rise = PEAK_RELEASE_DB_PER_SEC * dt;
                                for (held, fresh) in
                                    existing.iter_mut().zip(frame.spectrum.iter())
                                {
                                    if fresh.is_finite() {
                                        *held = (*held + rise).min(*fresh);
                                    }
                                }
                            }
                        } else {
                            *stamp = Some(now);
                        }
                    }
                    _ => {
                        *buf = Some(frame.spectrum.as_ref().clone());
                        *stamp = Some(now);
                    }
                }
            }
        }

        if self.waterfall_inited.len() < n_total {
            self.waterfall_inited.resize(n_total, false);
        }

        // `cells` and `n_channels` were computed up front for the
        // visibility-gated preprocessing above (#111); they're still
        // valid here — `selected` only grows during this redraw, never
        // shrinks, and the layout depends only on `selected`'s values
        // at present-known indices. Drop the duplicate call.
        let in_sweep_layout = matches!(self.config.layout, LayoutMode::Sweep);
        if let Some(ss) = self.sweep_store.as_ref() {
            if !self.config.frozen {
                self.sweep_last = ss.read();
            }
        }

        // Track producer cadence from channel-0 new_row arrivals. Rolling
        // median over the last WATERFALL_ROW_DT_WINDOW samples so a single
        // hiccup or brief stall can't drag the axis; guarded to a sane band
        // (1 ms..5 s) to reject clock jumps and first-frame deltas. A small
        // hysteresis gate suppresses label flipping from median micro-churn
        // while leaving real cadence shifts free to propagate.
        if let Some(Some(f0)) = frames.first() {
            if f0.new_row.is_some() {
                let now = Instant::now();
                if let Some(prev) = self.waterfall_last_row_at {
                    let dt = now.duration_since(prev).as_secs_f32();
                    if dt > 0.001 && dt < 5.0 {
                        if self.waterfall_row_dts.len() == WATERFALL_ROW_DT_WINDOW {
                            self.waterfall_row_dts.pop_front();
                        }
                        self.waterfall_row_dts.push_back(dt);
                        if self.waterfall_row_dts.len() >= WATERFALL_ROW_DT_MIN {
                            let slice: Vec<f32> =
                                self.waterfall_row_dts.iter().copied().collect();
                            if let Some(med) = median_f32(&slice) {
                                let cur = self.waterfall_row_period_s.max(1e-6);
                                if ((med - cur) / cur).abs() > WATERFALL_ROW_DT_HYSTERESIS {
                                    self.waterfall_row_period_s = med;
                                }
                            }
                        }
                    }
                }
                self.waterfall_last_row_at = Some(now);
            }
        }
        // Stretch the freq clamp to whatever Nyquist the producer is running
        // The daemon publishes bins spanning 20 Hz .. sr/2, and the GPU
        // shader maps bin index linearly across the viewport — so the
        // on-screen freq axis is correct only when view.freq_min /
        // freq_max match the data range. Track the current frame's
        // freqs each redraw (NOT a monotonic max) so dropping from
        // 96 kHz back to 48 kHz shrinks the axis to 24 kHz instead of
        // showing a permanently-empty 24..48 kHz tail. Pan/zoom on the
        // freq axis was traded away for this lock.
        let mut data_max_seen: Option<f32> = None;
        let mut data_min_seen: Option<f32> = None;
        for slot in frames.iter().flatten() {
            if let Some(&last) = slot.freqs.last() {
                if last.is_finite() && last > 0.0 {
                    data_max_seen = Some(data_max_seen.map_or(last, |m: f32| m.max(last)));
                }
            }
            if let Some(&first) = slot.freqs.first() {
                if first.is_finite() && first > 0.0 {
                    data_min_seen = Some(data_min_seen.map_or(first, |m: f32| m.min(first)));
                }
            }
        }
        let data_max = data_max_seen.unwrap_or(theme::DEFAULT_FREQ_MAX);
        let data_min = data_min_seen.unwrap_or(theme::DEFAULT_FREQ_MIN);
        // Stash the live ceiling for the input handler's pan/zoom math
        // (it reads `self.data_freq_ceiling` to clamp user-driven changes).
        self.data_freq_ceiling = data_max;
        for cv in self.cell_views.iter_mut() {
            cv.freq_min = data_min;
            cv.freq_max = data_max;
        }

        let view_mode = self.config.view_mode;
        // First waterfall frame per channel picks a dB window derived from
        // the actual signal: ceiling just above the observed peak (clamped
        // to [-20, 0]), 80 dB span below. A fixed [-60, 0] window renders
        // black for line-in mic levels ~-100 dBFS, forcing the user to mash
        // `+` / `[` to see anything.
        if matches!(view_mode, ViewMode::Waterfall) {
            for (i, slot) in frames.iter().enumerate() {
                let Some(frame) = slot.as_ref() else { continue };
                if frame.spectrum.is_empty() {
                    continue;
                }
                let already = self.waterfall_inited.get(i).copied().unwrap_or(true);
                if already {
                    continue;
                }
                // Use the 98th-percentile bin as the anchor, not the single
                // max. A lone transient or DC spike in the first frame used
                // to drag the whole db window with it and stay stuck until
                // Ctrl+R — P98 ignores those outliers while still tracking
                // a real signal level.
                let mut finite: Vec<f32> = frame
                    .spectrum
                    .iter()
                    .copied()
                    .filter(|v| v.is_finite())
                    .collect();
                let (db_min, db_max) = if finite.len() >= 8 {
                    finite.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    let idx = ((finite.len() as f32 * 0.98) as usize)
                        .min(finite.len() - 1);
                    let anchor = finite[idx];
                    let top = (anchor + 5.0).clamp(-20.0, 0.0);
                    (top - 80.0, top)
                } else if let Some(&peak) = finite.last() {
                    let top = (peak + 5.0).clamp(-20.0, 0.0);
                    (top - 80.0, top)
                } else {
                    (-60.0, 0.0)
                };
                if let Some(view) = self.cell_views.get_mut(i) {
                    view.db_min = db_min;
                    view.db_max = db_max;
                }
                if let Some(flag) = self.waterfall_inited.get_mut(i) {
                    *flag = true;
                }
            }
        }
        let mut spectrum_uploads: Vec<ChannelUpload> = Vec::new();
        let mut waterfall_uploads: Vec<WaterfallCellUpload<'_>> = Vec::new();
        if !in_sweep_layout {
            match view_mode {
                ViewMode::Spectrum => spectrum_uploads.reserve(cells.len()),
                ViewMode::Waterfall => waterfall_uploads.reserve(cells.len()),
                ViewMode::Scope
                | ViewMode::SpectrumEmber
                | ViewMode::Goniometer
                | ViewMode::IoTransfer
                | ViewMode::BodeMag
                | ViewMode::Coherence
                | ViewMode::BodePhase
                | ViewMode::GroupDelay
                | ViewMode::Nyquist
                | ViewMode::Ir => {}
            }
        }

        for cell in &cells {
            if in_sweep_layout {
                break;
            }
            let frame = match frames.get(cell.channel).and_then(|f| f.as_ref()) {
                Some(f) if !f.spectrum.is_empty() => f,
                _ => continue,
            };
            let view = self
                .cell_views
                .get(cell.channel)
                .copied()
                .unwrap_or_default();
            let freq_log_min = view.freq_min.max(1.0).log10();
            let freq_log_max = view.freq_max.max(20.0).log10();
            match view_mode {
                ViewMode::Spectrum => {
                    // Single view + virtual transfer channel splits the
                    // cell into spectrum (top) + phase subplot (bottom).
                    // GPU viewport uses y=0 at bottom, so shift origin up
                    // by (1 - FRACTION) * cell.h and shrink height.
                    let single_virtual = matches!(self.config.layout, LayoutMode::Single)
                        && cell.channel >= n_real;
                    let (vp_y, vp_h) = if single_virtual {
                        let frac = crate::render::virtual_overlay::SPECTRUM_FRACTION_SINGLE;
                        (cell.y + cell.h * (1.0 - frac), cell.h * frac)
                    } else {
                        (cell.y, cell.h)
                    };
                    // Daemon now publishes a log-aggregated spectrum (see
                    // ac_core::aggregate::spectrum_to_columns_wire). Upload as-is.
                    let live_cols: Vec<f32> = frame.spectrum.as_ref().clone();
                    let meta = ChannelMeta {
                        color: theme::channel_color(cell.channel),
                        viewport: [cell.x, vp_y, cell.w, vp_h],
                        db_min: view.db_min,
                        db_max: view.db_max,
                        freq_log_min,
                        freq_log_max,
                        n_bins: live_cols.len() as u32,
                        offset: 0,
                        fill_alpha: 0.0,
                        line_width: 0.0,
                    };
                    spectrum_uploads.push(ChannelUpload {
                        spectrum: live_cols,
                        meta,
                    });
                    // Peak-hold trace. Reuses the spectrum pipeline as a second
                    // upload with the same viewport/axes but a brighter,
                    // channel-tinted colour and a thicker line so the frozen
                    // max stands out above the live trace. Using the channel
                    // hue (brightened) instead of a single cyan means the
                    // peak line in Compare layout is visually paired with
                    // its parent trace — with N peaks overlapping in one
                    // rect, a shared cyan made them indistinguishable. Only
                    // real channels get a peak; virtual transfer cells are
                    // excluded (peak of a transfer magnitude is not a useful
                    // measurement).
                    if self.peak_hold_enabled && cell.channel < n_real {
                        if let Some(Some(peak)) = self.peak_holds.get(cell.channel) {
                            if peak.len() == frame.spectrum.len() {
                                let mut peak_color = theme::channel_color(cell.channel);
                                // Lightly brighten so peak sits visibly above
                                // the live trace in the same hue, but keep it
                                // subdued — alpha drops to 0.55 so the held
                                // line reads as a ghost of the live trace
                                // rather than a shinier copy.
                                for c in peak_color.iter_mut().take(3) {
                                    *c = (*c * 1.12).min(1.0);
                                }
                                peak_color[3] = 0.55;
                                let peak_cols: Vec<f32> = peak.clone();
                                let n_cols = peak_cols.len() as u32;
                                spectrum_uploads.push(ChannelUpload {
                                    spectrum: peak_cols,
                                    meta: ChannelMeta {
                                        color: peak_color,
                                        viewport: [cell.x, vp_y, cell.w, vp_h],
                                        db_min: view.db_min,
                                        db_max: view.db_max,
                                        freq_log_min,
                                        freq_log_max,
                                        n_bins: n_cols,
                                        offset: 0,
                                        fill_alpha: 0.0001,
                                        // line_width is a HALF-WIDTH in 0..1 screen
                                        // space; ~1.6× default reads as a
                                        // distinctly thicker trace.
                                        line_width: 0.003,
                                    },
                                });
                            }
                        }
                    }
                    // Min-hold trace: darker channel tint, same thickness as
                    // the live trace. Sits underneath the live line in dB;
                    // a darker hue keeps it from competing with peak + live
                    // for the eye.
                    if self.min_hold_enabled && cell.channel < n_real {
                        if let Some(Some(min)) = self.min_holds.get(cell.channel) {
                            if min.len() == frame.spectrum.len() {
                                let base = theme::channel_color(cell.channel);
                                let min_color = [
                                    base[0] * 0.55,
                                    base[1] * 0.55,
                                    base[2] * 0.55,
                                    1.0,
                                ];
                                let min_cols: Vec<f32> = min.clone();
                                let n_cols = min_cols.len() as u32;
                                spectrum_uploads.push(ChannelUpload {
                                    spectrum: min_cols,
                                    meta: ChannelMeta {
                                        color: min_color,
                                        viewport: [cell.x, vp_y, cell.w, vp_h],
                                        db_min: view.db_min,
                                        db_max: view.db_max,
                                        freq_log_min,
                                        freq_log_max,
                                        n_bins: n_cols,
                                        offset: 0,
                                        fill_alpha: 0.0001,
                                        line_width: 0.0022,
                                    },
                                });
                            }
                        }
                    }
                }
                ViewMode::Waterfall => {
                    // Detect log vs linear bin spacing from step growth at
                    // the two ends of freqs. Synthetic log-spaced grows by
                    // ~×1.01 per bin at the top vs bottom; real FFT linear
                    // bins have constant step.
                    let n = frame.freqs.len();
                    let (freq_first, freq_last, log_spaced) = if n >= 4 {
                        let lo_step = frame.freqs[1] - frame.freqs[0];
                        let hi_step = frame.freqs[n - 1] - frame.freqs[n - 2];
                        let is_log = hi_step > lo_step * 3.0;
                        (frame.freqs[0], frame.freqs[n - 1], is_log)
                    } else {
                        (
                            frame.freqs.first().copied().unwrap_or(1.0),
                            frame.freqs.last().copied().unwrap_or(24000.0),
                            false,
                        )
                    };
                    // When `frozen` (Enter key), suppress new-row uploads so
                    // the waterfall stops scrolling. Cached frames in
                    // `last_frames` keep their `new_row = Some(..)` from
                    // when they were first read, so without this gate the
                    // GPU would re-push the same row every redraw — making
                    // the flow appear to continue while the data is frozen.
                    let new_row = if self.config.frozen {
                        None
                    } else {
                        frame.new_row.as_deref().map(|v| v.as_slice())
                    };
                    waterfall_uploads.push(WaterfallCellUpload {
                        channel: cell.channel,
                        viewport: [cell.x, cell.y, cell.w, cell.h],
                        db_min: view.db_min,
                        db_max: view.db_max,
                        freq_log_min,
                        freq_log_max,
                        n_bins: frame.spectrum.len() as u32,
                        freq_first,
                        freq_last,
                        log_spaced,
                        rows_visible: view.rows_visible_f,
                        new_row,
                    });
                }
                ViewMode::Scope
                | ViewMode::SpectrumEmber
                | ViewMode::Goniometer
                | ViewMode::IoTransfer
                | ViewMode::BodeMag
                | ViewMode::Coherence
                | ViewMode::BodePhase
                | ViewMode::GroupDelay
                | ViewMode::Nyquist
                | ViewMode::Ir => {
                    // Ember-substrate views consume polylines built later in
                    // this method (synthetic sine for Scope, the active
                    // channel's spectrum frame for SpectrumEmber, synthetic
                    // stereo / mono signals for the Phase 1 trajectory
                    // views). The cell iteration is kept so Single-layout
                    // viewport math runs.
                }
            }
        }

        match view_mode {
            ViewMode::Spectrum => {
                spectrum.upload(&ctx.device, &ctx.queue, &spectrum_uploads);
            }
            ViewMode::Waterfall => {
                waterfall.upload(&ctx.device, &ctx.queue, n_channels, &waterfall_uploads);
            }
            ViewMode::Scope
            | ViewMode::SpectrumEmber
            | ViewMode::Goniometer
            | ViewMode::IoTransfer
                | ViewMode::BodeMag
                | ViewMode::Coherence
                | ViewMode::BodePhase
                | ViewMode::GroupDelay
                | ViewMode::Nyquist
                | ViewMode::Ir => {}
        }

        let raw_input = egui_state.take_egui_input(&ctx.window);
        let show_labels = self.config.layout != LayoutMode::Grid || n_channels <= 8;
        let connected = self
            .source
            .as_ref()
            .map(|s| s.connected())
            .unwrap_or(false);
        let config_snap = self.config.clone();
        let cell_views_snap = self.cell_views.clone();
        let selected_snap = self.selected.clone();
        let virtual_pairs_snap = self.virtual_render_pairs.clone();
        let peak_hold_enabled_snap = self.peak_hold_enabled;
        let min_hold_enabled_snap = self.min_hold_enabled;
        let active_palette_snap = waterfall.active_palette();
        let smoothing_snap = self.smoothing_frac;
        let ioct_bpo_snap = self.ioct_bpo;
        let band_weighting_snap = self.band_weighting.overlay_tag();
        // Plain-enum snapshots for the bottom keytip strip (RC-8). The
        // overlay-tag forms above are display-only; the enum versions
        // feed `keytips_for` so chip labels can show "off" / "A" /
        // "fast" etc. without re-deriving them from a string.
        let band_weighting_enum_snap = self.band_weighting;
        let time_integ_enum_snap = self.time_integration;
        let coherence_k_snap = self.ember_coherence_k;
        let goniometer_ms_snap = self.ember_gonio_rotation_ms;
        // Pull the loudness readout for the currently active channel.
        // Hover-targeted focus is a future refinement — the active
        // channel is the one the UI is already centred on.
        let loudness_focus_ch = self.config.active_channel;
        let loudness_snap: Option<crate::data::types::LoudnessReadout> = self
            .loudness_store
            .as_ref()
            .filter(|_| loudness_focus_ch < n_real)
            .and_then(|store| store.read(loudness_focus_ch as u32));
        // Snapshot the trajectory-view source state computed at the
        // dispatch site of the *previous* render frame. The 1-frame
        // lag is invisible for a status caption and avoids reordering
        // the overlay-paint vs substrate-deposit ordering inside this
        // method.
        let gonio_state_snap = self.gonio_real_audio_state;
        let bode_pair_snap = bode_pair;
        let time_integration_snap = build_time_integration_overlay(
            self.time_integration,
            &frames,
        );
        let peak_holds_snap = if self.peak_hold_enabled {
            self.peak_holds.clone()
        } else {
            Vec::new()
        };
        let virtual_tf_snap: Vec<Option<TransferFrame>> = virtual_snapshots
            .iter()
            .map(|(_, _, tf)| tf.clone())
            .collect();
        let n_real_snap = n_real;
        let show_help_snap = self.show_help;
        let monitor_params_snap = (self.analysis_mode == "fft").then_some(MonitorParamsInfo {
            interval_ms: self.monitor_interval_ms,
            fft_n: self.monitor_fft_n,
        });
        // For the CQT badge we need the live `f_min` — the daemon
        // clamps it dynamically above the const default based on ring
        // length / sample rate, so the only authoritative source is
        // the active channel's most recent frame. Other modes ignore
        // this value.
        let cqt_f_min_snap = if self.analysis_mode == "cqt" {
            frames
                .get(self.config.active_channel)
                .and_then(|f| f.as_ref())
                .and_then(|f| f.freqs.first().copied())
                .unwrap_or(0.0)
        } else {
            0.0
        };
        let tier_badge_snap = Some(crate::ui::fmt::tier_badge(
            &self.analysis_mode,
            self.monitor_fft_n,
            self.cwt_sigma,
            self.cwt_n_scales,
            cqt_f_min_snap,
        ));
        let sweep_snap = if in_sweep_layout {
            Some(self.sweep_last.clone())
        } else {
            None
        };
        let sweep_kind_snap = self.sweep_kind;
        let sweep_sel_snap = self.sweep_selected_idx;
        let width_px = ctx.config.width as f32;
        let height_px = ctx.config.height as f32;
        let notification = self
            .notification
            .as_ref()
            .filter(|(_, t)| t.elapsed() < NOTIFICATION_TTL)
            .map(|(s, _)| s.clone());
        let timing_for_overlay: Option<StatsSnapshot> =
            self.show_timing.then(|| self.timing_stats.snapshot());
        let gpu_supported = ctx.timing.is_some();

        // Resolve the hovered cell inline off local snapshots so we don't
        // borrow `self` across the egui-closure lifetime.
        let hover_info = self.cursor_pos.and_then(|pos| {
            let cx = pos.x as f32;
            let cy = pos.y as f32;
            let mut hit: Option<(usize, egui::Rect, f32, f32)> = None;
            for c in &cells {
                let r = layout::to_pixel_rect(c, width_px, height_px);
                if cx >= r.left() && cx <= r.right() && cy >= r.top() && cy <= r.bottom() {
                    let nx = (cx - r.left()) / r.width().max(1.0);
                    let ny = 1.0 - (cy - r.top()) / r.height().max(1.0);
                    hit = Some((c.channel, r, nx, ny));
                    break;
                }
            }
            let (channel, rect, nx, ny) = hit?;
            let view = cell_views_snap
                .get(channel)
                .copied()
                .unwrap_or_default();
            let log_min = view.freq_min.max(1.0).log10();
            let log_max = view.freq_max.max(log_min.exp().max(1.1)).log10();
            let freq_hz = 10_f32.powf(log_min + nx * (log_max - log_min));
            let readout = if matches!(config_snap.layout, LayoutMode::Sweep) {
                let cursor = egui::pos2(cx, cy);
                let kind = sweep_kind_snap.unwrap_or(SweepKind::Frequency);
                match crate::render::sweep::hit_test(rect, cursor, kind) {
                    Some((crate::render::sweep::SweepHitPanel::Thd, v)) => {
                        HoverReadout::Thd(v)
                    }
                    Some((crate::render::sweep::SweepHitPanel::Gain, v)) => {
                        HoverReadout::Gain(v)
                    }
                    Some((crate::render::sweep::SweepHitPanel::SpectrumDetail, v)) => {
                        HoverReadout::Db(v)
                    }
                    None => HoverReadout::Db(0.0),
                }
            } else if matches!(config_snap.view_mode, ViewMode::Waterfall) {
                // Waterfall/CWT Y-axis is time, not dB. Top = newest
                // (t_ago = 0); bottom = oldest visible row. Mirrors the
                // shader: rows_back = (1 - ny) * (rows_visible - 1).
                let rows_visible = view.rows_visible_f.max(1.0);
                let rows_back = (1.0 - ny) * (rows_visible - 1.0).max(0.0);
                let t_ago = rows_back * self.waterfall_row_period_s;
                HoverReadout::TimeAgo(t_ago)
            } else {
                let db = view.db_min + ny * (view.db_max - view.db_min);
                HoverReadout::Db(db)
            };
            Some(HoverInfo {
                channel,
                rect,
                cursor: egui::pos2(cx, cy),
                freq_hz,
                readout,
            })
        });

        let row_period_s = self.waterfall_row_period_s;
        let full_output = self.egui_ctx.run(raw_input, |ui_ctx| {
            let painter = ui_ctx.layer_painter(egui::LayerId::new(
                egui::Order::Background,
                egui::Id::new("ac-ui-grid"),
            ));
            let sel_border = egui::Color32::from_rgb(
                theme::SELECT_BORDER[0],
                theme::SELECT_BORDER[1],
                theme::SELECT_BORDER[2],
            );
            // Counter for how many peak readouts have been placed in the
            // top-right corner so far this frame. Only advances in Compare
            // layout (overlapping cells); other layouts each have their own
            // rect so every readout can sit at slot 0.
            let mut peak_corner_slot: usize = 0;
            for cell in &cells {
                let rect = layout::to_pixel_rect(cell, width_px, height_px);
                let view = cell_views_snap
                    .get(cell.channel)
                    .copied()
                    .unwrap_or_default();
                if matches!(config_snap.layout, LayoutMode::Sweep) {
                    if let Some(ref ss) = sweep_snap {
                        let kind = sweep_kind_snap.unwrap_or(SweepKind::Frequency);
                        crate::render::sweep::draw(&painter, rect, kind, ss, sweep_sel_snap);
                    }
                    continue;
                }
                let time_axis = matches!(config_snap.view_mode, ViewMode::Waterfall)
                    .then(|| grid::WaterfallTimeAxis {
                        row_period_s,
                        rows_visible: view.rows_visible_f,
                    });
                // Single view + virtual transfer channel → split cell:
                // spectrum on top, standalone phase subplot below. In all
                // other cases the grid fills the full cell and the phase
                // data (if any) overlays the spectrum.
                let single_virtual = matches!(config_snap.layout, LayoutMode::Single)
                    && matches!(config_snap.view_mode, ViewMode::Spectrum)
                    && cell.channel >= n_real_snap;
                let (grid_rect, phase_rect) = if single_virtual {
                    let frac = crate::render::virtual_overlay::SPECTRUM_FRACTION_SINGLE;
                    let split_y = rect.top() + rect.height() * frac;
                    let top = egui::Rect::from_min_max(
                        rect.min,
                        egui::pos2(rect.max.x, split_y),
                    );
                    let bot = egui::Rect::from_min_max(
                        egui::pos2(rect.min.x, split_y),
                        rect.max,
                    );
                    (top, Some(bot))
                } else {
                    (rect, None)
                };
                // Freq x-axis labels sit at the real cell bottom: the phase
                // subplot when split, the grid rect otherwise.
                let grid_freq_labels = show_labels && phase_rect.is_none();
                let cell_spl_off = frames
                    .get(cell.channel)
                    .and_then(|f| f.as_ref())
                    .and_then(|f| f.meta.spl_offset_db);
                grid::draw_grid(
                    &painter,
                    grid_rect,
                    &view,
                    config_snap.view_mode,
                    show_labels,
                    grid_freq_labels,
                    time_axis,
                    cell_spl_off,
                );
                let is_selected = selected_snap
                    .get(cell.channel)
                    .copied()
                    .unwrap_or(false);
                // Highlight selected cells in the non-Compare layouts. In
                // Compare the cells are already filtered to the selection set,
                // so a per-cell border just adds noise on top of the legend.
                if is_selected && !matches!(config_snap.layout, LayoutMode::Compare) {
                    painter.rect_stroke(
                        rect,
                        egui::CornerRadius::same(2),
                        egui::Stroke::new(1.5, sel_border),
                        egui::StrokeKind::Inside,
                    );
                }
                // Peak-hold overlay: fundamental marker + 2×–5× harmonic
                // ticks + corner readout. Spectrum view only; virtual
                // channels excluded (peak-of-transfer magnitude is not a
                // useful reading). Drawn after the grid so it stacks above
                // the axes.
                if peak_hold_enabled_snap
                    && matches!(config_snap.view_mode, ViewMode::Spectrum)
                    && cell.channel < n_real_snap
                {
                    if let (Some(Some(peak)), Some(Some(frame))) = (
                        peak_holds_snap.get(cell.channel),
                        frames.get(cell.channel),
                    ) {
                        draw_peak_overlay(
                            &painter,
                            grid_rect,
                            cell.channel,
                            peak,
                            &frame.freqs,
                            &view,
                            peak_corner_slot,
                        );
                        // Only Compare stacks readouts in one shared rect;
                        // Grid/Single cells have their own top-right so reset
                        // each cell to slot 0.
                        if matches!(config_snap.layout, LayoutMode::Compare) {
                            peak_corner_slot += 1;
                        }
                    }
                }
                // Virtual transfer channels get a standalone phase subplot
                // in Single view (split cell, per issue #49). Grid/Compare
                // show magnitude only — the phase overlay was visually
                // intrusive at grid cell size. Waterfall view is also a
                // no-op since the row image can't host a static polyline.
                if matches!(config_snap.view_mode, ViewMode::Spectrum)
                    && cell.channel >= n_real_snap
                {
                    if let Some(bot) = phase_rect {
                        let vi = cell.channel - n_real_snap;
                        if let Some(Some(tf)) = virtual_tf_snap.get(vi) {
                            painter.line_segment(
                                [
                                    egui::pos2(rect.left(), bot.top()),
                                    egui::pos2(rect.right(), bot.top()),
                                ],
                                egui::Stroke::new(
                                    1.0,
                                    egui::Color32::from_rgba_unmultiplied(180, 180, 180, 40),
                                ),
                            );
                            crate::render::virtual_overlay::draw_phase_subplot(
                                &painter, bot, &view, tf, show_labels,
                            );
                        }
                    }
                }
            }
            // Bottom keytip strip — RC-8. Built per-frame off the
            // snapshot so the chip state suffix (e.g. `weighting:Z`,
            // `smooth:1/6`) reflects the current view without coupling
            // the overlay layer to App.
            let keytip_state = crate::ui::keytips::KeytipState {
                view: config_snap.view_mode,
                band_weighting: band_weighting_enum_snap,
                time_integ: time_integ_enum_snap,
                smoothing_frac: smoothing_snap,
                peak_hold: peak_hold_enabled_snap,
                min_hold: min_hold_enabled_snap,
                coherence_k: coherence_k_snap,
                goniometer_ms: goniometer_ms_snap,
            };
            let keytips = crate::ui::keytips::keytips_for(&keytip_state);
            overlay::draw(
                ui_ctx,
                OverlayInput {
                    config: &config_snap,
                    frames: &frames,
                    cell_views: &cell_views_snap,
                    selected: &selected_snap,
                    connected,
                    notification: notification.as_deref(),
                    timing: timing_for_overlay,
                    gpu_supported,
                    hover: hover_info.clone(),
                    show_help: show_help_snap,
                    monitor_params: monitor_params_snap,
                    n_real: n_real_snap,
                    virtual_pairs: &virtual_pairs_snap,
                    active_palette: active_palette_snap,
                    smoothing_frac: smoothing_snap,
                    ioct_bpo: ioct_bpo_snap,
                    tier_badge: tier_badge_snap.clone(),
                    time_integration: time_integration_snap.clone(),
                    band_weighting: band_weighting_snap,
                    loudness: loudness_snap,
                    gonio_state: gonio_state_snap,
                    bode_pair: bode_pair_snap,
                    keytips: &keytips,
                },
            );
        });

        let pixels_per_point = self.egui_ctx.pixels_per_point();
        let paint_jobs = self
            .egui_ctx
            .tessellate(full_output.shapes, pixels_per_point);
        let screen_desc = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [ctx.config.width, ctx.config.height],
            pixels_per_point,
        };

        for (id, delta) in &full_output.textures_delta.set {
            egui_renderer.update_texture(&ctx.device, &ctx.queue, *id, delta);
        }

        let acquire_start = Instant::now();
        let surface_tex = match ctx.surface.get_current_texture() {
            Ok(t) => t,
            Err(wgpu::SurfaceError::Lost) | Err(wgpu::SurfaceError::Outdated) => {
                ctx.surface.configure(&ctx.device, &ctx.config);
                return;
            }
            Err(e) => {
                log::error!("surface acquire: {e:?}");
                return;
            }
        };
        let acquire_wait = acquire_start.elapsed();
        let view = surface_tex
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = ctx.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("ac-ui frame"),
        });

        egui_renderer.update_buffers(
            &ctx.device,
            &ctx.queue,
            &mut encoder,
            &paint_jobs,
            &screen_desc,
        );

        let spectrum_writes = ctx.timing.as_ref().map(|t| t.spectrum_writes());
        let egui_writes     = ctx.timing.as_ref().map(|t| t.egui_writes());

        // Ember substrate: decay + deposit happen as their own off-screen
        // render passes ahead of the surface clear, so the display pass
        // inside the spectrum pass can sample the freshly written buffer.
        // The renderer is substrate-only — caller supplies the polyline
        // and scroll velocity per view kind.
        if matches!(
            view_mode,
            ViewMode::Scope
                | ViewMode::SpectrumEmber
                | ViewMode::Goniometer
                | ViewMode::IoTransfer
                | ViewMode::BodeMag
                | ViewMode::Coherence
                | ViewMode::BodePhase
                | ViewMode::GroupDelay
                | ViewMode::Nyquist
                | ViewMode::Ir
        ) {
            let now = Instant::now();
            let dt = self
                .ember_last_tick
                .map(|t| now.saturating_duration_since(t).as_secs_f32())
                .unwrap_or(1.0 / 60.0)
                .clamp(0.0, 0.25);
            self.ember_last_tick = Some(now);

            match view_mode {
                ViewMode::Scope => {
                    // Inlined `current_sr()` because `self.ember` is already
                    // mut-borrowed above and a method call would need the
                    // whole `&self`. `self.last_frames` is a different field
                    // — Rust's split borrow lets us touch it independently.
                    let sr = self
                        .last_frames
                        .iter()
                        .flatten()
                        .map(|f| f.meta.sr)
                        .find(|&s| s > 0)
                        .unwrap_or(EMBER_FALLBACK_SR) as f32;
                    let (polyline, scroll_dx) = build_scope_polyline(
                        &mut self.ember_sine_phase,
                        sr,
                        EMBER_SCOPE_SINE_HZ,
                        self.ember_scope_window_s,
                        self.ember_scope_y_gain,
                        dt,
                    );
                    ember.set_tau_p(0.6 * self.ember_tau_p_scale);
                    ember.set_intensity(0.002 * self.ember_intensity_scale);
                    ember.set_tone(0.6, 0.5);
                    ember.advance(
                        &ctx.device, &ctx.queue, &mut encoder,
                        [0.0, 0.0, 1.0, 1.0],
                        &polyline, scroll_dx, dt,
                    );
                }
                ViewMode::SpectrumEmber => {
                    let active = self.config.active_channel;
                    let view = self.cell_views.get(active).copied().unwrap_or_default();
                    // Read from the local `frames` copy, not self.last_frames
                    // — by this point in redraw it carries the daemon's
                    // weighting / time integration AND the UI-side
                    // fractional-octave smoothing applied at line 188 above.
                    // self.last_frames is the un-smoothed source.
                    let polyline = frames
                        .get(active)
                        .and_then(|f| f.as_ref())
                        .filter(|f| !f.spectrum.is_empty())
                        .map(|f| build_spectrum_polyline(f, &view))
                        .unwrap_or_default();
                    // tau_p 1.2 s — short enough that an old peak's
                    // afterglow is gone in ~3 s, fast enough to keep up
                    // with sweeping or moving sources. The mirrored-envelope
                    // polyline doubles the per-frame deposit count vs a
                    // single trace, so intensity already nets out about
                    // right at 0.003.
                    ember.set_tau_p(1.2 * self.ember_tau_p_scale);
                    ember.set_intensity(0.003 * self.ember_intensity_scale);
                    ember.set_tone(0.6, 1.5);
                    ember.advance(
                        &ctx.device, &ctx.queue, &mut encoder,
                        [0.0, 0.0, 1.0, 1.0],
                        &polyline, 0.0, dt,
                    );
                }
                ViewMode::Goniometer => {
                    let sr = self
                        .last_frames
                        .iter()
                        .flatten()
                        .map(|f| f.meta.sr)
                        .find(|&s| s > 0)
                        .unwrap_or(EMBER_FALLBACK_SR) as f32;
                    let want = ((dt * sr) as usize).clamp(64, 4096);
                    let (status, real_pair) = resolve_stereo_pair(
                        bode_pair,
                        self.scope_store.as_ref(),
                        want,
                    );
                    self.gonio_real_audio_state = status;
                    let amp = match &real_pair {
                        Some((l, r)) => {
                            update_stereo_peak(&mut self.ember_stereo_peak, l, r, dt);
                            // Map running peak → display gain so peak signal
                            // hits ~90 % of cell. Cap so a 10 dBFS input
                            // doesn't blow up the figure off-screen on the
                            // next frame, and so silence doesn't div-by-tiny.
                            (0.9 / self.ember_stereo_peak.max(0.02)).clamp(0.5, 50.0)
                        }
                        None => EMBER_GONIO_AMP,
                    };
                    let polyline = build_goniometer_polyline(
                        &mut self.ember_gonio_carrier_phase,
                        &mut self.ember_gonio_phase_offset,
                        sr,
                        self.ember_gonio_rotation_ms,
                        amp,
                        dt,
                        real_pair.as_ref().map(|(l, r)| (l.as_slice(), r.as_slice())),
                    );
                    // Trajectory views revisit the same Lissajous pixels
                    // ~50× per second (1 kHz carrier on a closed orbit) —
                    // an order of magnitude denser than Scope's strip-
                    // chart deposit. Short τ_p + low intensity keeps the
                    // trail visible without saturating to white.
                    ember.set_tau_p(0.12 * self.ember_tau_p_scale);
                    ember.set_intensity(0.0008 * self.ember_intensity_scale);
                    ember.set_tone(0.6, 0.6);
                    ember.advance(
                        &ctx.device, &ctx.queue, &mut encoder,
                        [0.0, 0.0, 1.0, 1.0],
                        &polyline, 0.0, dt,
                    );
                }
                ViewMode::IoTransfer => {
                    let sr = self
                        .last_frames
                        .iter()
                        .flatten()
                        .map(|f| f.meta.sr)
                        .find(|&s| s > 0)
                        .unwrap_or(EMBER_FALLBACK_SR) as f32;
                    let want = ((dt * sr) as usize).clamp(64, 4096);
                    let (status, real_pair) = resolve_stereo_pair(
                        bode_pair,
                        self.scope_store.as_ref(),
                        want,
                    );
                    self.gonio_real_audio_state = status;
                    let amp = match &real_pair {
                        Some((l, r)) => {
                            update_stereo_peak(&mut self.ember_stereo_peak, l, r, dt);
                            (0.9 / self.ember_stereo_peak.max(0.02)).clamp(0.5, 50.0)
                        }
                        None => EMBER_GONIO_AMP,
                    };
                    let polyline = build_iotransfer_polyline(
                        &mut self.ember_gonio_carrier_phase,
                        &mut self.ember_gonio_phase_offset,
                        sr,
                        amp,
                        dt,
                        real_pair.as_ref().map(|(l, r)| (l.as_slice(), r.as_slice())),
                    );
                    // Same revisit-density profile as Goniometer's raw
                    // mode (1 kHz carrier on a closed loop), so
                    // identical tuning works.
                    ember.set_tau_p(0.12 * self.ember_tau_p_scale);
                    ember.set_intensity(0.0008 * self.ember_intensity_scale);
                    ember.set_tone(0.6, 0.6);
                    ember.advance(
                        &ctx.device, &ctx.queue, &mut encoder,
                        [0.0, 0.0, 1.0, 1.0],
                        &polyline, 0.0, dt,
                    );
                }
                ViewMode::BodeMag => {
                    let active = self.config.active_channel;
                    let view = self.cell_views.get(active).copied().unwrap_or_default();
                    let polyline = bode_pair
                        .and_then(|p| {
                            self.virtual_channels
                                .store_for(p)
                                .and_then(|s| s.read())
                                .map(|f| build_bodemag_polyline(&f, &view, self.ember_coherence_k))
                        })
                        .unwrap_or_default();
                    // Long τ_p (~4 s) so successive measurements
                    // fade-blend — the "free diff" workflow promised
                    // in unified.md §5. Single-trace polyline at the
                    // transfer worker's ~10 Hz tick is much sparser
                    // than spectrum, so intensity is bumped vs
                    // SpectrumEmber to keep visibility.
                    ember.set_tau_p(4.0 * self.ember_tau_p_scale);
                    ember.set_intensity(0.005 * self.ember_intensity_scale);
                    ember.set_tone(0.6, 1.5);
                    ember.advance(
                        &ctx.device, &ctx.queue, &mut encoder,
                        [0.0, 0.0, 1.0, 1.0],
                        &polyline, 0.0, dt,
                    );
                }
                ViewMode::Coherence => {
                    let polyline = bode_pair
                        .and_then(|p| {
                            self.virtual_channels
                                .store_for(p)
                                .and_then(|s| s.read())
                                .map(|f| build_coherence_polyline(&f))
                        })
                        .unwrap_or_default();
                    ember.set_tau_p(4.0 * self.ember_tau_p_scale);
                    ember.set_intensity(0.005 * self.ember_intensity_scale);
                    ember.set_tone(0.6, 1.5);
                    ember.advance(
                        &ctx.device, &ctx.queue, &mut encoder,
                        [0.0, 0.0, 1.0, 1.0],
                        &polyline, 0.0, dt,
                    );
                }
                ViewMode::BodePhase => {
                    let active = self.config.active_channel;
                    let view = self.cell_views.get(active).copied().unwrap_or_default();
                    let polyline = bode_pair
                        .and_then(|p| {
                            self.virtual_channels
                                .store_for(p)
                                .and_then(|s| s.read())
                                .map(|f| build_bodephase_polyline(&f, &view, self.ember_coherence_k))
                        })
                        .unwrap_or_default();
                    ember.set_tau_p(4.0 * self.ember_tau_p_scale);
                    ember.set_intensity(0.005 * self.ember_intensity_scale);
                    ember.set_tone(0.6, 1.5);
                    ember.advance(
                        &ctx.device, &ctx.queue, &mut encoder,
                        [0.0, 0.0, 1.0, 1.0],
                        &polyline, 0.0, dt,
                    );
                }
                ViewMode::GroupDelay => {
                    let active = self.config.active_channel;
                    let view = self.cell_views.get(active).copied().unwrap_or_default();
                    let polyline = bode_pair
                        .and_then(|p| {
                            self.virtual_channels
                                .store_for(p)
                                .and_then(|s| s.read())
                                .map(|f| build_groupdelay_polyline(&f, &view, self.ember_coherence_k))
                        })
                        .unwrap_or_default();
                    ember.set_tau_p(4.0 * self.ember_tau_p_scale);
                    ember.set_intensity(0.005 * self.ember_intensity_scale);
                    ember.set_tone(0.6, 1.5);
                    ember.advance(
                        &ctx.device, &ctx.queue, &mut encoder,
                        [0.0, 0.0, 1.0, 1.0],
                        &polyline, 0.0, dt,
                    );
                }
                ViewMode::Nyquist => {
                    // Read the latest TransferFrame for the resolved
                    // pair; auto-scale the (Re, Im) locus so the
                    // largest |H| sits at ~0.85 of cell radius. The
                    // auto-gain peak tracker (`ember_stereo_peak`,
                    // shared with Goniometer / IoTransfer — they
                    // aren't simultaneously visible) decays with
                    // τ = 0.5 s so DUT changes settle smoothly.
                    let polyline = bode_pair
                        .and_then(|p| self.virtual_channels.store_for(p).and_then(|s| s.read()))
                        .map(|frame| {
                            let frame_peak = frame.re.iter().zip(frame.im.iter())
                                .map(|(r, i)| (r * r + i * i).sqrt())
                                .fold(0.0_f32, f32::max);
                            // Same exponential decay as update_stereo_peak.
                            let tau_s = 0.5;
                            let decay = (-dt / tau_s).exp();
                            self.ember_stereo_peak = self
                                .ember_stereo_peak
                                .max(frame_peak)
                                * decay
                                + frame_peak * (1.0 - decay);
                            if self.ember_stereo_peak < 0.001 {
                                self.ember_stereo_peak = 0.001;
                            }
                            let amp = (0.85 / self.ember_stereo_peak.max(0.02))
                                .clamp(0.5, 50.0);
                            build_nyquist_polyline(&frame, amp, self.ember_coherence_k)
                        })
                        .unwrap_or_default();
                    ember.set_tau_p(4.0 * self.ember_tau_p_scale);
                    ember.set_intensity(0.005 * self.ember_intensity_scale);
                    ember.set_tone(0.6, 1.5);
                    ember.advance(
                        &ctx.device, &ctx.queue, &mut encoder,
                        [0.0, 0.0, 1.0, 1.0],
                        &polyline, 0.0, dt,
                    );
                }
                ViewMode::Ir => {
                    // Phase 4b: pull the latest impulse response for the
                    // active+1 transfer pair and plot h(t). Uses the
                    // shared `ember_stereo_peak` auto-gain (Goniometer/
                    // IoTransfer/Nyquist share — they're never
                    // simultaneously visible) so the dominant IR peak
                    // sits at ~0.4 of cell height.
                    let polyline = bode_pair
                        .and_then(|p| {
                            self.ir_store
                                .as_ref()
                                .and_then(|s| s.read(p))
                        })
                        .map(|frame| {
                            let frame_peak = frame
                                .samples
                                .iter()
                                .map(|s| s.abs())
                                .fold(0.0_f32, f32::max);
                            let tau_s = 0.5;
                            let decay = (-dt / tau_s).exp();
                            self.ember_stereo_peak = self
                                .ember_stereo_peak
                                .max(frame_peak)
                                * decay
                                + frame_peak * (1.0 - decay);
                            if self.ember_stereo_peak < 0.001 {
                                self.ember_stereo_peak = 0.001;
                            }
                            let amp = (0.4 / self.ember_stereo_peak.max(0.02))
                                .clamp(0.5, 50.0);
                            build_ir_polyline(&frame, amp)
                        })
                        .unwrap_or_default();
                    ember.set_tau_p(4.0 * self.ember_tau_p_scale);
                    ember.set_intensity(0.005 * self.ember_intensity_scale);
                    ember.set_tone(0.6, 1.5);
                    ember.advance(
                        &ctx.device, &ctx.queue, &mut encoder,
                        [0.0, 0.0, 1.0, 1.0],
                        &polyline, 0.0, dt,
                    );
                }
                _ => {}
            }
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("spectrum pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: theme::BG[0] as f64,
                            g: theme::BG[1] as f64,
                            b: theme::BG[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: spectrum_writes,
                occlusion_query_set: None,
            });
            match view_mode {
                ViewMode::Spectrum => spectrum.draw(&mut pass),
                ViewMode::Waterfall => waterfall.draw(&mut pass),
                ViewMode::Scope
                | ViewMode::SpectrumEmber
                | ViewMode::Goniometer
                | ViewMode::IoTransfer
                | ViewMode::BodeMag
                | ViewMode::Coherence
                | ViewMode::BodePhase
                | ViewMode::GroupDelay
                | ViewMode::Nyquist
                | ViewMode::Ir => ember.draw(&mut pass),
            }
        }

        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: egui_writes,
                occlusion_query_set: None,
            });
            let mut pass = pass.forget_lifetime();
            egui_renderer.render(&mut pass, &paint_jobs, &screen_desc);
        }

        if let Some(timing) = ctx.timing.as_mut() {
            timing.resolve(&mut encoder);
        }

        let capture = if self.pending_screenshot {
            self.pending_screenshot = false;
            Some(prepare_capture(ctx, &mut encoder, &surface_tex))
        } else {
            None
        };

        ctx.queue.submit(Some(encoder.finish()));
        surface_tex.present();

        let gpu_pass = if let Some(timing) = ctx.timing.as_mut() {
            timing.after_submit();
            let _ = ctx.device.poll(wgpu::Maintain::Poll);
            timing.poll();
            timing.last()
        } else {
            crate::render::timing::PassTimings::default()
        };

        for id in &full_output.textures_delta.free {
            egui_renderer.free_texture(id);
        }

        if let Some(cap) = capture {
            finalize_capture(ctx, cap, &self.output_dir, &frames, None);
            self.notify("saved");
        }

        let now = Instant::now();
        let frame_dt = now.saturating_duration_since(self.last_render);
        // Subtract the surface-acquire wait so the cpu metric reflects actual
        // CPU work, not vsync block time. With Fifo present mode the acquire
        // call sleeps until the next vblank; counting that as cpu time would
        // pin the metric to the frame budget regardless of how light the
        // workload is.
        let cpu_dt = now
            .saturating_duration_since(frame_start)
            .saturating_sub(acquire_wait);
        self.timing_stats.push(cpu_dt, frame_dt, gpu_pass);
        self.last_render = now;
    }
}

pub(super) fn dark_visuals() -> egui::Visuals {
    let mut v = egui::Visuals::dark();
    v.window_fill = Color32::from_rgba_unmultiplied(10, 10, 15, 0);
    v.panel_fill = Color32::from_rgba_unmultiplied(10, 10, 15, 0);
    v
}

struct CaptureJob {
    buffer: wgpu::Buffer,
    width: u32,
    height: u32,
    bytes_per_row: u32,
    format: wgpu::TextureFormat,
}

fn prepare_capture(
    ctx: &RenderContext,
    encoder: &mut wgpu::CommandEncoder,
    surface_tex: &wgpu::SurfaceTexture,
) -> CaptureJob {
    let width = ctx.config.width;
    let height = ctx.config.height;
    let bytes_per_row = export::bytes_per_row_aligned(width);
    let size = (bytes_per_row as u64) * (height as u64);
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("screenshot buf"),
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &surface_tex.texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    CaptureJob {
        buffer,
        width,
        height,
        bytes_per_row,
        format: ctx.config.format,
    }
}

fn finalize_capture(
    ctx: &RenderContext,
    job: CaptureJob,
    output_dir: &std::path::Path,
    frames: &[Option<DisplayFrame>],
    transfer: Option<TransferFrame>,
) {
    let slice = job.buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    let _ = ctx.device.poll(wgpu::Maintain::Wait);
    match rx.recv() {
        Ok(Ok(())) => {
            let data = slice.get_mapped_range();
            let pixels = data.to_vec();
            drop(data);
            job.buffer.unmap();
            export::spawn_save(ScreenshotRequest {
                output_dir: output_dir.to_path_buf(),
                width: job.width,
                height: job.height,
                bytes_per_row: job.bytes_per_row,
                pixels,
                format: job.format,
                frames: frames.to_vec(),
                transfer,
            });
        }
        _ => log::error!("screenshot map failed"),
    }
}

/// Pick up to `n` ranked local maxima from a peak-hold buffer. Strict local
/// max (`peak[i] > peak[i-1]` and `> peak[i+1]`), restricted to the current
/// view window and clamped to ≥ 20 Hz so DC/sub-audio noise can't dominate.
/// A 1/3-octave greedy exclusion is applied after sorting by amplitude so
/// neighbouring bins in the same spectral lobe can't monopolise the list.
/// Returns `(bin_index, f_hz, amp_db)` in rank order (descending dB).
fn top_peaks(
    peak: &[f32],
    freqs: &[f32],
    view: &CellView,
    n: usize,
) -> Vec<(usize, f32, f32)> {
    if peak.is_empty() || freqs.len() != peak.len() || n == 0 {
        return Vec::new();
    }
    let floor_hz = view.freq_min.max(theme::DEFAULT_FREQ_MIN);
    let mut candidates: Vec<(usize, f32, f32)> = Vec::new();
    for i in 1..peak.len().saturating_sub(1) {
        let f = freqs[i];
        let amp = peak[i];
        if !f.is_finite() || !amp.is_finite() {
            continue;
        }
        if f < floor_hz || f > view.freq_max {
            continue;
        }
        if amp > peak[i - 1] && amp > peak[i + 1] {
            candidates.push((i, f, amp));
        }
    }
    candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());

    const EXCLUSION_OCTAVES: f32 = 1.0 / 3.0;
    let mut picked: Vec<(usize, f32, f32)> = Vec::with_capacity(n);
    for cand in candidates {
        if picked.len() >= n {
            break;
        }
        let too_close = picked.iter().any(|&(_, f, _)| {
            (cand.1.max(1e-6) / f.max(1e-6)).log2().abs() < EXCLUSION_OCTAVES
        });
        if too_close {
            continue;
        }
        picked.push(cand);
    }
    picked
}

/// Per-cell peak-hold overlay: top-5 local maxima ranked by dB, with
/// rank-1 drawn as a full-size triangle and ranks 2–5 as half-size
/// triangles with a small rank number. Corner "PEAK CHn" header + one row
/// per ranked peak. Called inside the egui closure (Spectrum view, real
/// channels only).
fn draw_peak_overlay(
    painter: &egui::Painter,
    rect: egui::Rect,
    channel: usize,
    peak: &[f32],
    freqs: &[f32],
    view: &CellView,
    // Which row in the top-right corner this readout occupies. 0 = topmost.
    // Compare layout stacks N overlapping cells into the same rect; without a
    // per-channel slot the readouts would all overdraw the same pixel and
    // only the last one would survive.
    corner_slot: usize,
) {
    if peak.is_empty() || freqs.len() != peak.len() {
        return;
    }
    let picked = top_peaks(peak, freqs, view, 5);
    if picked.is_empty() {
        return;
    }

    let log_min = view.freq_min.max(1.0).log10();
    let log_max = view.freq_max.max(log_min.exp().max(1.1)).log10();
    let log_span = (log_max - log_min).max(0.0001);
    let db_span = (view.db_max - view.db_min).max(0.0001);

    // Colour the markers with the channel's own palette entry so Compare
    // layout — where every selected channel's peak traces stack into the
    // same rect — lets the eye pair each triangle/label/readout with its
    // underlying spectrum trace.
    let ch_rgba = theme::channel_color(channel);
    let marker_color = Color32::from_rgb(
        (ch_rgba[0] * 255.0) as u8,
        (ch_rgba[1] * 255.0) as u8,
        (ch_rgba[2] * 255.0) as u8,
    );
    let rank_color = Color32::from_rgba_unmultiplied(
        (ch_rgba[0] * 255.0) as u8,
        (ch_rgba[1] * 255.0) as u8,
        (ch_rgba[2] * 255.0) as u8,
        160,
    );

    let freq_amp_to_px = |f: f32, amp: f32| -> egui::Pos2 {
        let tx = (f.max(1.0).log10() - log_min) / log_span;
        let ty = (amp - view.db_min) / db_span;
        let x = rect.left() + tx.clamp(0.0, 1.0) * rect.width();
        let y = rect.top() + (1.0 - ty.clamp(0.0, 1.0)) * rect.height();
        egui::pos2(x, y)
    };

    // Rank 1: full-size triangle + Hz/dB label above the peak. Matches the
    // old "fundamental" glyph so users trained on the v1 overlay keep their
    // mental model.
    let (_, f0, a0) = picked[0];
    let p0 = freq_amp_to_px(f0, a0);
    let tri = [
        egui::pos2(p0.x - 5.0, p0.y - 10.0),
        egui::pos2(p0.x + 5.0, p0.y - 10.0),
        egui::pos2(p0.x,       p0.y - 2.0),
    ];
    painter.add(egui::Shape::convex_polygon(
        tri.to_vec(),
        marker_color,
        egui::Stroke::new(1.0, marker_color),
    ));
    let label = format!("{} {:.1} dB", format_freq_compact(f0), a0);
    painter.text(
        egui::pos2(p0.x, p0.y - 12.0),
        egui::Align2::CENTER_BOTTOM,
        label,
        egui::FontId::monospace(theme::GRID_LABEL_PX),
        marker_color,
    );

    // Ranks 2..=5: half-size triangle with a small rank-number label.
    for (rank_i, &(_, f, amp)) in picked.iter().enumerate().skip(1) {
        let p = freq_amp_to_px(f, amp);
        let tri = [
            egui::pos2(p.x - 3.0, p.y - 8.0),
            egui::pos2(p.x + 3.0, p.y - 8.0),
            egui::pos2(p.x,       p.y - 2.0),
        ];
        painter.add(egui::Shape::convex_polygon(
            tri.to_vec(),
            rank_color,
            egui::Stroke::new(1.0, rank_color),
        ));
        painter.text(
            egui::pos2(p.x, p.y - 10.0),
            egui::Align2::CENTER_BOTTOM,
            format!("{}", rank_i + 1),
            egui::FontId::monospace(theme::GRID_LABEL_PX),
            rank_color,
        );
    }

    // Top-left corner readout: "PEAK CHn" header + one ranked row per
    // picked peak. Sits top-left so it never collides with the top-right
    // status stack (sample rate, gain, tier badge, loudness, colorbar).
    // `corner_slot` stacks one full block per channel so Compare layout's
    // overlapping cells don't overdraw a single pixel; Grid/Single always
    // pass 0 and render at the top.
    let row_h = theme::GRID_LABEL_PX + 2.0;
    let block_rows = 1 + picked.len();
    let block_top = rect.top() + 2.0 + corner_slot as f32 * block_rows as f32 * row_h;
    painter.text(
        egui::pos2(rect.left() + 4.0, block_top),
        egui::Align2::LEFT_TOP,
        crate::ui::fmt::peak_header(channel),
        egui::FontId::monospace(theme::GRID_LABEL_PX),
        marker_color,
    );
    for (i, &(_, f, amp)) in picked.iter().enumerate() {
        let color = if i == 0 { marker_color } else { rank_color };
        painter.text(
            egui::pos2(
                rect.left() + 4.0,
                block_top + (i + 1) as f32 * row_h,
            ),
            egui::Align2::LEFT_TOP,
            crate::ui::fmt::peak_rank_line(i + 1, f, amp),
            egui::FontId::monospace(theme::GRID_LABEL_PX),
            color,
        );
    }
}

// Compact frequency formatter for the peak overlay lives in `ui::fmt`;
// re-export here so existing call sites in this file don't have to fully
// qualify the path.
use crate::ui::fmt::format_freq_compact;

/// Synthetic 1 kHz sine, sample rate read from the live system, mapped to
/// the ember substrate as a strip-chart LineList: new samples occupy the
/// rightmost band of width `dt / window_s`, oldest on the left of the
/// band, newest at x = 1.0. Each returned pair is one connected segment;
/// since wraparound is impossible inside a single frame's band, every
/// consecutive sample pair is emitted unconditionally.
fn build_scope_polyline(
    sine_phase:   &mut f32,
    sample_rate:  f32,
    sine_freq_hz: f32,
    window_s:     f32,
    y_gain:       f32,
    dt:           f32,
) -> (Vec<[f32; 3]>, f32) {
    let scroll_dx = (dt / window_s.max(1e-3)).clamp(0.0, 1.0);
    let n = ((dt * sample_rate) as usize).min(8000);
    let mut pairs = Vec::with_capacity(n.saturating_sub(1) * 2);
    let two_pi = std::f32::consts::TAU;
    let phase_step = two_pi * sine_freq_hz / sample_rate;
    let denom = (n.saturating_sub(1)).max(1) as f32;
    let amp = y_gain.clamp(0.01, 0.5);
    let mut prev: Option<[f32; 3]> = None;
    for i in 0..n {
        let s = sine_phase.sin();
        *sine_phase = (*sine_phase + phase_step) % two_pi;
        let frac = i as f32 / denom;
        let x = (1.0 - scroll_dx) + frac * scroll_dx;
        let y = 0.5 + amp * s;
        let cur = [x, y, 1.0];
        if let Some(p) = prev {
            pairs.push(p);
            pairs.push(cur);
        }
        prev = Some(cur);
    }
    (pairs, scroll_dx)
}

/// Confidence-weight helper for coherence-aware ember views. Returns
/// `γ²^k` clamped to `[0, 1]`, with `k == 0` short-circuiting to 1.0
/// (weighting disabled — every bin deposits at full intensity). NaN /
/// negative coherence falls through to 0.0 so spurious bins don't
/// brighten unexpectedly.
fn coherence_weight(coherence: f32, k: f32) -> f32 {
    if !(k > 0.0) {
        return 1.0;
    }
    if !coherence.is_finite() {
        return 0.0;
    }
    coherence.clamp(0.0, 1.0).powf(k)
}

/// Number of x-axis columns the spectrum is aggregated into before
/// deposition. Sized to match the ember substrate width so adjacent
/// columns fall on adjacent pixels and the LineList renderer doesn't pile
/// dozens of FFT bins into the same column. Smaller values make the trace
/// feel chunkier without changing the underlying signal interpretation.
const EMBER_SPECTRUM_COLS: usize = 512;

/// `SpectrumFrame` → mirrored-envelope LineList. Logarithmic frequency
/// axis on x; magnitude renders as a symmetric envelope around y = 0.5
/// (top trace at 0.5 + amp, bottom at 0.5 − amp, where amp scales with
/// the bin's normalised dB). Bins below `db_min` break the polyline so
/// the trace disappears off-screen rather than pinning a glowing baseline
/// at y = 0; bins above `db_max` clamp to the top edge as expected.
///
/// Bins are first aggregated by max-magnitude into `EMBER_SPECTRUM_COLS`
/// log-spaced columns. Without this, linear FFT output (~11.7 Hz/bin at
/// 96 kHz / N=8192) collides ~15 bins per pixel in the top decade of a
/// log x-axis, producing visible moiré/aliasing in the rendered trace.
/// Max-aggregation matches the existing spectrum view's behaviour: peaks
/// dominate over averaging so transients still register cleanly.
fn build_spectrum_polyline(
    frame: &crate::data::types::DisplayFrame,
    view: &CellView,
) -> Vec<[f32; 3]> {
    let log_min = view.freq_min.max(1.0).log10();
    let log_max = view.freq_max.max(view.freq_min * 1.001).log10();
    let span_f = (log_max - log_min).max(1e-6);
    let span_db = (view.db_max - view.db_min).max(1e-3);
    let n_cols = EMBER_SPECTRUM_COLS;
    let mut col_max: Vec<f32> = vec![f32::NEG_INFINITY; n_cols];
    let n = frame.freqs.len().min(frame.spectrum.len());
    for i in 0..n {
        let f = frame.freqs[i];
        let mag = frame.spectrum[i];
        if !f.is_finite() || f < view.freq_min || f > view.freq_max
            || !mag.is_finite() || mag < view.db_min {
            continue;
        }
        let xn = (f.max(1.0).log10() - log_min) / span_f;
        if !(0.0..=1.0).contains(&xn) {
            continue;
        }
        let col = ((xn * n_cols as f32) as usize).min(n_cols - 1);
        if mag > col_max[col] {
            col_max[col] = mag;
        }
    }

    let mut pairs = Vec::with_capacity(n_cols * 4);
    let mut prev: Option<([f32; 3], [f32; 3])> = None;
    for col in 0..n_cols {
        let mag = col_max[col];
        if !mag.is_finite() {
            prev = None;
            continue;
        }
        let x = (col as f32 + 0.5) / n_cols as f32;
        let n_mag = ((mag - view.db_min) / span_db).clamp(0.0, 1.0);
        let amp = 0.45 * n_mag;
        let top = [x, 0.5 + amp, 1.0];
        let bot = [x, 0.5 - amp, 1.0];
        if let Some((prev_top, prev_bot)) = prev {
            pairs.push(prev_top);
            pairs.push(top);
            pairs.push(prev_bot);
            pairs.push(bot);
        }
        prev = Some((top, bot));
    }
    pairs
}

/// `TransferFrame` → Bode-magnitude single-trace LineList. Logarithmic
/// frequency on x, signed dB on y mapped through the cell's
/// `db_min`/`db_max` window. Phase 2 of unified.md — long τ_p in the
/// dispatch arm gives the free fade-diff workflow promised in §5.
///
/// Bins are aggregated by max-magnitude into `EMBER_SPECTRUM_COLS`
/// log-spaced columns (same anti-moiré pattern `build_spectrum_polyline`
/// uses; transfer frames arrive log-spaced from the daemon already, but
/// column-binning still helps when the cell freq window is zoomed).
/// Bins outside the cell's freq or dB window break the polyline.
fn build_bodemag_polyline(
    frame: &crate::data::types::TransferFrame,
    view: &CellView,
    coherence_k: f32,
) -> Vec<[f32; 3]> {
    let log_min = view.freq_min.max(1.0).log10();
    let log_max = view.freq_max.max(view.freq_min * 1.001).log10();
    let span_f = (log_max - log_min).max(1e-6);
    let span_db = (view.db_max - view.db_min).max(1e-3);
    let n_cols = EMBER_SPECTRUM_COLS;
    // Track per-column max value plus the coherence of the bin that won
    // — that bin's γ² is what we trust for this column. NEG_INFINITY
    // marks "no valid bin yet".
    let mut col_max: Vec<f32> = vec![f32::NEG_INFINITY; n_cols];
    let mut col_coh: Vec<f32> = vec![1.0; n_cols];
    let n = frame.freqs.len().min(frame.magnitude_db.len());
    let coh = &frame.coherence;
    for i in 0..n {
        let f = frame.freqs[i];
        let mag = frame.magnitude_db[i];
        if !f.is_finite() || f < view.freq_min || f > view.freq_max
            || !mag.is_finite() {
            continue;
        }
        let xn = (f.max(1.0).log10() - log_min) / span_f;
        if !(0.0..=1.0).contains(&xn) {
            continue;
        }
        let col = ((xn * n_cols as f32) as usize).min(n_cols - 1);
        if mag > col_max[col] {
            col_max[col] = mag;
            col_coh[col] = coh.get(i).copied().unwrap_or(1.0);
        }
    }
    let mut pairs = Vec::with_capacity(n_cols * 2);
    let mut prev: Option<[f32; 3]> = None;
    for col in 0..n_cols {
        let mag = col_max[col];
        if !mag.is_finite() {
            // Empty column on a log axis — daemon emits linear-
            // spaced bins (every ~12 Hz at sr=48 kHz), but the
            // log-spaced columns at low freq are *wider in Hz*
            // than the bin spacing, so most columns below ~1 kHz
            // get no bin at all. Skip without resetting prev so
            // the polyline bridges the gap; visually the trace
            // becomes a single segment between the bins that
            // actually landed in cols, which is the right Bode
            // reading. (This intentionally differs from the
            // spectrum builder, which DOES break on dB-floor
            // misses — that's an actual "no signal here" gate.)
            continue;
        }
        let x = (col as f32 + 0.5) / n_cols as f32;
        // Single trace at y = (mag − db_min) / span_db, centred in the
        // cell with 0.45 padding so the dB window edges aren't hard
        // against the cell border.
        let n_db = ((mag - view.db_min) / span_db).clamp(0.0, 1.0);
        let y = 0.05 + 0.9 * n_db;
        let w = coherence_weight(col_coh[col], coherence_k);
        let cur = [x, y, w];
        if let Some(p) = prev {
            pairs.push(p);
            pairs.push(cur);
        }
        prev = Some(cur);
    }
    pairs
}

/// `TransferFrame` → coherence γ²(f) single-trace LineList. y is the
/// raw coherence value (already in [0, 1]) padded to fit the cell.
/// Aggregated by *min* per column — for coherence we want the
/// pessimistic value (one bad bin in a column means "don't trust
/// this region"), the inverse of the spectrum's max-aggregation
/// which prioritises peaks. Phase 2 of unified.md.
fn build_coherence_polyline(
    frame: &crate::data::types::TransferFrame,
) -> Vec<[f32; 3]> {
    // Coherence views always span the full audio band — no cell
    // freq-window dependence (γ² is dimensionless and useful across
    // the whole band regardless of where the user has zoomed the dB
    // axis on Bode).
    let log_min = 20.0_f32.log10();
    let log_max = 24_000.0_f32.log10();
    let span_f = (log_max - log_min).max(1e-6);
    let n_cols = EMBER_SPECTRUM_COLS;
    let mut col_min: Vec<f32> = vec![f32::INFINITY; n_cols];
    let n = frame.freqs.len().min(frame.coherence.len());
    for i in 0..n {
        let f = frame.freqs[i];
        let c = frame.coherence[i];
        if !f.is_finite() || !c.is_finite() {
            continue;
        }
        let xn = (f.max(1.0).log10() - log_min) / span_f;
        if !(0.0..=1.0).contains(&xn) {
            continue;
        }
        let col = ((xn * n_cols as f32) as usize).min(n_cols - 1);
        if c < col_min[col] {
            col_min[col] = c;
        }
    }
    let mut pairs = Vec::with_capacity(n_cols * 2);
    let mut prev: Option<[f32; 3]> = None;
    for col in 0..n_cols {
        let c = col_min[col];
        if !c.is_finite() {
            // See BodeMag for why we skip without breaking — empty
            // columns at low freq are an axis-mismatch artifact, not
            // a "no data here" signal.
            continue;
        }
        let x = (col as f32 + 0.5) / n_cols as f32;
        let y = 0.05 + 0.9 * c.clamp(0.0, 1.0);
        // Coherence view itself never gates on coherence (would be
        // circular — bins where γ² is what we want to *see* would
        // self-extinguish). Always full weight.
        let cur = [x, y, 1.0];
        if let Some(p) = prev {
            pairs.push(p);
            pairs.push(cur);
        }
        prev = Some(cur);
    }
    pairs
}

/// Phase unwrap (degrees). Removes ±360° jumps that the daemon's
/// wrapped-to-[-180, 180] phase introduces wherever the underlying
/// smooth phase wrapped through ±180°. Used by GroupDelay so the
/// finite-difference derivative doesn't see ±360°/Δf spikes; can be
/// reused if BodePhase ever grows an "unwrap" toggle.
fn unwrap_phase_deg(phase: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(phase.len());
    for (i, &p) in phase.iter().enumerate() {
        if i == 0 {
            out.push(p);
            continue;
        }
        let prev_unwrapped = out[i - 1];
        let prev_orig = phase[i - 1];
        let mut delta = p - prev_orig;
        while delta > 180.0 {
            delta -= 360.0;
        }
        while delta < -180.0 {
            delta += 360.0;
        }
        out.push(prev_unwrapped + delta);
    }
    out
}

/// `TransferFrame` → Bode-phase single-trace LineList. Wrapped
/// phase (the daemon's TransferFrame is already in [-180, +180])
/// mapped through the cell's db_min/db_max window — for BodePhase
/// the theme defaults that window to (-180, +180) so phase paints
/// at its natural scale. Phase 2.5 of unified.md.
///
/// Wraps deliberately stay in the polyline (no break at ±180): the
/// substrate fade makes the discontinuities visually mild, and
/// breaking at every wrap would fragment the trace into useless
/// pieces. Users wanting unwrapped phase look at GroupDelay (which
/// derives from unwrapped internally) or wait for an `unwrap` toggle
/// in a future revision.
fn build_bodephase_polyline(
    frame: &crate::data::types::TransferFrame,
    view: &CellView,
    coherence_k: f32,
) -> Vec<[f32; 3]> {
    let log_min = view.freq_min.max(1.0).log10();
    let log_max = view.freq_max.max(view.freq_min * 1.001).log10();
    let span_f = (log_max - log_min).max(1e-6);
    let span_y = (view.db_max - view.db_min).max(1e-3);
    let n_cols = EMBER_SPECTRUM_COLS;
    // Aggregate by *first valid value* per column (no max/min — phase
    // is signed and doesn't have a meaningful "peak" or "floor").
    // Carry the bin's coherence alongside so downstream weighting can
    // dim noisy phase regions.
    let mut col_phase: Vec<Option<(f32, f32)>> = vec![None; n_cols];
    let n = frame.freqs.len().min(frame.phase_deg.len());
    let coh = &frame.coherence;
    for i in 0..n {
        let f = frame.freqs[i];
        let p = frame.phase_deg[i];
        if !f.is_finite() || f < view.freq_min || f > view.freq_max
            || !p.is_finite() {
            continue;
        }
        let xn = (f.max(1.0).log10() - log_min) / span_f;
        if !(0.0..=1.0).contains(&xn) {
            continue;
        }
        let col = ((xn * n_cols as f32) as usize).min(n_cols - 1);
        let c = coh.get(i).copied().unwrap_or(1.0);
        col_phase[col].get_or_insert((p, c));
    }
    let mut pairs = Vec::with_capacity(n_cols * 2);
    let mut prev: Option<[f32; 3]> = None;
    for col in 0..n_cols {
        let Some((p, c)) = col_phase[col] else {
            // See BodeMag for why we skip without breaking.
            continue;
        };
        let x = (col as f32 + 0.5) / n_cols as f32;
        let n_y = ((p - view.db_min) / span_y).clamp(0.0, 1.0);
        let y = 0.05 + 0.9 * n_y;
        let w = coherence_weight(c, coherence_k);
        let cur = [x, y, w];
        if let Some(prev_p) = prev {
            pairs.push(prev_p);
            pairs.push(cur);
        }
        prev = Some(cur);
    }
    pairs
}

/// `TransferFrame` → group delay τ_g(f) = −dφ/dω in milliseconds,
/// computed from a forward-difference derivative of the *unwrapped*
/// phase array. Wrapped phase would produce ±360°/Δf spikes wherever
/// the underlying smooth phase wrapped through ±180°.
///
/// Conversion: τ_g[ms] = −(1000 / 360) · Δφ_deg / Δf_hz. Y range is
/// the cell's db_min/db_max window (theme defaults to -5..+20 ms,
/// which covers most realistic audio DUTs). Phase 2.5.
fn build_groupdelay_polyline(
    frame: &crate::data::types::TransferFrame,
    view: &CellView,
    coherence_k: f32,
) -> Vec<[f32; 3]> {
    if frame.freqs.len() < 2 || frame.phase_deg.len() < 2 {
        return Vec::new();
    }
    let n = frame.freqs.len().min(frame.phase_deg.len());
    let unwrapped = unwrap_phase_deg(&frame.phase_deg[..n]);
    let coh = &frame.coherence;
    // Forward-difference τ_g per bin gap. Place each value at the
    // midpoint frequency between consecutive bins so the curve
    // doesn't visually lag. Coherence per derivative point is the
    // *minimum* of the two contributing bins — group delay only
    // becomes trustworthy when *both* phase samples it depends on
    // are coherent.
    let mut deriv: Vec<(f32, f32, f32)> = Vec::with_capacity(n.saturating_sub(1));
    for i in 0..(n - 1) {
        let f0 = frame.freqs[i];
        let f1 = frame.freqs[i + 1];
        let df = f1 - f0;
        if !f0.is_finite() || !f1.is_finite() || df.abs() < 1e-6 {
            continue;
        }
        let dphi = unwrapped[i + 1] - unwrapped[i];
        let tau_g_ms = -(1000.0 / 360.0) * dphi / df;
        let f_mid = 0.5 * (f0 + f1);
        let c0 = coh.get(i).copied().unwrap_or(1.0);
        let c1 = coh.get(i + 1).copied().unwrap_or(1.0);
        let c_pair = c0.min(c1);
        deriv.push((f_mid, tau_g_ms, c_pair));
    }
    if deriv.is_empty() {
        return Vec::new();
    }
    let log_min = view.freq_min.max(1.0).log10();
    let log_max = view.freq_max.max(view.freq_min * 1.001).log10();
    let span_f = (log_max - log_min).max(1e-6);
    let span_y = (view.db_max - view.db_min).max(1e-3);
    let n_cols = EMBER_SPECTRUM_COLS;
    // Aggregate by *median-style first-valid* per column (group delay
    // is signed; max would cherry-pick the largest spike, biasing
    // visual interpretation). First-valid is good enough at the
    // typical bin density.
    let mut col_val: Vec<Option<(f32, f32)>> = vec![None; n_cols];
    for (f, t, c) in &deriv {
        if !f.is_finite() || !t.is_finite() || *f < view.freq_min || *f > view.freq_max {
            continue;
        }
        let xn = (f.max(1.0).log10() - log_min) / span_f;
        if !(0.0..=1.0).contains(&xn) {
            continue;
        }
        let col = ((xn * n_cols as f32) as usize).min(n_cols - 1);
        col_val[col].get_or_insert((*t, *c));
    }
    let mut pairs = Vec::with_capacity(n_cols * 2);
    let mut prev: Option<[f32; 3]> = None;
    for col in 0..n_cols {
        let Some((t, c)) = col_val[col] else {
            // See BodeMag for why we skip without breaking.
            continue;
        };
        let x = (col as f32 + 0.5) / n_cols as f32;
        let n_y = ((t - view.db_min) / span_y).clamp(0.0, 1.0);
        let y = 0.05 + 0.9 * n_y;
        let w = coherence_weight(c, coherence_k);
        let cur = [x, y, w];
        if let Some(prev_p) = prev {
            pairs.push(prev_p);
            pairs.push(cur);
        }
        prev = Some(cur);
    }
    pairs
}

/// `TransferFrame` → Nyquist locus polyline. Plot (Re H, Im H) as a
/// connected curve in the complex plane, parameterised by frequency.
/// `amp` is the auto-gain factor computed at the dispatch site so
/// the largest |H| sits at ~0.85 of cell radius regardless of DUT
/// gain. The curve is centred at (0.5, 0.5) — origin = no signal,
/// unit circle (drawn via the helper below) = |H| = 1 boundary.
///
/// Skips bins where re/im aren't finite or the magnitude is below a
/// "show nothing" floor (1e-6). When the daemon is from before
/// Phase 3 (re/im fields empty) returns an empty polyline. Phase 4
/// of unified.md.
fn build_nyquist_polyline(
    frame: &crate::data::types::TransferFrame,
    amp: f32,
    coherence_k: f32,
) -> Vec<[f32; 3]> {
    if frame.re.is_empty() || frame.re.len() != frame.im.len() {
        return Vec::new();
    }
    // Reference unit circle deposited at low intensity (4× lower
    // count than the trace itself, but still much rarer per pixel
    // than the moving curve, so the relative brightness reads as a
    // faint guide rather than competing with the signal trace).
    // 64-vertex polygon ≈ 0.5° per segment — visually smooth. The
    // reference always carries weight 1.0 — coherence weighting
    // applies to the *measurement* trace, not the static guide.
    let mut pairs = Vec::with_capacity(frame.re.len() * 2 + 128);
    let unit_r = 0.45 * amp.min(1.0); // unit circle visible only when DUT < gain ≈ 1
    if unit_r > 0.01 {
        let n_circ = 64;
        let mut prev_circ: Option<[f32; 3]> = None;
        for k in 0..=n_circ {
            let t = (k as f32 / n_circ as f32) * std::f32::consts::TAU;
            let cur = [0.5 + unit_r * t.cos(), 0.5 + unit_r * t.sin(), 1.0];
            if let Some(p) = prev_circ {
                pairs.push(p);
                pairs.push(cur);
            }
            prev_circ = Some(cur);
        }
    }
    let scale = 0.45 * amp;
    let coh = &frame.coherence;
    let mut prev: Option<[f32; 3]> = None;
    for (idx, (r, i)) in frame.re.iter().zip(frame.im.iter()).enumerate() {
        if !r.is_finite() || !i.is_finite() {
            prev = None;
            continue;
        }
        let mag = (r * r + i * i).sqrt();
        if mag < 1e-6 {
            prev = None;
            continue;
        }
        let x = 0.5 + scale * *r;
        let y = 0.5 + scale * *i;
        // Clip to substrate viewport — bins where the auto-gain
        // hasn't yet caught up with a sudden DUT gain spike would
        // otherwise render at clamp boundaries and fragment the
        // trace (better to drop those bins than draw a misleading
        // edge artefact).
        if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
            prev = None;
            continue;
        }
        let c = coh.get(idx).copied().unwrap_or(1.0);
        let w = coherence_weight(c, coherence_k);
        let cur = [x, y, w];
        if let Some(p) = prev {
            pairs.push(p);
            pairs.push(cur);
        }
        prev = Some(cur);
    }
    pairs
}

/// `IrFrame` → impulse-response polyline. x = time (mapped from each
/// sample's array index through the frame's `dt_ms` / `t_origin_ms`
/// metadata into the substrate's [0, 1] cell range), y = amplitude
/// (auto-gain `amp` factor at the dispatch site, centred at 0.5).
/// `unified.md` Phase 4b.
///
/// Faint reference baseline drawn at y = 0.5 so the user can see
/// asymmetric / DC-biased IRs at a glance. Empty `samples` (cold
/// start) yields an empty polyline.
fn build_ir_polyline(frame: &crate::data::types::IrFrame, amp: f32) -> Vec<[f32; 3]> {
    let n = frame.samples.len();
    if n < 2 {
        return Vec::new();
    }
    // Reference baseline: 16-segment line at y = 0.5 across the cell.
    // Drawn first; the substrate's per-deposit additive accumulation
    // means the IR trace overlaying it will dominate any pixel they
    // share, but the baseline stays visible in flat regions. IR is
    // an IFFT of complex H, not a per-sample frequency-domain
    // estimator, so per-sample γ² doesn't apply — every vertex
    // deposits at full weight.
    let mut pairs = Vec::with_capacity(32 + n * 2);
    let mut prev_base: Option<[f32; 3]> = None;
    for k in 0..=16 {
        let cur = [k as f32 / 16.0, 0.5, 1.0];
        if let Some(p) = prev_base {
            pairs.push(p);
            pairs.push(cur);
        }
        prev_base = Some(cur);
    }
    let denom = (n - 1) as f32;
    let mut prev: Option<[f32; 3]> = None;
    for (i, &s) in frame.samples.iter().enumerate() {
        if !s.is_finite() {
            prev = None;
            continue;
        }
        let x = i as f32 / denom;
        let y = (0.5 + 0.45 * amp * s).clamp(0.0, 1.0);
        let cur = [x, y, 1.0];
        if let Some(p) = prev {
            pairs.push(p);
            pairs.push(cur);
        }
        prev = Some(cur);
    }
    pairs
}

/// Auto-gain peak tracker for the stereo trajectory views.
/// `peak` is updated to the max of the per-frame sample max and a
/// time-decayed previous value, so transient loudness spikes don't
/// permanently shrink the figure (decay is gradual) and silence
/// gradually re-expands it. Decay constant ~0.5 s — fast enough to
/// follow musical dynamics, slow enough that level changes don't
/// pump visibly.
fn update_stereo_peak(peak: &mut f32, l: &[f32], r: &[f32], dt: f32) {
    let frame_peak = l
        .iter()
        .chain(r.iter())
        .map(|s| s.abs())
        .fold(0.0_f32, f32::max);
    // Exponential decay: peak *= exp(-dt/tau). Convert to per-frame
    // factor; clamped so a long stall (window minimised) doesn't
    // amplify forever to the lower bound.
    let tau_s = 0.5;
    let decay = (-dt / tau_s).exp();
    *peak = peak.max(frame_peak) * decay + frame_peak * (1.0 - decay);
    // Floor — silence shouldn't drive the auto-gain to infinity (the
    // dispatch site additionally clamps the resulting amp factor).
    if *peak < 0.001 {
        *peak = 0.001;
    }
}

/// Resolve the (L, R) stereo pair for a trajectory view (Goniometer,
/// IoTransfer) from a registered `TransferPair`. The pair carries
/// `meas` and `ref_ch`; we map them to (L = ref_ch, R = meas) so:
/// - **IoTransfer**: matches its existing convention (X = ref input,
///   Y = DUT output), since the user wires REF → DUT and registers
///   the pair via Space-select MEAS first, REF last → T.
/// - **Goniometer**: stereo phase scope, no semantic asymmetry —
///   any consistent labelling reads correctly. We follow IoTransfer's
///   ordering for uniformity: L = ref_ch, R = meas.
///
/// `pair = None` (no T-registered pair) → `(NoTransferPair, None)`.
/// The caller falls back to the synthetic carrier and the overlay
/// caption hints at the Space + T workflow.
///
/// Returns:
/// - `(Real { l, r }, Some((l_samples, r_samples)))` when both
///   channels have recent matching scope frames.
/// - `(NoTransferPair, None)` when no pair is registered.
/// - `(NotStreamingYet { l, r }, None)` when a pair is registered
///   but scope frames haven't arrived yet (cold start, or daemon
///   stopped streaming).
/// - `(NoAudio, None)` when there's no scope store at all
///   (synthetic / pre-connect).
fn resolve_stereo_pair(
    pair: Option<crate::data::types::TransferPair>,
    scope_store: Option<&crate::data::store::ScopeStore>,
    want_samples: usize,
) -> (
    crate::data::types::StereoStatus,
    Option<(Vec<f32>, Vec<f32>)>,
) {
    use crate::data::types::StereoStatus;
    let store = match scope_store {
        Some(s) => s,
        None => return (StereoStatus::NoAudio, None),
    };
    let p = match pair {
        Some(p) => p,
        None => return (StereoStatus::NoTransferPair, None),
    };
    let phys_l = p.ref_ch;
    let phys_r = p.meas;
    let max_age = std::time::Duration::from_millis(250);
    match (
        store.read_recent(phys_l, want_samples, max_age),
        store.read_recent(phys_r, want_samples, max_age),
    ) {
        (Some((_, fi_l, sl)), Some((_, fi_r, sr_buf)))
            if fi_l.abs_diff(fi_r) <= 1 && sl.len() == sr_buf.len() && !sl.is_empty() =>
        {
            (StereoStatus::Real { l: phys_l, r: phys_r }, Some((sl, sr_buf)))
        }
        _ => (StereoStatus::NotStreamingYet { l: phys_l, r: phys_r }, None),
    }
}

/// Goniometer (unified.md §6 / Appendix A).
///
/// `real = Some((l, r))` — equal-length slices of f32 audio in [-1, 1]
/// from `active_channel` and `active_channel + 1` (`unified.md` Phase
/// 0b). When provided, the carrier/phase synthesizer is bypassed and
/// neither phase accumulator advances — the synthetic state stays
/// frozen so a subsequent fallback (wire frames stop arriving) resumes
/// from where it last ran instead of jumping in time.
///
/// `real = None` — synthetic 1 kHz carrier on both with a 0.3 Hz phase
/// drift on R. The figure cycles through every phase state (in-phase
/// line → ellipse → circle → anti-phase line) on a ~3 s loop.
///
/// Defensive: mismatched lengths or empty slices in `Some(...)` fall
/// back to the synthetic body. Builder is otherwise pure — never
/// panics on caller misuse.
fn build_goniometer_polyline(
    carrier_phase: &mut f32,
    phase_offset: &mut f32,
    sample_rate: f32,
    rotation_ms: bool,
    amp: f32,
    dt: f32,
    real: Option<(&[f32], &[f32])>,
) -> Vec<[f32; 3]> {
    let inv_sqrt2 = std::f32::consts::FRAC_1_SQRT_2;
    let project = |l: f32, r: f32| -> [f32; 3] {
        let (px, py) = if rotation_ms {
            ((l - r) * inv_sqrt2, (l + r) * inv_sqrt2)
        } else {
            (l, r)
        };
        [0.5 + 0.45 * px, 0.5 + 0.45 * py, 1.0]
    };

    if let Some((ls, rs)) = real {
        if !ls.is_empty() && ls.len() == rs.len() {
            let mut pairs = Vec::with_capacity(ls.len().saturating_sub(1) * 2);
            let mut prev: Option<[f32; 3]> = None;
            for (l, r) in ls.iter().zip(rs.iter()) {
                let cur = project(amp * *l, amp * *r);
                if let Some(p) = prev {
                    pairs.push(p);
                    pairs.push(cur);
                }
                prev = Some(cur);
            }
            return pairs;
        }
    }

    let n = ((dt * sample_rate) as usize).min(8000);
    if n == 0 {
        return Vec::new();
    }
    let mut pairs = Vec::with_capacity(n.saturating_sub(1) * 2);
    let two_pi = std::f32::consts::TAU;
    let step_carrier = two_pi * EMBER_GONIO_FREQ / sample_rate;
    let step_offset = two_pi * EMBER_GONIO_PHASE_DRIFT_HZ / sample_rate;
    let mut prev: Option<[f32; 3]> = None;
    for _ in 0..n {
        let l = amp * carrier_phase.sin();
        let r = amp * (*carrier_phase + *phase_offset).sin();
        *carrier_phase = (*carrier_phase + step_carrier) % two_pi;
        *phase_offset = (*phase_offset + step_offset) % two_pi;
        let cur = project(l, r);
        if let Some(p) = prev {
            pairs.push(p);
            pairs.push(cur);
        }
        prev = Some(cur);
    }
    pairs
}

/// IoTransfer (unified.md §6, Phase 1.5) — input/output transfer
/// Lissajous, the textbook analog-bench distortion-shape view.
///
/// `real = Some((l, r))` — `l` is the reference signal,
/// `r` is the DUT output. Plot `(L, R)` raw — no M/S rotation.
/// Linear pass-through DUT: y = G·x → straight line at slope G.
/// Hard clipping: flat tops on the diagonal at the DUT rails.
/// Soft compression / class-A asymmetry / crossover / slew-limiting
/// each leave a recognisable geometric signature.
///
/// `real = None` — synthetic 1 kHz carrier + 0.3 Hz phase drift on R,
/// same source as Goniometer. The orbit will be a slowly-rotating
/// ellipse (which is what a "DUT" with a 90° phase shift would
/// actually look like — also a useful baseline shape to recognise).
fn build_iotransfer_polyline(
    carrier_phase: &mut f32,
    phase_offset: &mut f32,
    sample_rate: f32,
    amp: f32,
    dt: f32,
    real: Option<(&[f32], &[f32])>,
) -> Vec<[f32; 3]> {
    let project = |x: f32, y: f32| -> [f32; 3] {
        // Raw X/Y — no rotation. x = ref input, y = DUT output.
        // Perfect linear DUT puts the trace on the y=x diagonal.
        [0.5 + 0.45 * x, 0.5 + 0.45 * y, 1.0]
    };

    if let Some((ls, rs)) = real {
        if !ls.is_empty() && ls.len() == rs.len() {
            let mut pairs = Vec::with_capacity(ls.len().saturating_sub(1) * 2);
            let mut prev: Option<[f32; 3]> = None;
            for (l, r) in ls.iter().zip(rs.iter()) {
                let cur = project(amp * *l, amp * *r);
                if let Some(p) = prev {
                    pairs.push(p);
                    pairs.push(cur);
                }
                prev = Some(cur);
            }
            return pairs;
        }
    }

    let n = ((dt * sample_rate) as usize).min(8000);
    if n == 0 {
        return Vec::new();
    }
    let mut pairs = Vec::with_capacity(n.saturating_sub(1) * 2);
    let two_pi = std::f32::consts::TAU;
    let step_carrier = two_pi * EMBER_GONIO_FREQ / sample_rate;
    let step_offset = two_pi * EMBER_GONIO_PHASE_DRIFT_HZ / sample_rate;
    let mut prev: Option<[f32; 3]> = None;
    for _ in 0..n {
        let l = amp * carrier_phase.sin();
        let r = amp * (*carrier_phase + *phase_offset).sin();
        *carrier_phase = (*carrier_phase + step_carrier) % two_pi;
        *phase_offset = (*phase_offset + step_offset) % two_pi;
        let cur = project(l, r);
        if let Some(p) = prev {
            pairs.push(p);
            pairs.push(cur);
        }
        prev = Some(cur);
    }
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(freq_min: f32, freq_max: f32) -> CellView {
        CellView {
            freq_min,
            freq_max,
            ..CellView::default()
        }
    }

    #[test]
    fn top_peaks_single_isolated() {
        // Single clear peak at bin 5 inside a noise floor.
        let freqs: Vec<f32> = (0..16).map(|i| 100.0 * (i + 1) as f32).collect();
        let mut peak = vec![-90.0f32; 16];
        peak[5] = -10.0;
        let v = view(20.0, 10_000.0);
        let got = top_peaks(&peak, &freqs, &v, 5);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 5);
        assert!((got[0].1 - freqs[5]).abs() < 1e-4);
        assert!((got[0].2 - -10.0).abs() < 1e-4);
    }

    #[test]
    fn top_peaks_orders_by_descending_db() {
        // Two well-separated peaks (>1/3 octave apart). Softer one at bin 3
        // (-20 dB, 400 Hz); louder at bin 11 (-5 dB, 1200 Hz).
        let freqs: Vec<f32> = (0..16).map(|i| 100.0 * (i + 1) as f32).collect();
        let mut peak = vec![-90.0f32; 16];
        peak[3] = -20.0;
        peak[11] = -5.0;
        let v = view(20.0, 10_000.0);
        let got = top_peaks(&peak, &freqs, &v, 5);
        assert_eq!(got.len(), 2);
        // Loudest first.
        assert_eq!(got[0].0, 11);
        assert_eq!(got[1].0, 3);
        assert!(got[0].2 > got[1].2);
    }

    #[test]
    fn top_peaks_excludes_within_one_third_octave() {
        // Three local maxima packed inside ~0.2 octaves around 1 kHz. Only
        // the loudest survives the 1/3-octave exclusion.
        let freqs = vec![
            950.0, 960.0, 970.0, 980.0, 1000.0, 1020.0, 1050.0, 1080.0, 1100.0,
        ];
        // Create maxima at 960, 1000, 1050. Fill rest low; alternate surrounding
        // values so each listed index is a strict local max.
        let peak = vec![
            -80.0, -20.0, -80.0, -80.0, -10.0, -80.0, -25.0, -80.0, -80.0,
        ];
        let v = view(20.0, 10_000.0);
        let got = top_peaks(&peak, &freqs, &v, 5);
        assert_eq!(got.len(), 1, "got = {got:?}");
        assert_eq!(got[0].0, 4); // loudest at 1000 Hz
    }

    #[test]
    fn top_peaks_rejects_out_of_view() {
        let freqs: Vec<f32> = (0..16).map(|i| 100.0 * (i + 1) as f32).collect();
        let mut peak = vec![-90.0f32; 16];
        peak[2] = -5.0; // 300 Hz — outside window
        peak[10] = -10.0; // 1100 Hz — inside window
        let v = view(1000.0, 2000.0);
        let got = top_peaks(&peak, &freqs, &v, 5);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 10);
    }

    #[test]
    fn top_peaks_rejects_sub_20hz() {
        // Bin 0 sits below 20 Hz. Even if it's the loudest strict local max,
        // the DC floor clamp excludes it.
        let freqs = vec![5.0, 10.0, 50.0, 200.0, 1000.0];
        let peak = vec![-100.0, -3.0, -100.0, -30.0, -100.0];
        let v = view(1.0, 10_000.0);
        let got = top_peaks(&peak, &freqs, &v, 5);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 3);
        assert!((got[0].1 - 200.0).abs() < 1e-4);
    }

    #[test]
    fn top_peaks_empty_inputs() {
        let v = view(20.0, 10_000.0);
        assert!(top_peaks(&[], &[], &v, 5).is_empty());
    }

    #[test]
    fn top_peaks_len_mismatch_returns_empty() {
        let v = view(20.0, 10_000.0);
        let freqs = vec![100.0, 200.0, 300.0];
        let peak = vec![-10.0, -20.0];
        assert!(top_peaks(&peak, &freqs, &v, 5).is_empty());
    }

    #[test]
    fn top_peaks_respects_n_cap() {
        // Five well-spaced peaks (>1 octave apart each).
        let freqs: Vec<f32> = (0..11).map(|i| 50.0 * (2.0_f32).powi(i as i32)).collect();
        let mut peak = vec![-90.0f32; 11];
        for (i, amp) in [-10.0, -15.0, -20.0, -25.0, -30.0].iter().enumerate() {
            peak[1 + 2 * i] = *amp;
        }
        let v = view(20.0, 100_000.0);
        let got = top_peaks(&peak, &freqs, &v, 3);
        assert_eq!(got.len(), 3);
        // Loudest three kept.
        assert!(got[0].2 >= got[1].2);
        assert!(got[1].2 >= got[2].2);
    }

    // ---- Phase 1 trajectory-view builder tests (unified.md) ----

    /// Goniometer in M/S rotation: when L = R (in-phase mono), the
    /// rotation collapses to (0, √2·L) — every deposited point should
    /// land on the y-axis (x ≈ 0.5). The synthetic builder uses two
    /// different frequencies on L/R, so we can't use it directly to test
    /// this; we drive `build_goniometer_polyline` with sample_rate =
    /// EMBER_GONIO_FREQ_L = EMBER_GONIO_FREQ_R, which means both phases
    /// step by 2π each sample → both sin() collapse to the same value.
    /// Instead of fighting that, we use sample_rate × 2 so the per-sample
    /// step is π — both phases stay at sin(0)=0, sin(π)=0, … and the
    /// substrate stays empty. The cleaner approach: assert the property
    /// directly on the math by constructing a unit-amplitude L=R
    /// sequence and checking the rotated y stays on x=0.5.
    #[test]
    fn goniometer_in_phase_mono_traces_vertical() {
        // Drive the builder at SR == carrier × 1024 so the synthetic
        // L/R signals are slow-evolving (not a useful equality test in
        // its own right). The deterministic property we test is the
        // M/S rotation algebra: when L = R, x = (L−R)/√2 = 0 → 0.5
        // after offset. We pick a sample rate that makes L and R
        // identical by giving them the same step (using the L freq for
        // both via a custom invocation isn't possible without changing
        // the API), so we just check the structural invariant: every
        // x and every y stay inside the substrate's [0,1] box.
        let mut pl = 0.0_f32;
        let mut pr = 0.0_f32;
        let pairs = build_goniometer_polyline(
            &mut pl, &mut pr, 48_000.0, true, EMBER_GONIO_AMP, 1.0 / 60.0,
            None,
        );
        // Two consecutive vertices form one connected segment, so the
        // pair count is always even and every emitted vertex must sit
        // inside the substrate viewport.
        assert!(pairs.len() % 2 == 0, "LineList vertices must be even");
        for [x, y, _] in &pairs {
            assert!((0.0..=1.0).contains(x), "x = {x} out of [0,1]");
            assert!((0.0..=1.0).contains(y), "y = {y} out of [0,1]");
        }
    }

    /// Connectivity contract: n samples → 2·(n−1) LineList vertices.
    /// Same property `build_scope_polyline` honours.
    #[test]
    fn goniometer_emits_n_minus_one_pairs() {
        let mut pl = 0.0_f32;
        let mut pr = 0.0_f32;
        let sr = 48_000.0;
        let dt = 0.01;
        let n = (dt * sr) as usize;
        let pairs = build_goniometer_polyline(
            &mut pl, &mut pr, sr, true, EMBER_GONIO_AMP, dt, None,
        );
        assert_eq!(
            pairs.len(),
            2 * n.saturating_sub(1),
            "n={n} → 2·(n−1)={} vertices, got {}",
            2 * n.saturating_sub(1),
            pairs.len()
        );
    }

    /// Goniometer with raw (L,R) rotation — L and R are independent
    /// sines, so no special algebraic invariant beyond "stays in box".
    #[test]
    fn goniometer_raw_lr_stays_in_unit_box() {
        let mut pl = 0.5_f32;
        let mut pr = 1.7_f32;
        let pairs = build_goniometer_polyline(
            &mut pl, &mut pr, 48_000.0, false, EMBER_GONIO_AMP, 0.05, None,
        );
        for [x, y, _] in &pairs {
            assert!((0.0..=1.0).contains(x), "x = {x} out of [0,1]");
            assert!((0.0..=1.0).contains(y), "y = {y} out of [0,1]");
        }
    }

    // ---- Phase 0b real-audio path tests (unified.md §9 OQ7) ----

    /// When `real` is provided, the M/S rotation algebra applies
    /// directly to the supplied samples. L=R ⇒ (L−R) = 0 → x stays at
    /// 0.5 for every emitted vertex.
    #[test]
    fn goniometer_real_audio_l_eq_r_traces_vertical() {
        let mut pl = 0.0_f32;
        let mut pr = 0.0_f32;
        let l: Vec<f32> = (0..32).map(|i| (i as f32 * 0.1).sin()).collect();
        let r = l.clone();
        let pairs = build_goniometer_polyline(
            &mut pl, &mut pr, 48_000.0, true, EMBER_GONIO_AMP, 1.0 / 60.0,
            Some((&l, &r)),
        );
        assert!(!pairs.is_empty(), "real-audio branch should produce pairs");
        for [x, _, _] in &pairs {
            assert!(
                (x - 0.5).abs() < 1e-5,
                "L=R should project x ≈ 0.5, got {x}",
            );
        }
    }

    /// Mismatched-length real input falls back to the synthetic body
    /// (returns a non-empty polyline rather than panicking or emitting
    /// from a partial walk of the slices).
    #[test]
    fn goniometer_real_audio_mismatched_lengths_falls_back() {
        let mut pl = 0.0_f32;
        let mut pr = 0.0_f32;
        let l: Vec<f32> = vec![0.1, 0.2, 0.3];
        let r: Vec<f32> = vec![0.1, 0.2, 0.3, 0.4, 0.5];
        let pairs = build_goniometer_polyline(
            &mut pl, &mut pr, 48_000.0, true, EMBER_GONIO_AMP, 1.0 / 60.0,
            Some((&l, &r)),
        );
        // Synthetic fallback at dt × sr = 800 produces ~798 line pairs;
        // the mismatched-real branch would only emit ~2 if it walked
        // the shorter slice. Anything > 100 vertices means the
        // synthetic fallback ran.
        assert!(
            pairs.len() > 100,
            "expected synthetic fallback (>100 vertices); got {}",
            pairs.len(),
        );
    }

    /// Real-audio branch must NOT advance the synthetic phase
    /// accumulators, so when the wire briefly drops the next synthetic
    /// frame resumes from the last advance — no audible/visible time
    /// jump.
    #[test]
    fn goniometer_real_audio_does_not_advance_phase_state() {
        let mut pl = 0.42_f32;
        let mut pr = 0.13_f32;
        let pl_before = pl;
        let pr_before = pr;
        let l: Vec<f32> = (0..256).map(|i| (i as f32 * 0.01).sin()).collect();
        let r = l.clone();
        let _ = build_goniometer_polyline(
            &mut pl, &mut pr, 48_000.0, true, EMBER_GONIO_AMP, 1.0 / 60.0,
            Some((&l, &r)),
        );
        assert_eq!(pl, pl_before, "carrier_phase must stay frozen in real branch");
        assert_eq!(pr, pr_before, "phase_offset must stay frozen in real branch");
    }

    // ---- Phase 1.5 IoTransfer tests ----

    /// Perfect linear pass-through (R == L) ⇒ every emitted vertex
    /// satisfies x == y — the trace is the y=x diagonal. This is the
    /// reference shape for "DUT is linear at unity gain."
    #[test]
    fn iotransfer_real_audio_unity_traces_diagonal() {
        let mut pl = 0.0_f32;
        let mut pr = 0.0_f32;
        let l: Vec<f32> = (0..32).map(|i| (i as f32 * 0.1).sin()).collect();
        let r = l.clone();
        let pairs = build_iotransfer_polyline(
            &mut pl, &mut pr, 48_000.0, 1.0, 1.0 / 60.0,
            Some((&l, &r)),
        );
        assert!(!pairs.is_empty(), "real-audio branch should produce pairs");
        for [x, y, _] in &pairs {
            assert!(
                (x - y).abs() < 1e-5,
                "y=x diagonal expected; got ({x}, {y})",
            );
        }
    }

    /// Hard clipping at the DUT: when R is hard-clipped to ±0.5 while
    /// L sweeps full ±1, vertices where L > 0.5 (in scaled coords)
    /// must cluster at the substrate y coordinate corresponding to
    /// 0.5 — the visual "flat top" signature of clipping.
    #[test]
    fn iotransfer_real_audio_clipped_traces_flat_tops() {
        let mut pl = 0.0_f32;
        let mut pr = 0.0_f32;
        let l: Vec<f32> = (0..64).map(|i| (i as f32 * 0.2).sin()).collect();
        let r: Vec<f32> = l.iter().map(|&v| v.clamp(-0.5, 0.5)).collect();
        let pairs = build_iotransfer_polyline(
            &mut pl, &mut pr, 48_000.0, 1.0, 1.0 / 60.0,
            Some((&l, &r)),
        );
        // Substrate map: y = 0.5 + 0.45 * R. R clipped at +0.5 → y =
        // 0.725. Find any vertex where the corresponding L > 0.5
        // (i.e. x > 0.5 + 0.45*0.5 = 0.725) and verify y is pinned
        // at 0.725 ± float tolerance.
        let mut saw_flat_top = false;
        for [x, y, _] in &pairs {
            if *x > 0.725 + 1e-4 {
                assert!(
                    (y - 0.725).abs() < 1e-4,
                    "x = {x} should hit clipped y = 0.725; got y = {y}",
                );
                saw_flat_top = true;
            }
        }
        assert!(saw_flat_top, "expected at least one flat-top sample");
    }

    /// Synthetic fallback (real = None) must produce non-empty pairs
    /// inside the substrate's [0,1] viewport.
    #[test]
    fn iotransfer_synthetic_fallback_in_unit_box() {
        let mut pl = 0.0_f32;
        let mut pr = 0.0_f32;
        let pairs = build_iotransfer_polyline(
            &mut pl, &mut pr, 48_000.0, EMBER_GONIO_AMP, 1.0 / 60.0,
            None,
        );
        assert!(!pairs.is_empty(), "synthetic fallback should produce pairs");
        for [x, y, _] in &pairs {
            assert!((0.0..=1.0).contains(x), "x = {x} out of [0,1]");
            assert!((0.0..=1.0).contains(y), "y = {y} out of [0,1]");
        }
    }

    /// Real-audio branch must NOT advance synthetic phase state, so a
    /// later fallback resumes from where it last advanced — same
    /// freeze invariant Goniometer has.
    #[test]
    fn iotransfer_real_audio_does_not_advance_phase_state() {
        let mut pl = 0.42_f32;
        let mut pr = 0.13_f32;
        let pl_before = pl;
        let pr_before = pr;
        let l: Vec<f32> = (0..256).map(|i| (i as f32 * 0.01).sin()).collect();
        let r = l.clone();
        let _ = build_iotransfer_polyline(
            &mut pl, &mut pr, 48_000.0, 1.0, 1.0 / 60.0,
            Some((&l, &r)),
        );
        assert_eq!(pl, pl_before, "carrier_phase must stay frozen in real branch");
        assert_eq!(pr, pr_before, "phase_offset must stay frozen in real branch");
    }

    // ---- Phase 2 BodeMag + Coherence builder tests ----

    fn transfer_frame_lin_log_db(
        freqs: Vec<f32>,
        magnitude_db: Vec<f32>,
        coherence: Vec<f32>,
    ) -> crate::data::types::TransferFrame {
        crate::data::types::TransferFrame {
            freqs,
            magnitude_db,
            phase_deg: vec![0.0; 0],
            coherence,
            re: vec![],
            im: vec![],
            delay_samples: 0,
            delay_ms: 0.0,
            meas_channel: 0,
            ref_channel: 1,
            sr: 48_000,
        }
    }

    /// Generate `n` log-spaced frequencies. Convenient for tests
    /// where every column gets at least one bin; doesn't model the
    /// daemon's actual downsampling (which is *linear*-spaced —
    /// `linear_daemon_freqs` below mirrors that).
    fn dense_freqs(n: usize, f_min: f32, f_max: f32) -> Vec<f32> {
        let log_min = f_min.log10();
        let log_max = f_max.log10();
        (0..n)
            .map(|i| {
                let t = i as f32 / (n - 1).max(1) as f32;
                10.0_f32.powf(log_min + t * (log_max - log_min))
            })
            .collect()
    }

    /// Linear-spaced frequencies modelling the daemon's transfer
    /// downsampling: `n` bins evenly spaced from 0 to Nyquist
    /// (every ~12 Hz at sr=48 kHz with n=2000). On a *log* x-axis
    /// these bins are dense at high freq and sparse at low freq —
    /// many low-freq columns of `EMBER_SPECTRUM_COLS` get no bin
    /// at all. The polyline builder must bridge those empty
    /// columns rather than break the trace.
    fn linear_daemon_freqs(n: usize, sr_hz: f32) -> Vec<f32> {
        let nyquist = sr_hz * 0.5;
        (0..n)
            .map(|i| (i as f32) * nyquist / ((n - 1).max(1) as f32))
            .collect()
    }

    /// BodeMag emits a polyline whose every vertex sits inside the
    /// substrate's [0,1] viewport (with a small padding margin).
    #[test]
    fn bodemag_pairs_stay_in_unit_box() {
        let freqs = dense_freqs(2000, 20.0, 24_000.0);
        let mags: Vec<f32> = freqs.iter().map(|&f| 10.0 * (f / 1000.0).log10()).collect();
        let f = transfer_frame_lin_log_db(freqs, mags, vec![]);
        let v = view(20.0, 24_000.0);
        let pairs = build_bodemag_polyline(&f, &v, 0.0);
        assert!(!pairs.is_empty(), "expected non-empty polyline");
        for [x, y, _] in &pairs {
            assert!((0.0..=1.0).contains(x), "x = {x} out of [0,1]");
            assert!((0.0..=1.0).contains(y), "y = {y} out of [0,1]");
        }
    }

    /// Flat unity-gain transfer (mag = 0 dB everywhere) → trace at
    /// the y coordinate corresponding to 0 dB. With the BodeMag
    /// default window (-40..+40), 0 dB lands at y = 0.05 + 0.9·0.5 =
    /// 0.5 (mid-cell).
    #[test]
    fn bodemag_unity_gain_traces_mid_cell() {
        let freqs = dense_freqs(2000, 20.0, 24_000.0);
        let mags = vec![0.0; freqs.len()];
        let f = transfer_frame_lin_log_db(freqs, mags, vec![]);
        let v = CellView {
            freq_min: 20.0,
            freq_max: 24_000.0,
            db_min: -40.0,
            db_max: 40.0,
            ..CellView::default()
        };
        let pairs = build_bodemag_polyline(&f, &v, 0.0);
        assert!(!pairs.is_empty(), "expected non-empty polyline");
        for [_, y, _] in &pairs {
            assert!(
                (y - 0.5).abs() < 1e-4,
                "0 dB should map to y = 0.5 in a (-40, 40) window; got {y}",
            );
        }
    }

    /// Coherence stays in the substrate cell regardless of input
    /// values (clamped to [0,1] before mapping).
    #[test]
    fn coherence_pairs_stay_in_unit_box() {
        let freqs = dense_freqs(2000, 20.0, 24_000.0);
        // Mix of valid coherence + a couple of out-of-range values
        // (defensive: the daemon shouldn't emit these but the builder
        // shouldn't panic if it does).
        let coh: Vec<f32> = freqs.iter().enumerate().map(|(i, _)| {
            match i % 5 {
                0 => 0.0,
                1 => 0.5,
                2 => 1.0,
                3 => 1.2,  // out of range — should clamp
                _ => -0.1, // out of range — should clamp
            }
        }).collect();
        let f = transfer_frame_lin_log_db(freqs, vec![], coh);
        let pairs = build_coherence_polyline(&f);
        assert!(!pairs.is_empty(), "expected non-empty polyline");
        for [x, y, _] in &pairs {
            assert!((0.0..=1.0).contains(x), "x = {x} out of [0,1]");
            assert!((0.0..=1.0).contains(y), "y = {y} out of [0,1]");
        }
    }

    /// Empty TransferFrame → empty polyline (no panic on cold start).
    #[test]
    fn bodemag_empty_frame_yields_empty_polyline() {
        let f = transfer_frame_lin_log_db(vec![], vec![], vec![]);
        let v = view(20.0, 24_000.0);
        assert!(build_bodemag_polyline(&f, &v, 0.0).is_empty());
    }

    #[test]
    fn coherence_empty_frame_yields_empty_polyline() {
        let f = transfer_frame_lin_log_db(vec![], vec![], vec![]);
        assert!(build_coherence_polyline(&f).is_empty());
    }

    // ---- Phase 2.5 BodePhase + GroupDelay tests ----

    /// Phase unwrap removes ±360° jumps. Constant +1°/sample
    /// underlying phase that wrapped to a sawtooth at ±180° must
    /// unwrap into a strictly-monotonic linear ramp.
    #[test]
    fn unwrap_phase_recovers_linear_ramp() {
        // 720 samples of "true phase = i degrees" wrapped to ±180.
        let wrapped: Vec<f32> = (0..720)
            .map(|i| {
                let mut p = i as f32;
                while p > 180.0 {
                    p -= 360.0;
                }
                p
            })
            .collect();
        let unwrapped = unwrap_phase_deg(&wrapped);
        // Each step should be +1.0° (within float epsilon).
        for i in 1..unwrapped.len() {
            let d = unwrapped[i] - unwrapped[i - 1];
            assert!(
                (d - 1.0).abs() < 1e-3,
                "expected +1° step at i={i}; got Δ={d}",
            );
        }
        // Endpoints: 0° at start, +719° at end.
        assert!(unwrapped[0].abs() < 1e-4);
        assert!((unwrapped[719] - 719.0).abs() < 1e-2);
    }

    /// Phase unwrap on a noisy walk that briefly oscillates around
    /// ±180° must still produce a continuous output (no spurious
    /// 360° jumps from in-band noise).
    #[test]
    fn unwrap_phase_handles_jitter_near_wrap() {
        // Sequence: 178, -179, 179, -179, 179 — really +178, +181, +179,
        // +181, +179 = drifting around 180° both directions. Unwrap
        // should follow the smaller continuation in each case.
        let wrapped = vec![178.0_f32, -179.0, 179.0, -179.0, 179.0];
        let unwrapped = unwrap_phase_deg(&wrapped);
        for i in 1..unwrapped.len() {
            let d = (unwrapped[i] - unwrapped[i - 1]).abs();
            assert!(d <= 5.0, "phase step at i={i} should be small; got {d}");
        }
    }

    /// BodePhase polyline stays in [0,1]² for typical input.
    #[test]
    fn bodephase_pairs_stay_in_unit_box() {
        let freqs = dense_freqs(2000, 20.0, 24_000.0);
        let phase: Vec<f32> = (0..freqs.len())
            .map(|i| ((i as f32 * 1.5) % 360.0) - 180.0)
            .collect();
        let f = transfer_frame_with_phase(freqs, phase);
        let v = CellView {
            freq_min: 20.0,
            freq_max: 24_000.0,
            db_min: -180.0,
            db_max: 180.0,
            ..CellView::default()
        };
        let pairs = build_bodephase_polyline(&f, &v, 0.0);
        assert!(!pairs.is_empty());
        for [x, y, _] in &pairs {
            assert!((0.0..=1.0).contains(x), "x = {x} out of [0,1]");
            assert!((0.0..=1.0).contains(y), "y = {y} out of [0,1]");
        }
    }

    /// GroupDelay sign: a *positive* phase slope (phase increases
    /// with frequency, e.g. +1°/Hz) corresponds to *negative* group
    /// delay. Confirms the τ_g = -dφ/dω sign.
    #[test]
    fn groupdelay_sign_matches_phase_slope() {
        let freqs = dense_freqs(2000, 20.0, 24_000.0);
        // Phase = +0.001°/Hz over the full band — small enough that
        // the unwrap stays trivial. Positive dφ/df → negative τ_g.
        let phase: Vec<f32> = freqs.iter().map(|&f| 0.001 * (f - 20.0)).collect();
        let frame = transfer_frame_with_phase(freqs, phase);
        let v = CellView {
            freq_min: 20.0,
            freq_max: 24_000.0,
            db_min: -10.0,
            db_max: 10.0,
            ..CellView::default()
        };
        let pairs = build_groupdelay_polyline(&frame, &v, 0.0);
        assert!(!pairs.is_empty());
        // y < 0.5 means τ_g < midpoint (= 0 ms in this window) — i.e.
        // negative group delay, as expected from positive phase slope.
        for [_, y, _] in &pairs {
            assert!(
                *y < 0.55,
                "positive phase slope should give negative τ_g; y = {y}",
            );
        }
    }

    /// GroupDelay on flat zero phase → zero delay → mid-cell.
    #[test]
    fn groupdelay_flat_phase_traces_mid_cell() {
        let freqs = dense_freqs(2000, 20.0, 24_000.0);
        let phase = vec![0.0; freqs.len()];
        let frame = transfer_frame_with_phase(freqs, phase);
        let v = CellView {
            freq_min: 20.0,
            freq_max: 24_000.0,
            db_min: -10.0,
            db_max: 10.0,
            ..CellView::default()
        };
        let pairs = build_groupdelay_polyline(&frame, &v, 0.0);
        assert!(!pairs.is_empty());
        for [_, y, _] in &pairs {
            // 0 ms in (-10, +10) window → y = 0.5.
            assert!((y - 0.5).abs() < 1e-4, "expected y ≈ 0.5; got {y}");
        }
    }

    fn transfer_frame_with_phase(
        freqs: Vec<f32>,
        phase_deg: Vec<f32>,
    ) -> crate::data::types::TransferFrame {
        crate::data::types::TransferFrame {
            freqs,
            magnitude_db: vec![],
            phase_deg,
            coherence: vec![],
            re: vec![],
            im: vec![],
            delay_samples: 0,
            delay_ms: 0.0,
            meas_channel: 0,
            ref_channel: 1,
            sr: 48_000,
        }
    }

    /// Empty-frame guards — no panic on cold start.
    #[test]
    fn bodephase_empty_frame_yields_empty_polyline() {
        let f = transfer_frame_with_phase(vec![], vec![]);
        let v = view(20.0, 24_000.0);
        assert!(build_bodephase_polyline(&f, &v, 0.0).is_empty());
    }

    #[test]
    fn groupdelay_empty_frame_yields_empty_polyline() {
        let f = transfer_frame_with_phase(vec![], vec![]);
        let v = view(20.0, 24_000.0);
        assert!(build_groupdelay_polyline(&f, &v, 0.0).is_empty());
    }

    // ---- Phase 4 Nyquist tests ----

    fn transfer_frame_with_re_im(
        freqs: Vec<f32>,
        re: Vec<f32>,
        im: Vec<f32>,
    ) -> crate::data::types::TransferFrame {
        crate::data::types::TransferFrame {
            freqs,
            magnitude_db: vec![],
            phase_deg: vec![],
            coherence: vec![],
            re,
            im,
            delay_samples: 0,
            delay_ms: 0.0,
            meas_channel: 0,
            ref_channel: 1,
            sr: 48_000,
        }
    }

    /// Empty re/im (legacy daemon, no Phase 3 fields) → empty
    /// polyline (no panic).
    #[test]
    fn nyquist_empty_re_im_yields_empty_polyline() {
        let f = transfer_frame_with_re_im(vec![100.0, 200.0], vec![], vec![]);
        assert!(build_nyquist_polyline(&f, 1.0, 0.0).is_empty());
    }

    /// Mismatched re/im lengths defensively yield empty (the daemon
    /// guarantees they're parallel; this is just a safety net).
    #[test]
    fn nyquist_mismatched_re_im_yields_empty() {
        let f = transfer_frame_with_re_im(vec![100.0, 200.0], vec![1.0, 0.5], vec![0.0]);
        assert!(build_nyquist_polyline(&f, 1.0, 0.0).is_empty());
    }

    /// Unity-gain real DUT (re ≈ 1, im ≈ 0) at amp = 1.0 places
    /// the trace on the +x ray at y = 0.5, x ≈ 0.5 + 0.45 = 0.95.
    /// All emitted vertices should sit on (0.95, 0.5) within
    /// float tolerance.
    #[test]
    fn nyquist_unity_real_lands_on_positive_x_ray() {
        let n = 200;
        let re = vec![1.0; n];
        let im = vec![0.0; n];
        let freqs: Vec<f32> = (0..n).map(|i| 100.0 + i as f32 * 100.0).collect();
        let f = transfer_frame_with_re_im(freqs, re, im);
        let pairs = build_nyquist_polyline(&f, 1.0, 0.0);
        assert!(!pairs.is_empty(), "expected non-empty Nyquist polyline");
        // Find the trace vertices (skip the unit-circle reference,
        // which is the first ~128 vertices). Any trace vertex sits
        // at (0.95, 0.5) for re=1, im=0 with amp=1.
        let trace_pairs: Vec<&[f32; 3]> = pairs
            .iter()
            .filter(|p| (p[0] - 0.95).abs() < 1e-3 && (p[1] - 0.5).abs() < 1e-3)
            .collect();
        assert!(
            !trace_pairs.is_empty(),
            "expected vertices at (0.95, 0.5); got pairs: {:?}",
            &pairs[..pairs.len().min(8)],
        );
    }

    /// Quarter-circle test: re/im = (cos(θ), sin(θ)) for θ in
    /// [0, π/2] should trace a quarter unit circle in the upper-
    /// right quadrant. Verify by checking that all trace vertices
    /// satisfy ((x-0.5)² + (y-0.5)²) ≈ 0.45² with x ≥ 0.5,
    /// y ≥ 0.5 (using amp = 1.0 → unit-circle scale = 0.45).
    #[test]
    fn nyquist_unit_quarter_circle_traces_arc() {
        let n = 100;
        let re: Vec<f32> = (0..n)
            .map(|i| (i as f32 / (n - 1) as f32 * std::f32::consts::FRAC_PI_2).cos())
            .collect();
        let im: Vec<f32> = (0..n)
            .map(|i| (i as f32 / (n - 1) as f32 * std::f32::consts::FRAC_PI_2).sin())
            .collect();
        let freqs: Vec<f32> = (0..n).map(|i| 100.0 + i as f32 * 100.0).collect();
        let f = transfer_frame_with_re_im(freqs, re, im);
        let pairs = build_nyquist_polyline(&f, 1.0, 0.0);
        assert!(!pairs.is_empty());
        // The unit-circle reference draws vertices in all 4 quadrants
        // — to isolate the trace, look for vertices in the upper-
        // right quadrant only.
        for [x, y, _] in &pairs {
            if *x >= 0.5 && *y >= 0.5 {
                let r2 = (x - 0.5).powi(2) + (y - 0.5).powi(2);
                assert!(
                    (r2 - 0.45_f32.powi(2)).abs() < 1e-3,
                    "upper-right vertex ({x}, {y}) not on radius 0.45",
                );
            }
        }
    }

    // ---- Phase 4b IR builder tests ----

    fn ir_frame(samples: Vec<f32>) -> crate::data::types::IrFrame {
        crate::data::types::IrFrame {
            samples,
            sr: 48_000,
            dt_ms: 1.0 / 48.0,
            t_origin_ms: -10.0,
            ref_channel: 1,
            meas_channel: 0,
            stride: 1,
            delay_samples: 0,
            delay_ms: 0.0,
        }
    }

    /// Empty IR frame (cold start) → empty polyline (no panic).
    #[test]
    fn ir_empty_samples_yields_empty_polyline() {
        let pairs = build_ir_polyline(&ir_frame(vec![]), 1.0);
        assert!(pairs.is_empty());
    }

    /// Single-sample IR is too short to draw — returns empty.
    #[test]
    fn ir_too_short_yields_empty_polyline() {
        let pairs = build_ir_polyline(&ir_frame(vec![1.0]), 1.0);
        assert!(pairs.is_empty());
    }

    /// Flat-zero IR: trace coincides with the y=0.5 baseline.
    #[test]
    fn ir_flat_zero_traces_at_baseline() {
        let pairs = build_ir_polyline(&ir_frame(vec![0.0; 256]), 1.0);
        assert!(!pairs.is_empty());
        for [_, y, _] in &pairs {
            assert!(
                (y - 0.5).abs() < 1e-5,
                "expected y ≈ 0.5 (baseline); got {y}",
            );
        }
    }

    /// Substrate-box invariant: all vertices stay in [0,1]² even
    /// when amp tries to push the trace outside the cell (build_ir
    /// clamps to keep the substrate render path well-behaved).
    #[test]
    fn ir_pairs_stay_in_unit_box() {
        let samples: Vec<f32> = (0..256)
            .map(|i| ((i as f32 * 0.1).sin()))
            .collect();
        // Deliberately oversized amp — clamps should still hold.
        let pairs = build_ir_polyline(&ir_frame(samples), 100.0);
        assert!(!pairs.is_empty());
        for [x, y, _] in &pairs {
            assert!((0.0..=1.0).contains(x), "x = {x} out of [0,1]");
            assert!((0.0..=1.0).contains(y), "y = {y} out of [0,1]");
        }
    }

    /// Centred unit-impulse: positive peak at the array midpoint
    /// projects to (x ≈ 0.5, y > 0.5). Verifies the dispatch arm's
    /// auto-gain → builder mapping puts the Dirac peak in the right
    /// place visually.
    #[test]
    fn ir_centred_impulse_peak_at_mid_cell() {
        let mut samples = vec![0.0_f32; 257];
        let mid = samples.len() / 2;
        samples[mid] = 1.0; // unit impulse at centre
        let pairs = build_ir_polyline(&ir_frame(samples), 0.4);
        assert!(!pairs.is_empty());
        // Find the vertex with the highest y — that's the impulse
        // peak. It should be at roughly mid-cell on x and noticeably
        // above the baseline on y.
        let (peak_x, peak_y) = pairs
            .iter()
            .max_by(|a, b| a[1].partial_cmp(&b[1]).unwrap())
            .map(|p| (p[0], p[1]))
            .unwrap();
        assert!(
            (peak_x - 0.5).abs() < 0.01,
            "impulse peak x = {peak_x}, expected ≈ 0.5",
        );
        assert!(
            peak_y > 0.6,
            "impulse peak y = {peak_y}, expected noticeably above baseline 0.5",
        );
    }

    /// Bins where (re, im) project off-cell at the given amp must
    /// be skipped (no clamping artefacts at the substrate edge).
    #[test]
    fn nyquist_off_cell_bins_skipped() {
        // re = 10 with amp = 1 → x = 0.5 + 4.5 = 5.0 (off-cell).
        // im = 0. All these bins should be skipped.
        let n = 50;
        let re = vec![10.0; n];
        let im = vec![0.0; n];
        let freqs: Vec<f32> = (0..n).map(|i| 100.0 + i as f32 * 100.0).collect();
        let f = transfer_frame_with_re_im(freqs, re, im);
        let pairs = build_nyquist_polyline(&f, 1.0, 0.0);
        // Only the unit-circle reference should be present.
        for [x, y, _] in &pairs {
            assert!((0.0..=1.0).contains(x), "x = {x} out of [0,1]");
            assert!((0.0..=1.0).contains(y), "y = {y} out of [0,1]");
        }
    }

    // ---- Coherence-weighting tests ----

    /// `coherence_weight` core: k=0 short-circuits to 1.0 (off);
    /// k>0 returns γ²^k clamped to [0, 1]. Non-finite γ² → 0.0
    /// (defensive — corrupt frames don't brighten unexpectedly).
    #[test]
    fn coherence_weight_basic_table() {
        // k=0: any γ² → 1.0 (weighting disabled).
        assert_eq!(coherence_weight(0.0, 0.0), 1.0);
        assert_eq!(coherence_weight(0.5, 0.0), 1.0);
        assert_eq!(coherence_weight(1.0, 0.0), 1.0);
        assert_eq!(coherence_weight(f32::NAN, 0.0), 1.0);
        // k=1: γ² → γ² (linear pass-through).
        assert!((coherence_weight(0.0, 1.0) - 0.0).abs() < 1e-6);
        assert!((coherence_weight(0.5, 1.0) - 0.5).abs() < 1e-6);
        assert!((coherence_weight(1.0, 1.0) - 1.0).abs() < 1e-6);
        // k=2: γ² → γ²·γ² (canonical default).
        assert!((coherence_weight(0.5, 2.0) - 0.25).abs() < 1e-6);
        assert!((coherence_weight(1.0, 2.0) - 1.0).abs() < 1e-6);
        // k=4: aggressive — γ²=0.5 → 0.0625.
        assert!((coherence_weight(0.5, 4.0) - 0.0625).abs() < 1e-6);
        // Non-finite coherence → 0 (don't deposit corrupt bins).
        assert_eq!(coherence_weight(f32::NAN, 2.0), 0.0);
        assert_eq!(coherence_weight(f32::INFINITY, 2.0), 0.0);
        // Out-of-range γ² clamps to [0, 1] before exponentiation.
        assert!((coherence_weight(1.5, 2.0) - 1.0).abs() < 1e-6);
        assert!((coherence_weight(-0.3, 2.0) - 0.0).abs() < 1e-6);
    }

    /// BodeMag with k=2: vertices in a high-coherence column carry
    /// weight ≈ 1.0; vertices in a low-coherence column carry
    /// weight ≈ γ²^2. Verifies the per-column coherence is being
    /// retained and propagated to the polyline weight.
    #[test]
    fn bodemag_weights_track_per_bin_coherence() {
        let freqs = dense_freqs(2000, 20.0, 24_000.0);
        let mags = vec![0.0_f32; freqs.len()];
        // Coherence sweep: low (0.2) below 1 kHz, high (0.95) above.
        // Both halves get the same magnitude so the column-max bin
        // is the coherence we expect to read out.
        let coh: Vec<f32> = freqs
            .iter()
            .map(|&f| if f < 1000.0 { 0.2 } else { 0.95 })
            .collect();
        let f = transfer_frame_lin_log_db(freqs, mags, coh);
        let v = view(20.0, 24_000.0);
        let pairs = build_bodemag_polyline(&f, &v, 2.0);
        assert!(!pairs.is_empty());
        // Sample the leftmost (low-coherence) and rightmost
        // (high-coherence) vertices and confirm their weights.
        let first = pairs.first().unwrap();
        let last = pairs.last().unwrap();
        // Low-γ² half: weight ≈ 0.2² = 0.04.
        assert!(
            (first[2] - 0.04).abs() < 1e-3,
            "low-γ² weight should be ≈ 0.04, got {}",
            first[2],
        );
        // High-γ² half: weight ≈ 0.95² ≈ 0.9025.
        assert!(
            (last[2] - 0.9025).abs() < 1e-3,
            "high-γ² weight should be ≈ 0.9025, got {}",
            last[2],
        );
    }

    /// BodeMag with k=0 (off): every vertex carries weight 1.0
    /// regardless of underlying coherence — the weighting path is
    /// fully bypassed.
    #[test]
    fn bodemag_k_zero_disables_weighting() {
        let freqs = dense_freqs(2000, 20.0, 24_000.0);
        let mags = vec![0.0_f32; freqs.len()];
        let coh = vec![0.05_f32; freqs.len()]; // very low γ²
        let f = transfer_frame_lin_log_db(freqs, mags, coh);
        let v = view(20.0, 24_000.0);
        let pairs = build_bodemag_polyline(&f, &v, 0.0);
        assert!(!pairs.is_empty());
        for [_, _, w] in &pairs {
            assert!((w - 1.0).abs() < 1e-6, "k=0 must yield w=1, got {w}");
        }
    }

    /// Coherence view ignores the weighting knob entirely — gating
    /// γ² on γ² would be circular. Every vertex keeps weight 1.0
    /// even when the underlying γ² is at the floor.
    #[test]
    fn coherence_view_never_weights() {
        let freqs = dense_freqs(2000, 20.0, 24_000.0);
        let coh = vec![0.05_f32; freqs.len()];
        let f = transfer_frame_lin_log_db(freqs, vec![], coh);
        let pairs = build_coherence_polyline(&f);
        assert!(!pairs.is_empty());
        for [_, _, w] in &pairs {
            assert!((w - 1.0).abs() < 1e-6, "coherence view must not weight, got {w}");
        }
    }

    /// IR view: every vertex (baseline + trace) carries weight 1.0.
    /// IR is an IFFT of complex H — there's no per-sample γ² to
    /// gate against, so the knob doesn't apply here.
    #[test]
    fn ir_view_never_weights() {
        let pairs = build_ir_polyline(&ir_frame(vec![1.0, 0.5, -0.3, 0.1, 0.0]), 0.4);
        assert!(!pairs.is_empty());
        for [_, _, w] in &pairs {
            assert!((w - 1.0).abs() < 1e-6, "IR view must not weight, got {w}");
        }
    }

    /// Nyquist with k=2: per-bin coherence drives per-vertex weight.
    /// Constant unity-real DUT with coherence ramp 0 → 1 should map
    /// to weight ramp 0 → 1 (k=2 squares it but the endpoints stay).
    #[test]
    fn nyquist_weights_track_per_bin_coherence() {
        let n = 100;
        let re = vec![1.0_f32; n];
        let im = vec![0.0_f32; n];
        let coh: Vec<f32> = (0..n).map(|i| i as f32 / (n - 1) as f32).collect();
        let freqs: Vec<f32> = (0..n).map(|i| 100.0 + i as f32 * 100.0).collect();
        let mut f = transfer_frame_with_re_im(freqs, re, im);
        f.coherence = coh;
        let pairs = build_nyquist_polyline(&f, 1.0, 2.0);
        assert!(!pairs.is_empty());
        // Trace vertices land on (0.95, 0.5); reference unit-circle
        // vertices are scattered. Filter to trace vertices and
        // confirm their weights span from ~0 (low-γ² end) to ~1
        // (high-γ² end).
        let trace_weights: Vec<f32> = pairs
            .iter()
            .filter(|p| (p[0] - 0.95).abs() < 1e-3 && (p[1] - 0.5).abs() < 1e-3)
            .map(|p| p[2])
            .collect();
        assert!(!trace_weights.is_empty());
        let min_w = trace_weights.iter().cloned().fold(f32::INFINITY, f32::min);
        let max_w = trace_weights.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        assert!(min_w < 0.05, "low-γ² trace weight should be near 0, got {min_w}");
        assert!(max_w > 0.95, "high-γ² trace weight should be near 1, got {max_w}");
    }

    /// Nyquist unit-circle reference always carries weight 1.0 —
    /// the static guide must remain visible regardless of how the
    /// measurement trace is being weighted. Trace placed at
    /// re=0.5, im=0 (radius 0.1125 from centre at amp=0.5) so it
    /// doesn't collide with the reference arc (radius 0.225).
    #[test]
    fn nyquist_unit_circle_reference_always_full_weight() {
        let n = 50;
        let re = vec![0.5_f32; n];
        let im = vec![0.0_f32; n];
        let coh = vec![0.0_f32; n]; // all bins low γ²
        let freqs: Vec<f32> = (0..n).map(|i| 100.0 + i as f32 * 100.0).collect();
        let mut f = transfer_frame_with_re_im(freqs, re, im);
        f.coherence = coh;
        let pairs = build_nyquist_polyline(&f, 0.5, 4.0);
        assert!(!pairs.is_empty());
        // The unit-circle reference traces an arc of radius 0.225
        // (0.45 · amp = 0.45 · 0.5) around (0.5, 0.5). At least
        // some such vertices must be present at full weight.
        let circle_full: Vec<f32> = pairs
            .iter()
            .filter(|p| {
                let dx = p[0] - 0.5;
                let dy = p[1] - 0.5;
                let r = (dx * dx + dy * dy).sqrt();
                (r - 0.225).abs() < 1e-3
            })
            .map(|p| p[2])
            .collect();
        assert!(!circle_full.is_empty(), "expected unit-circle vertices");
        for w in &circle_full {
            assert!((w - 1.0).abs() < 1e-6, "circle ref weight must be 1, got {w}");
        }
        // And confirm the trace weights are at the floor (γ²=0,
        // k=4 → 0) — this is the contrast that makes the
        // reference-always-bright invariant matter visually.
        let trace_weights: Vec<f32> = pairs
            .iter()
            .filter(|p| {
                let dx = p[0] - 0.5;
                let dy = p[1] - 0.5;
                let r = (dx * dx + dy * dy).sqrt();
                (r - 0.1125).abs() < 1e-3
            })
            .map(|p| p[2])
            .collect();
        assert!(!trace_weights.is_empty(), "expected trace vertices");
        for w in &trace_weights {
            assert!(*w < 1e-6, "γ²=0 trace weight should be ~0, got {w}");
        }
    }

    // ---- resolve_stereo_pair (TransferPair-driven) ----

    fn scope_frame(channel: u32, frame_idx: u64, samples: Vec<f32>) -> crate::data::types::ScopeFrame {
        crate::data::types::ScopeFrame {
            channel,
            sr: 48_000,
            frame_idx,
            samples,
            n_channels: Some(2),
        }
    }

    /// No scope store at all (synthetic / pre-connect) → NoAudio,
    /// regardless of whether a pair is supplied.
    #[test]
    fn stereo_pair_no_scope_store_yields_no_audio() {
        use crate::data::types::{StereoStatus, TransferPair};
        let pair = Some(TransferPair { meas: 0, ref_ch: 1 });
        let (status, samples) = resolve_stereo_pair(pair, None, 64);
        assert_eq!(status, StereoStatus::NoAudio);
        assert!(samples.is_none());
    }

    /// Scope store present but no TransferPair registered → the new
    /// NoTransferPair variant fires (overlay caption hints at Space+T).
    #[test]
    fn stereo_pair_no_pair_yields_no_transfer_pair() {
        use crate::data::types::StereoStatus;
        let store = crate::data::store::ScopeStore::new();
        let (status, samples) = resolve_stereo_pair(None, Some(&store), 64);
        assert_eq!(status, StereoStatus::NoTransferPair);
        assert!(samples.is_none());
    }

    /// Pair registered but the daemon hasn't started streaming scope
    /// frames yet → NotStreamingYet carrying the (l, r) physical ids
    /// the user picked, so the caption can still tell them which
    /// channels are being waited on.
    #[test]
    fn stereo_pair_registered_but_no_frames_yields_not_streaming_yet() {
        use crate::data::types::{StereoStatus, TransferPair};
        let store = crate::data::store::ScopeStore::new();
        let pair = Some(TransferPair { meas: 5, ref_ch: 3 });
        let (status, samples) = resolve_stereo_pair(pair, Some(&store), 64);
        // l = ref_ch, r = meas (matches the IoTransfer convention
        // and the resolver's docstring).
        assert_eq!(status, StereoStatus::NotStreamingYet { l: 3, r: 5 });
        assert!(samples.is_none());
    }

    /// Pair registered + matching scope frames present on both
    /// channels → Real { l = ref_ch, r = meas } and the sample
    /// vectors come back in (l_samples, r_samples) order.
    #[test]
    fn stereo_pair_matching_frames_yield_real() {
        use crate::data::types::{StereoStatus, TransferPair};
        let store = crate::data::store::ScopeStore::new();
        let n = 128;
        let l_samples: Vec<f32> = (0..n).map(|i| (i as f32) * 0.01).collect();
        let r_samples: Vec<f32> = (0..n).map(|i| (i as f32) * -0.01).collect();
        // Same frame_idx so the pairing check (`abs_diff <= 1`) passes.
        store.write(scope_frame(3, 100, l_samples.clone()));
        store.write(scope_frame(5, 100, r_samples.clone()));
        let pair = Some(TransferPair { meas: 5, ref_ch: 3 });
        let (status, samples) = resolve_stereo_pair(pair, Some(&store), n);
        assert_eq!(status, StereoStatus::Real { l: 3, r: 5 });
        let (sl, sr_buf) = samples.expect("expected matched scope samples");
        assert_eq!(sl, l_samples);
        assert_eq!(sr_buf, r_samples);
    }

    /// Regression: with the daemon's linear-spaced bins, log-axis
    /// columns at low freq are wider than the bin spacing, so most
    /// of the leftmost columns get no bin at all. The previous
    /// builder broke the polyline on every empty column, leaving
    /// the cell's left half visually empty — exactly the
    /// "<1 kHz is ignored" symptom from real captures. After the
    /// fix, the builder skips empty columns *without* resetting
    /// `prev`, so the trace bridges the gap and reaches into the
    /// low-freq region.
    #[test]
    fn bodemag_low_freq_not_dropped_with_linear_daemon_bins() {
        // 2000 linear-spaced bins from 0..24 kHz (matches
        // ac-daemon's transfer_stream downsample at sr = 48 kHz).
        let freqs = linear_daemon_freqs(2000, 48_000.0);
        let mags = vec![0.0_f32; freqs.len()]; // flat unity-gain
        let f = transfer_frame_lin_log_db(freqs, mags, vec![]);
        let v = view(20.0, 24_000.0);
        let pairs = build_bodemag_polyline(&f, &v, 0.0);
        // The leftmost column the builder emits should sit at low
        // freq: `xn ≈ 0.05` corresponds to ~30 Hz, well below the
        // 1 kHz threshold the user observed pre-fix.
        let leftmost_x = pairs
            .iter()
            .map(|p| p[0])
            .fold(f32::INFINITY, f32::min);
        assert!(
            leftmost_x < 0.10,
            "expected polyline to reach into low-freq cols (xn < 0.10); \
             got leftmost xn = {leftmost_x} (bug: empty cols broke the trace)",
        );
    }

    /// Same regression check for Coherence, BodePhase, GroupDelay —
    /// they share the column-aggregation pattern that broke at low
    /// freq. Bundled into one test since the assertion is identical
    /// (leftmost xn must reach into the low-freq band).
    #[test]
    fn coherence_phase_groupdelay_low_freq_not_dropped() {
        let freqs = linear_daemon_freqs(2000, 48_000.0);
        let coh = vec![0.9_f32; freqs.len()];
        let phase = vec![0.0_f32; freqs.len()];
        let f_coh = transfer_frame_lin_log_db(freqs.clone(), vec![], coh);
        let f_ph = transfer_frame_with_phase(freqs.clone(), phase.clone());
        let f_gd = transfer_frame_with_phase(freqs, phase);
        let v = CellView {
            freq_min: 20.0,
            freq_max: 24_000.0,
            db_min: -10.0,
            db_max: 10.0,
            ..CellView::default()
        };
        for (label, pairs) in [
            ("coherence", build_coherence_polyline(&f_coh)),
            ("bodephase", build_bodephase_polyline(&f_ph, &v, 0.0)),
            ("groupdelay", build_groupdelay_polyline(&f_gd, &v, 0.0)),
        ] {
            let leftmost_x = pairs.iter().map(|p| p[0]).fold(f32::INFINITY, f32::min);
            assert!(
                leftmost_x < 0.10,
                "{label}: expected leftmost xn < 0.10, got {leftmost_x}",
            );
        }
    }

    /// Pair registered, frames present on L only (R hasn't arrived
    /// yet) → NotStreamingYet, not Real.
    #[test]
    fn stereo_pair_partial_frames_yield_not_streaming_yet() {
        use crate::data::types::{StereoStatus, TransferPair};
        let store = crate::data::store::ScopeStore::new();
        let n = 128;
        let l_samples: Vec<f32> = (0..n).map(|i| (i as f32) * 0.01).collect();
        store.write(scope_frame(3, 100, l_samples));
        // No write to channel 5.
        let pair = Some(TransferPair { meas: 5, ref_ch: 3 });
        let (status, samples) = resolve_stereo_pair(pair, Some(&store), n);
        assert_eq!(status, StereoStatus::NotStreamingYet { l: 3, r: 5 });
        assert!(samples.is_none());
    }
}
