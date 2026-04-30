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
        // at: fake-audio daemon is typically 48 kHz → 24 kHz, but a 96 kHz
        // session will hand us freqs up to ~48 kHz and the clamp must follow.
        // Daemon owns the aggregation and publishes bins spanning f_min..f_max
        // (20 Hz .. sr/2). The GPU shader maps bin index linearly across the
        // viewport, so the on-screen axis is correct only if view.freq_min /
        // freq_max match the data range. Lock both to the data range — pan/zoom
        // on the freq axis was explicitly traded away for this.
        let mut data_max_seen = self.data_freq_ceiling;
        let mut data_min_seen = theme::DEFAULT_FREQ_MIN;
        for slot in frames.iter().flatten() {
            if let Some(&last) = slot.freqs.last() {
                if last.is_finite() && last > data_max_seen {
                    data_max_seen = last;
                }
            }
            if let Some(&first) = slot.freqs.first() {
                if first.is_finite() && first > 0.0 {
                    data_min_seen = first;
                }
            }
        }
        if data_max_seen > self.data_freq_ceiling {
            self.data_freq_ceiling = data_max_seen;
        }
        for cv in self.cell_views.iter_mut() {
            cv.freq_min = data_min_seen;
            cv.freq_max = self.data_freq_ceiling;
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
                ViewMode::Scope | ViewMode::SpectrumEmber => {}
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
                        new_row: frame.new_row.as_deref().map(|v| v.as_slice()),
                    });
                }
                ViewMode::Scope | ViewMode::SpectrumEmber => {
                    // Ember-substrate views consume polylines built later in
                    // this method (synthetic sine for Scope, the active
                    // channel's spectrum frame for SpectrumEmber). The cell
                    // iteration is kept so Single-layout viewport math runs.
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
            ViewMode::Scope | ViewMode::SpectrumEmber => {}
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
        let active_palette_snap = waterfall.active_palette();
        let smoothing_snap = self.smoothing_frac;
        let ioct_bpo_snap = self.ioct_bpo;
        let band_weighting_snap = self.band_weighting.overlay_tag();
        // Pull the loudness readout for the currently active channel.
        // Hover-targeted focus is a future refinement — the active
        // channel is the one the UI is already centred on.
        let loudness_focus_ch = self.config.active_channel;
        let loudness_snap: Option<crate::data::types::LoudnessReadout> = self
            .loudness_store
            .as_ref()
            .filter(|_| loudness_focus_ch < n_real)
            .and_then(|store| store.read(loudness_focus_ch as u32));
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
        let box_zoom_snap = self.box_zoom.as_ref().map(|b| {
            (
                egui::pos2(b.start.x as f32, b.start.y as f32),
                egui::pos2(b.current.x as f32, b.current.y as f32),
            )
        });
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
            if let Some((start, current)) = box_zoom_snap {
                let painter = ui_ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    egui::Id::new("ac-ui-box-zoom"),
                ));
                let rect = egui::Rect::from_two_pos(start, current);
                // Translucent fill + crisp stroke so the selected region
                // reads as a highlight rather than a cursor artifact.
                painter.rect_filled(
                    rect,
                    egui::CornerRadius::ZERO,
                    egui::Color32::from_rgba_unmultiplied(80, 160, 255, 32),
                );
                painter.rect_stroke(
                    rect,
                    egui::CornerRadius::ZERO,
                    egui::Stroke::new(1.5, egui::Color32::from_rgb(140, 200, 255)),
                    egui::StrokeKind::Outside,
                );
            }
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
        if matches!(view_mode, ViewMode::Scope | ViewMode::SpectrumEmber) {
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
                        .unwrap_or(48_000) as f32;
                    let (polyline, scroll_dx) = build_scope_polyline(
                        &mut self.ember_sine_phase,
                        sr,
                        EMBER_SCOPE_SINE_HZ,
                        self.ember_scope_window_s,
                        self.ember_scope_y_gain,
                        dt,
                    );
                    ember.set_tau_p(0.6);
                    ember.set_intensity(0.002);
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
                    let polyline = self
                        .last_frames
                        .get(active)
                        .and_then(|f| f.as_ref())
                        .filter(|f| !f.spectrum.is_empty())
                        .map(|f| build_spectrum_polyline(f, &view))
                        .unwrap_or_default();
                    // 0.06 saturated everything — the polyline rasterizer
                    // hits ~1000 pixels/frame, intensity multiplies, and
                    // with tau=5 s the trace pinned at L>>1 → LUT clamped
                    // to white. Drop 20× and shorten tau to 2.5 s so a
                    // moved peak's old position fades visibly while the
                    // new one paints in.
                    ember.set_tau_p(2.5);
                    ember.set_intensity(0.003);
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
                ViewMode::Scope | ViewMode::SpectrumEmber => ember.draw(&mut pass),
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

/// Synthetic 1 kHz sine sampled at 48 kHz, mapped to the ember substrate as
/// a strip-chart polyline: new samples occupy the rightmost band of width
/// `dt / window_s`, oldest on the left of the band, newest at x = 1.0. The
/// returned `scroll_dx` matches that band width and is what the caller
/// passes to `EmberRenderer::advance` to keep the pre-existing substrate
/// shifted in lockstep.
fn build_scope_polyline(
    sine_phase:   &mut f32,
    sample_rate:  f32,
    sine_freq_hz: f32,
    window_s:     f32,
    y_gain:       f32,
    dt:           f32,
) -> (Vec<[f32; 2]>, f32) {
    let scroll_dx = (dt / window_s.max(1e-3)).clamp(0.0, 1.0);
    let n = ((dt * sample_rate) as usize).min(8000);
    let mut pts = Vec::with_capacity(n);
    let two_pi = std::f32::consts::TAU;
    let phase_step = two_pi * sine_freq_hz / sample_rate;
    let denom = (n.saturating_sub(1)).max(1) as f32;
    let amp = y_gain.clamp(0.01, 0.5);
    for i in 0..n {
        let s = sine_phase.sin();
        *sine_phase = (*sine_phase + phase_step) % two_pi;
        let frac = i as f32 / denom;
        let x = (1.0 - scroll_dx) + frac * scroll_dx;
        let y = 0.5 + amp * s;
        pts.push([x, y]);
    }
    (pts, scroll_dx)
}

/// `SpectrumFrame` → ember polyline. Logarithmic frequency axis matching
/// the cell's view window; dB axis linear in the cell's `db_min..db_max`.
/// Produces one vertex per visible bin (those whose freq falls inside the
/// window); fewer than 2 → empty polyline → ember just decays this frame.
fn build_spectrum_polyline(
    frame: &crate::data::types::DisplayFrame,
    view: &CellView,
) -> Vec<[f32; 2]> {
    let log_min = view.freq_min.max(1.0).log10();
    let log_max = view.freq_max.max(view.freq_min * 1.001).log10();
    let span_f = (log_max - log_min).max(1e-6);
    let span_db = (view.db_max - view.db_min).max(1e-3);
    let n = frame.freqs.len().min(frame.spectrum.len());
    let mut pts = Vec::with_capacity(n);
    for i in 0..n {
        let f = frame.freqs[i];
        if !(f.is_finite() && f >= view.freq_min && f <= view.freq_max) {
            continue;
        }
        let mag = frame.spectrum[i];
        if !mag.is_finite() {
            continue;
        }
        let x = ((f.max(1.0).log10() - log_min) / span_f).clamp(0.0, 1.0);
        let y = ((mag - view.db_min) / span_db).clamp(0.0, 1.0);
        pts.push([x, y]);
    }
    pts
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
}
