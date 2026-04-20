//! Frame-render pipeline — `App::redraw` and its helpers. Runs every vsync
//! (when the compositor repaints) or on demand (winit `RedrawRequested`).
//! Reads from data stores, uploads to GPU, composites egui on top, and
//! submits the frame. All overlays (peak-hold, tuner, monitor stats) are
//! painted inside the single egui pass driven from `redraw`.

use std::sync::Arc;
use std::time::Instant;

use egui::Color32;

use crate::data::smoothing;
use crate::data::types::{
    CellView, DisplayFrame, FrameMeta, LayoutMode, SweepKind, TransferFrame, TunerMode, ViewMode,
};
use crate::render::context::RenderContext;
use crate::render::grid;
use crate::render::spectrum::{ChannelMeta, ChannelUpload};
use crate::render::waterfall::CellUpload as WaterfallCellUpload;
use crate::theme;
use crate::ui::export::{self, ScreenshotRequest};
use crate::ui::layout;
use crate::ui::overlay::{self, HoverInfo, HoverReadout, MonitorParamsInfo, OverlayInput};
use crate::ui::stats::StatsSnapshot;

use super::helpers::{
    NOTIFICATION_TTL, PEAK_HOLD_DECAY, PEAK_RELEASE_DB_PER_SEC, TUNER_MIN_CONFIDENCE,
    TunerSensitivity, WATERFALL_ROW_DT_HYSTERESIS, WATERFALL_ROW_DT_MIN, WATERFALL_ROW_DT_WINDOW,
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
                        sr:               tf.sr,
                        clipping:         false,
                        xruns:            0,
                    },
                    new_row: if is_fresh { Some(spectrum) } else { None },
                }
            });
            frames.push(frame);
        }

        // Fractional-octave smoothing. Runs before peak-hold so the held max
        // is taken over the smoothed trace the user is actually looking at;
        // it also keeps the frame-level `spectrum` consistent with what the
        // overlay reads for hover labels. Window indices are cached per
        // `(n_frac, n_bins, last_freq)` to avoid a log-range recompute per
        // frame.
        if let Some(n_frac) = self.smoothing_frac {
            for frame in frames.iter_mut().flatten() {
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
        // buffer instead of panicking or silently clipping.
        if self.peak_hold_enabled {
            let now = Instant::now();
            for (i, slot) in frames.iter().enumerate().take(n_real) {
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

        // Drum tuner candidates are pushed by the daemon on the `tuner`
        // PUB topic and cached in `tuner_store`. Off mode drops the cache so
        // a stale pre-Off readout doesn't reappear on re-enter.
        if self.tuner_mode == TunerMode::Off {
            if let Some(s) = self.tuner_store.as_ref() {
                s.clear();
            }
            self.tuner_range_lock = None;
        }
        if self.tuner_locks.len() < n_real {
            self.tuner_locks.resize(n_real, None);
        }

        // Min-hold accumulator: mirror of the peak loop with the comparator
        // flipped. Same decay rule so a brief gap in the signal doesn't pin
        // the trace down forever at whatever accidental silence the buffer
        // captured.
        if self.min_hold_enabled {
            let now = Instant::now();
            for (i, slot) in frames.iter().enumerate().take(n_real) {
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

        let n_channels = frames.len();
        let cells = layout::compute(
            self.config.layout,
            n_channels,
            self.config.active_channel,
            &self.selected,
            &self.selection_order,
            self.active_meas_idx,
            grid_params_snap,
        );
        let in_transfer_layout = matches!(self.config.layout, LayoutMode::Transfer);
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
                let peak = frame
                    .spectrum
                    .iter()
                    .copied()
                    .filter(|v| v.is_finite())
                    .fold(f32::NEG_INFINITY, f32::max);
                let (db_min, db_max) = if peak.is_finite() {
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
        if !in_transfer_layout && !in_sweep_layout {
            match view_mode {
                ViewMode::Spectrum => spectrum_uploads.reserve(cells.len()),
                ViewMode::Waterfall => waterfall_uploads.reserve(cells.len()),
            }
        }

        for cell in &cells {
            if in_transfer_layout || in_sweep_layout {
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
                                // Brighten so peak sits visibly above the live
                                // trace in the same hue. Clamp to 1.0 so
                                // already-bright hues (pale gold) don't wrap.
                                for c in peak_color.iter_mut().take(3) {
                                    *c = (*c * 1.35).min(1.0);
                                }
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
            }
        }

        match view_mode {
            ViewMode::Spectrum => {
                spectrum.upload(&ctx.device, &ctx.queue, &spectrum_uploads);
            }
            ViewMode::Waterfall => {
                waterfall.upload(&ctx.device, &ctx.queue, n_channels, &waterfall_uploads);
            }
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
        let peak_holds_snap = if self.peak_hold_enabled {
            self.peak_holds.clone()
        } else {
            Vec::new()
        };
        let tuner_mode_snap = self.tuner_mode;
        let (tuner_last_snap, tuner_history_snap): (
            Vec<Option<ac_core::tuner::FundamentalCandidate>>,
            Vec<Vec<f64>>,
        ) = if self.tuner_mode != TunerMode::Off {
            self.tuner_store
                .as_ref()
                .map(|s| s.snapshot(n_real))
                .unwrap_or_else(|| (Vec::new(), Vec::new()))
        } else {
            (Vec::new(), Vec::new())
        };
        let tuner_locks_snap = if self.tuner_mode == TunerMode::Locked {
            self.tuner_locks.clone()
        } else {
            Vec::new()
        };
        let tuner_range_lock_snap = if self.tuner_mode != TunerMode::Off {
            self.tuner_range_lock
        } else {
            None
        };
        let tuner_sens_snap = self.tuner_sensitivity;
        let tuner_min_level_snap = self.tuner_min_level_dbfs;
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
        let mut transfer_snap: Option<TransferFrame> = if in_transfer_layout {
            self.transfer_last.clone()
        } else {
            None
        };
        // Apply the same fractional-octave smoothing to the full Transfer-layout
        // magnitude trace so it matches the grid-view virtual cells. Only
        // magnitude-dB is smoothed — phase has 2π wraps that would average to
        // meaningless midpoints, and coherence is already a windowed stat.
        if let (Some(n_frac), Some(tf)) = (self.smoothing_frac, transfer_snap.as_mut()) {
            if !tf.freqs.is_empty() && !tf.magnitude_db.is_empty() {
                let windows = smoothing::OctaveWindows::build(n_frac, &tf.freqs);
                tf.magnitude_db = smoothing::smooth_db(&tf.magnitude_db, &windows);
            }
        }
        let sweep_snap = if in_sweep_layout {
            Some(self.sweep_last.clone())
        } else {
            None
        };
        let sweep_kind_snap = self.sweep_kind;
        let sweep_sel_snap = self.sweep_selected_idx;
        let selection_order_snap = self.selection_order.clone();
        let active_meas_idx_snap = self.active_meas_idx;
        let active_meas_snap = {
            // Inline to dodge an otherwise-mutable borrow of `self` held by
            // `render_ctx.as_mut()` above.
            let n = selection_order_snap.len();
            if n >= 2 {
                let meas_count = n - 1;
                let idx = active_meas_idx_snap.min(meas_count - 1);
                Some(selection_order_snap[idx])
            } else {
                None
            }
        };
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
            // In Transfer layout the y-axis meaning depends on which
            // sub-panel the cursor is in — mag shows dB, phase shows degrees,
            // coh shows 0..1. Outside all three panels (the inter-panel gap)
            // we fall back to mag dB so the crosshair label stays populated.
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
            } else if matches!(config_snap.layout, LayoutMode::Transfer) {
                let cursor = egui::pos2(cx, cy);
                match crate::render::transfer::hit_test(rect, cursor) {
                    Some((crate::render::transfer::HitPanel::Phase, v)) => {
                        HoverReadout::Phase(v)
                    }
                    Some((crate::render::transfer::HitPanel::Coherence, v)) => {
                        HoverReadout::Coherence(v)
                    }
                    Some((crate::render::transfer::HitPanel::Magnitude, v)) => {
                        HoverReadout::Db(v)
                    }
                    None => {
                        let db = view.db_min + ny * (view.db_max - view.db_min);
                        HoverReadout::Db(db)
                    }
                }
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
                if matches!(config_snap.layout, LayoutMode::Transfer) {
                    let color = theme::channel_color(cell.channel);
                    crate::render::transfer::draw(
                        &painter,
                        rect,
                        &view,
                        transfer_snap.as_ref(),
                        color,
                    );
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
                grid::draw_grid(
                    &painter,
                    grid_rect,
                    &view,
                    config_snap.view_mode,
                    show_labels,
                    grid_freq_labels,
                    time_axis,
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
                // Drum-tuner overlay: f0 marker + membrane-mode partial
                // markers + corner readout. Spectrum view only, real channels
                // only. Sits above the peak-hold glyphs so the tuner markers
                // aren't obscured by them on a loud drum hit where both
                // features co-exist.
                if tuner_mode_snap != TunerMode::Off
                    && matches!(config_snap.view_mode, ViewMode::Spectrum)
                    && cell.channel < n_real_snap
                {
                    let raw_cand = tuner_last_snap
                        .get(cell.channel)
                        .and_then(|o| o.as_ref());
                    let cand_opt = raw_cand
                        .filter(|c| c.confidence >= TUNER_MIN_CONFIDENCE);
                    let hist_empty: Vec<f64> = Vec::new();
                    let hist = tuner_history_snap
                        .get(cell.channel)
                        .unwrap_or(&hist_empty);
                    if std::env::var_os("AC_TUNER_DEBUG").is_some() {
                        eprintln!(
                            "[ui draw] ch{} mode={:?} raw={} cand_pass={} hist_len={} freqs_len={}",
                            cell.channel, tuner_mode_snap,
                            raw_cand.map(|c| format!("f0={:.1}Hz conf={:.2}", c.freq_hz, c.confidence))
                                .unwrap_or_else(|| "None".into()),
                            cand_opt.is_some(), hist.len(),
                            frames.get(cell.channel).and_then(|f| f.as_ref()).map(|f| f.freqs.len()).unwrap_or(0),
                        );
                    }
                    if cand_opt.is_some()
                        || !hist.is_empty()
                        || tuner_range_lock_snap.is_some()
                    {
                        let lock = tuner_locks_snap
                            .get(cell.channel)
                            .copied()
                            .flatten();
                        let freqs: &[f32] = frames
                            .get(cell.channel)
                            .and_then(|f| f.as_ref())
                            .map(|f| f.freqs.as_slice())
                            .unwrap_or(&[]);
                        draw_tuner_overlay(
                            &painter,
                            grid_rect,
                            cell.channel,
                            cand_opt,
                            freqs,
                            hist,
                            &view,
                            lock,
                            tuner_mode_snap,
                            tuner_range_lock_snap,
                            tuner_sens_snap,
                            tuner_min_level_snap,
                        );
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
            overlay::draw(
                ui_ctx,
                OverlayInput {
                    config: &config_snap,
                    frames: &frames,
                    cell_views: &cell_views_snap,
                    selected: &selected_snap,
                    selection_order: &selection_order_snap,
                    transfer: transfer_snap.as_ref(),
                    active_meas: active_meas_snap,
                    active_meas_idx: active_meas_idx_snap,
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
            let transfer_for_capture = if in_transfer_layout {
                self.transfer_last.clone()
            } else {
                None
            };
            finalize_capture(ctx, cap, &self.output_dir, &frames, transfer_for_capture);
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

/// Per-cell peak-hold overlay: hottest-peak triangle + label, 2×–5× harmonic
/// ticks, and a corner "PEAK CHn: f Hz A dB" readout. Called inside the egui
/// closure (Spectrum view, real channels only). Skips DC/sub-audio bins so
/// a spurious low-frequency excursion can't lock the marker.
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
    let log_min = view.freq_min.max(1.0).log10();
    let log_max = view.freq_max.max(log_min.exp().max(1.1)).log10();
    let log_span = (log_max - log_min).max(0.0001);
    let db_span = (view.db_max - view.db_min).max(0.0001);

    // Argmax across only the visible freq window, clamped to ≥ 20 Hz so the
    // marker can't latch onto DC or sub-audio noise.
    let mut best_idx: Option<usize> = None;
    let mut best_amp = f32::NEG_INFINITY;
    for (i, (&f, &amp)) in freqs.iter().zip(peak.iter()).enumerate() {
        if !f.is_finite() || !amp.is_finite() {
            continue;
        }
        if f < view.freq_min.max(theme::DEFAULT_FREQ_MIN) || f > view.freq_max {
            continue;
        }
        if amp > best_amp {
            best_amp = amp;
            best_idx = Some(i);
        }
    }
    let Some(argmax) = best_idx else { return };
    let f0 = freqs[argmax];
    let a0 = peak[argmax];

    // Colour the markers with the channel's own palette entry so Compare
    // layout — where every selected channel's peak traces stack into the
    // same rect — lets the eye pair each triangle/label/readout with its
    // underlying spectrum trace. `PEAK_MARKER` (cyan) was fine for a single
    // channel but ambiguous once N peaks share one cell.
    let ch_rgba = theme::channel_color(channel);
    let marker_color = Color32::from_rgb(
        (ch_rgba[0] * 255.0) as u8,
        (ch_rgba[1] * 255.0) as u8,
        (ch_rgba[2] * 255.0) as u8,
    );
    let tick_color = Color32::from_rgba_unmultiplied(
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

    // Collect the 2×..=5× harmonic samples up front so the same data drives
    // both the on-plot markers and the corner text block. A harmonic is kept
    // regardless of whether its frequency is inside the current view window —
    // the corner readout still wants to show its dB — but on-plot drawing
    // gates on the visible range.
    struct Harmonic {
        k: u32,
        f_hz: f32,
        amp_db: f32,
        in_view: bool,
    }
    let mut harmonics: Vec<Harmonic> = Vec::with_capacity(4);
    for k in 2u32..=5 {
        let f_k = f0 * k as f32;
        if freqs.last().map_or(true, |&f| f_k > f) {
            break;
        }
        // Nearest-bin search; freqs are monotonic so a simple partition_point
        // lands on the right neighbour. Clamp to the buffer so the amp lookup
        // never panics.
        let near = match freqs.partition_point(|&f| f < f_k) {
            0 => 0,
            p if p >= freqs.len() => freqs.len() - 1,
            p => {
                if (freqs[p] - f_k).abs() < (f_k - freqs[p - 1]).abs() {
                    p
                } else {
                    p - 1
                }
            }
        };
        let a_k = peak[near];
        if !a_k.is_finite() {
            continue;
        }
        harmonics.push(Harmonic {
            k,
            f_hz: f_k,
            amp_db: a_k,
            in_view: f_k >= view.freq_min && f_k <= view.freq_max,
        });
    }

    // Harmonic markers: small downward triangle + "k×" label at each
    // in-view harmonic's amplitude. A 5-px vertical tick was too thin to
    // read against the spectrum trace; the triangle matches the fundamental
    // glyph at half size so the family resemblance is obvious.
    for h in harmonics.iter().filter(|h| h.in_view) {
        let p = freq_amp_to_px(h.f_hz, h.amp_db);
        let tri = [
            egui::pos2(p.x - 3.0, p.y - 8.0),
            egui::pos2(p.x + 3.0, p.y - 8.0),
            egui::pos2(p.x,       p.y - 2.0),
        ];
        painter.add(egui::Shape::convex_polygon(
            tri.to_vec(),
            tick_color,
            egui::Stroke::new(1.0, tick_color),
        ));
        painter.text(
            egui::pos2(p.x, p.y - 10.0),
            egui::Align2::CENTER_BOTTOM,
            format!("{}×", h.k),
            egui::FontId::monospace(theme::GRID_LABEL_PX),
            tick_color,
        );
    }

    // Fundamental marker: downward triangle above the peak point + label.
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

    // Top-right corner readout — visible even when the peak is off-screen
    // (e.g. user zoomed below the fundamental). The header gives the
    // fundamental; one indented line per 2×..=5× harmonic follows with its
    // frequency and dB. `corner_slot` stacks one full block per channel so
    // Compare layout's overlapping cells don't overdraw a single pixel;
    // Grid/Single always pass 0 and render at the top.
    let row_h = theme::GRID_LABEL_PX + 2.0;
    let block_rows = 1 + harmonics.len();
    let block_top = rect.top() + 2.0 + corner_slot as f32 * block_rows as f32 * row_h;
    let corner = crate::ui::fmt::peak_corner_label(channel, f0, a0);
    painter.text(
        egui::pos2(rect.right() - 4.0, block_top),
        egui::Align2::RIGHT_TOP,
        corner,
        egui::FontId::monospace(theme::GRID_LABEL_PX),
        marker_color,
    );
    for (i, h) in harmonics.iter().enumerate() {
        let line = crate::ui::fmt::peak_harmonic_line(h.k, h.f_hz, h.amp_db);
        painter.text(
            egui::pos2(
                rect.right() - 4.0,
                block_top + (i + 1) as f32 * row_h,
            ),
            egui::Align2::RIGHT_TOP,
            line,
            egui::FontId::monospace(theme::GRID_LABEL_PX),
            tick_color,
        );
    }
}

// Compact frequency formatter for the peak overlay lives in `ui::fmt`;
// re-export here so existing call sites in this file don't have to fully
// qualify the path.
use crate::ui::fmt::format_freq_compact;

/// Per-cell drum-tuner overlay. Draws a full-height vertical line at the
/// identified (0,1) fundamental, shorter ticks at each matched overtone
/// partial, and a corner readout stack with Hz/note/cents/confidence and
/// (when locked) the target + deviation with traffic-light colouring.
///
/// Coordinate system matches `draw_peak_overlay`: log-frequency x-axis
/// clamped to `view.freq_min..freq_max`; any marker whose Hz falls outside
/// the visible window is skipped for the on-plot glyph but still appears
/// in the corner text so the reader sees it exists.
#[allow(clippy::too_many_arguments)]
fn draw_tuner_overlay(
    painter: &egui::Painter,
    rect: egui::Rect,
    channel: usize,
    cand: Option<&ac_core::tuner::FundamentalCandidate>,
    freqs: &[f32],
    history: &[f64],
    view: &CellView,
    lock_target_hz: Option<f64>,
    mode: TunerMode,
    range_lock: Option<(f64, f64)>,
    sensitivity: TunerSensitivity,
    min_level_dbfs: Option<f32>,
) {
    let log_min = view.freq_min.max(1.0).log10();
    let log_max = view.freq_max.max(log_min.exp().max(1.1)).log10();
    let log_span = (log_max - log_min).max(0.0001);
    // Daemon aggregates raw FFT bins into log-spaced display columns, so
    // the parabolic-interp Hz sitting between two column centers lands off
    // the visible peak. Snap marker-x to the nearest column center so the
    // vertical line always sits on a plotted sample.
    let snap_to_column = |f: f32| -> f32 {
        if freqs.len() < 2 {
            return f;
        }
        let idx = freqs.partition_point(|&v| v < f);
        if idx == 0 {
            freqs[0]
        } else if idx >= freqs.len() {
            *freqs.last().unwrap()
        } else {
            let lo = freqs[idx - 1];
            let hi = freqs[idx];
            if (f - lo).abs() <= (hi - f).abs() {
                lo
            } else {
                hi
            }
        }
    };
    let freq_to_x = |f: f32| -> Option<f32> {
        let fs = snap_to_column(f);
        if !fs.is_finite() || fs < view.freq_min || fs > view.freq_max {
            return None;
        }
        let tx = (fs.max(1.0).log10() - log_min) / log_span;
        Some(rect.left() + tx.clamp(0.0, 1.0) * rect.width())
    };

    let ch_rgba = theme::channel_color(channel);
    let base = Color32::from_rgba_unmultiplied(
        (ch_rgba[0] * 255.0) as u8,
        (ch_rgba[1] * 255.0) as u8,
        (ch_rgba[2] * 255.0) as u8,
        220,
    );
    let partial_color = Color32::from_rgba_unmultiplied(
        (ch_rgba[0] * 255.0) as u8,
        (ch_rgba[1] * 255.0) as u8,
        (ch_rgba[2] * 255.0) as u8,
        140,
    );
    let text_color = Color32::from_rgb(theme::TEXT[0], theme::TEXT[1], theme::TEXT[2]);

    // Fundamental marker: solid vertical line through the whole cell so
    // it's visible even when the (0,1) peak itself is below the dB floor.
    if let Some(cand) = cand {
        if let Some(x) = freq_to_x(cand.freq_hz as f32) {
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                egui::Stroke::new(2.0, base),
            );
            let (note, cents_off) = crate::ui::fmt::hz_to_note(cand.freq_hz);
            let label = format!("f0 {:.1} Hz  {} {:+.0}¢", cand.freq_hz, note, cents_off);
            painter.text(
                egui::pos2(x + 4.0, rect.top() + 2.0),
                egui::Align2::LEFT_TOP,
                label,
                egui::FontId::monospace(theme::GRID_LABEL_PX),
                base,
            );
        }

        // Overtone partial ticks: short verticals in the lower half of the
        // cell so they don't collide with the fundamental label at the top.
        // Partial ticks only — no per-tick text labels. At 8–11 matched
        // partials squeezed into a narrow log-scaled Hz band the labels
        // stack on top of each other and become unreadable; the corner
        // readout already enumerates the same (m,n)/Δ% data with the
        // partials sorted by ideal ratio so the user has a clean table to
        // read instead of a smear on the plot.
        for p in cand.partials.iter().skip(1) {
            if let Some(x) = freq_to_x(p.measured_hz as f32) {
                let y_top = rect.top() + rect.height() * 0.65;
                painter.line_segment(
                    [egui::pos2(x, y_top), egui::pos2(x, rect.bottom())],
                    egui::Stroke::new(1.0, partial_color),
                );
            }
        }
    }

    // Lock target: dashed vertical at the remembered target Hz plus a
    // traffic-light-coloured deviation readout so the tuner reads at a
    // glance while the user retunes a lug. Green within ±5¢ is the
    // standard acceptance band for "in tune"; yellow/red escalate from there.
    if matches!(mode, TunerMode::Locked) {
        if let (Some(target), Some(cand)) = (lock_target_hz, cand) {
            let delta_cents = crate::ui::fmt::cents(cand.freq_hz, target);
            let lock_color = if delta_cents.abs() <= 5.0 {
                Color32::from_rgb(80, 220, 120)
            } else if delta_cents.abs() <= 20.0 {
                Color32::from_rgb(230, 200, 60)
            } else {
                Color32::from_rgb(230, 80, 60)
            };
            if let Some(x) = freq_to_x(target as f32) {
                // Dashed line: paint 4 px segments with 4 px gaps so the
                // lock marker is visually distinct from the solid f0 line.
                let mut y = rect.top();
                while y < rect.bottom() {
                    let y1 = (y + 4.0).min(rect.bottom());
                    painter.line_segment(
                        [egui::pos2(x, y), egui::pos2(x, y1)],
                        egui::Stroke::new(1.0, lock_color),
                    );
                    y += 8.0;
                }
            }
            let delta_hz = cand.freq_hz - target;
            let lock_line = format!(
                "target {:.1} Hz  Δ {:+.2} Hz  {:+.1}¢",
                target, delta_hz, delta_cents,
            );
            painter.text(
                egui::pos2(rect.right() - 4.0, rect.bottom() - 4.0),
                egui::Align2::RIGHT_BOTTOM,
                lock_line,
                egui::FontId::monospace(theme::GRID_LABEL_PX),
                lock_color,
            );
        }
    }

    // Corner readout (top-right of this cell). Stacks below the peak-hold
    // corner block when peak hold is also on: the peak block uses slot 0
    // at `rect.top() + 2.0`, so anchor the tuner block further down. The
    // height offset is a fixed constant; the two overlays are each short
    // enough that an exact measurement would be overkill.
    let row_h = theme::GRID_LABEL_PX + 2.0;
    let peak_block_rows = 6.0;
    let mut y = rect.top() + 2.0 + peak_block_rows * row_h;
    if let Some(cand) = cand {
        let corner_text = crate::ui::fmt::tuner_corner_label(cand);
        let n_lines = corner_text.lines().count().max(1) as f32;
        painter.text(
            egui::pos2(rect.right() - 4.0, y),
            egui::Align2::RIGHT_TOP,
            corner_text,
            egui::FontId::monospace(theme::GRID_LABEL_PX),
            text_color,
        );
        y += n_lines * row_h;
    }
    // Range-lock indicator: one line showing the clamped search window
    // when active, so the user can tell at a glance why the tuner is
    // ignoring sub-harmonic candidates outside the band.
    if let Some((lo, hi)) = range_lock {
        painter.text(
            egui::pos2(rect.right() - 4.0, y),
            egui::Align2::RIGHT_TOP,
            format!("range-lock {:.0}-{:.0} Hz", lo, hi),
            egui::FontId::monospace(theme::GRID_LABEL_PX),
            text_color,
        );
        y += row_h;
    }
    // History block: newest last, so the list reads top-down as oldest →
    // latest. Label prefix lets the user parse it even when no live f0 is
    // showing (between triggers or after a low-confidence hit).
    if !history.is_empty() {
        painter.text(
            egui::pos2(rect.right() - 4.0, y),
            egui::Align2::RIGHT_TOP,
            "last hits:",
            egui::FontId::monospace(theme::GRID_LABEL_PX),
            text_color,
        );
        y += row_h;
        for &hz in history {
            let (note, cents_off) = crate::ui::fmt::hz_to_note(hz);
            painter.text(
                egui::pos2(rect.right() - 4.0, y),
                egui::Align2::RIGHT_TOP,
                format!("{:.1} Hz  {} {:+.0}¢", hz, note, cents_off),
                egui::FontId::monospace(theme::GRID_LABEL_PX),
                text_color,
            );
            y += row_h;
        }
    }
    // Sensitivity + min-level readout — always shown while the tuner is
    // visible so the user can see what detector settings produced the
    // candidate above (or what's blocking one).
    let level_str = match min_level_dbfs {
        Some(v) => format!("{:.0} dBFS", v),
        None => "off".to_string(),
    };
    painter.text(
        egui::pos2(rect.right() - 4.0, y),
        egui::Align2::RIGHT_TOP,
        format!("sens {}  min {}", sensitivity.label(), level_str),
        egui::FontId::monospace(theme::GRID_LABEL_PX),
        text_color,
    );
}
