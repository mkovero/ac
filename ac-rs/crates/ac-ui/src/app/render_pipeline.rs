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
use crate::render::ember::EmberScissor;
use crate::render::grid;
use crate::render::spectrum::{ChannelMeta, ChannelUpload};
use crate::render::waterfall::CellUpload as WaterfallCellUpload;
use crate::theme;
use crate::ui::export::{self, ScreenshotRequest};
use crate::ui::layout;
use crate::ui::overlay::{
    self, HoverInfo, HoverReadout, MonitorParamsInfo, OverlayInput, TimeIntegrationOverlay,
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
        TimeIntegrationMode::Off => return None,
        TimeIntegrationMode::Fast => ("fast", Some(TAU_FAST_S)),
        TimeIntegrationMode::Slow => ("slow", Some(TAU_SLOW_S)),
        TimeIntegrationMode::Leq => ("Leq", None),
    };
    let duration_s = frames
        .iter()
        .flatten()
        .find_map(|f| f.meta.leq_duration_s)
        .filter(|d: &f64| d.is_finite());
    Some(TimeIntegrationOverlay {
        mode: label,
        tau_s,
        duration_s,
    })
}

use super::helpers::{
    median_f32, NOTIFICATION_TTL, PEAK_HOLD_DECAY, PEAK_RELEASE_DB_PER_SEC,
    WATERFALL_ROW_DT_HYSTERESIS, WATERFALL_ROW_DT_MIN, WATERFALL_ROW_DT_WINDOW,
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
        let pending_error = self
            .source
            .as_ref()
            .and_then(|src| src.status())
            .and_then(|s| s.take_error());
        if let Some(err) = pending_error {
            if err.contains("transfer_stream") {
                self.transfer_stream_active = false;
            }
            self.notify(&err);
        }
        // Goniometer is real-channel-only and pair-consuming: it always
        // shows the active channel paired with its immediate neighbour
        // (active, active+1) — no `T`-registered virtual channel involved.
        // `None` when there's no real neighbour (last channel / mono).
        let bode_pair: Option<crate::data::types::TransferPair> =
            matches!(self.config.view_mode, ViewMode::Goniometer)
                .then(|| {
                    let n_real = self.store.as_ref().map(|s| s.len()).unwrap_or(0);
                    let active = self.config.active_channel;
                    (active + 1 < n_real).then_some(crate::data::types::TransferPair {
                        ref_ch: active as u32,
                        meas: (active + 1) as u32,
                    })
                })
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
        // channel before its first packet).
        //
        // `transfer_stream` ships a linear-Hz axis (uniform index-stride
        // decimation), not the log-spaced/pre-aggregated layout monitor
        // frames use — uploading it as-is scrambled the drawn frequency
        // axis against every other per-frequency element in the cell (#162
        // problem P1). `samples_on_axis_to_columns` re-aggregates onto the
        // same log-column grid and band-power statistic real frames use, so
        // the spectrum/waterfall/ember renderers (which all assume
        // log-uniform columns) draw it correctly without themselves needing
        // to know about the transfer path.
        //
        // Coherence gating (P2) happens at this same construction site so
        // every consumer of the resulting `DisplayFrame` inherits it from
        // one place: a column whose measured γ² samples median below
        // `theme::PHASE_COH_GATE` is set to NAN, which the spectrum/
        // waterfall/ember renderers already treat as a gap.
        let n_real = frames.len();
        let virtual_snapshots = self.virtual_channels.read_all_with_serial();
        self.virtual_render_pairs = virtual_snapshots.iter().map(|(p, _, _)| *p).collect();
        {
            let live: std::collections::HashSet<_> =
                virtual_snapshots.iter().map(|(p, _, _)| *p).collect();
            self.virtual_seen_serial.retain(|p, _| live.contains(p));
        }
        for (pair, serial, maybe_tf) in &virtual_snapshots {
            let is_fresh =
                *serial != 0 && self.virtual_seen_serial.get(pair).copied().unwrap_or(0) != *serial;
            if is_fresh {
                self.virtual_seen_serial.insert(*pair, *serial);
            }
            let frame = maybe_tf.as_ref().map(|tf| {
                // Same (f_min, f_max, n_columns) convention the daemon uses
                // for real monitor frames (`spectrum_to_columns_wire`), so
                // the virtual cell's freq range lines up with real cells'.
                let f_min = theme::DEFAULT_FREQ_MIN;
                let f_max = (tf.sr as f32 / 2.0).max(f_min + 1.0);
                let n_columns = ac_core::visualize::aggregate::DEFAULT_WIRE_COLUMNS;
                let mut mags = ac_core::visualize::aggregate::samples_on_axis_to_columns(
                    &tf.freqs,
                    &tf.magnitude_db,
                    f_min,
                    f_max,
                    n_columns,
                );
                gate_transfer_columns_by_coherence(
                    &mut mags,
                    &tf.freqs,
                    &tf.coherence,
                    f_min,
                    f_max,
                );
                let freqs = column_centre_freqs_f32(f_min, f_max, n_columns);
                let spectrum = Arc::new(mags);
                DisplayFrame {
                    spectrum: spectrum.clone(),
                    freqs: Arc::new(freqs),
                    meta: FrameMeta {
                        freq_hz: 0.0,
                        fundamental_dbfs: -140.0,
                        thd_pct: 0.0,
                        thdn_pct: 0.0,
                        in_dbu: None,
                        dbu_offset_db: None,
                        peaks: Arc::new(Vec::new()),
                        spl_offset_db: None,
                        mic_correction: None,
                        sr: tf.sr,
                        clipping: false,
                        xruns: 0,
                        leq_duration_s: None,
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
        // Inlined to dodge a borrow conflict with the mutable `ctx`
        // already held — calling `self.layout_selection()` here would
        // need `&self`, but `ctx` already pins `&mut self.render_ctx`.
        // Direct field access lets NLL split the borrow per field.
        let layout_sel: &[bool] = if matches!(self.config.layout, LayoutMode::Compare) {
            &self.compare_set
        } else {
            &self.selected
        };
        let cells = layout::compute(
            self.config.layout,
            n_channels,
            self.config.active_channel,
            layout_sel,
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
                    .is_none_or(|w| !w.matches(n_frac, frame.freqs.len(), last_f));
                if needs_rebuild {
                    self.smoothing_cache = Some(smoothing::OctaveWindows::build(
                        n_frac,
                        frame.freqs.as_ref(),
                    ));
                }
                let windows = self.smoothing_cache.as_ref().unwrap();
                let smoothed = smoothing::smooth_db(frame.spectrum.as_slice(), windows);
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
            let first_new = self.cell_views.len();
            self.cell_views.resize(n_total, CellView::default());
            // Virtual (transfer) slots get the dB-re-unity window on
            // creation (#163 P2) instead of the dBFS default — |H(ω)| has
            // no meaning on a -120..0 dBFS scale. `first_new.max(n_real)`
            // is a no-op guard for the (impossible today, cheap to keep
            // correct) case where this resize covers real slots too.
            for cv in self.cell_views.iter_mut().skip(first_new.max(n_real)) {
                cv.db_min = theme::VIRTUAL_DB_MIN;
                cv.db_max = theme::VIRTUAL_DB_MAX;
            }
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
                let stamp = self.peak_last_update.get_mut(i).expect("resized above");
                let tick = self.peak_last_tick.get_mut(i).expect("resized above");
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
                                for (held, fresh) in existing.iter_mut().zip(frame.spectrum.iter())
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
                let stamp = self.min_last_update.get_mut(i).expect("resized above");
                let tick = self.min_last_tick.get_mut(i).expect("resized above");
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
                                for (held, fresh) in existing.iter_mut().zip(frame.spectrum.iter())
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
                            let slice: Vec<f32> = self.waterfall_row_dts.iter().copied().collect();
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
        // Stretch the freq clamp to whatever sr/2 the producer is running at.
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
                    let idx = ((finite.len() as f32 * 0.98) as usize).min(finite.len() - 1);
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
                ViewMode::Scope | ViewMode::SpectrumEmber | ViewMode::Goniometer => {}
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
                    let single_virtual =
                        matches!(self.config.layout, LayoutMode::Single) && cell.channel >= n_real;
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
                                let min_color =
                                    [base[0] * 0.55, base[1] * 0.55, base[2] * 0.55, 1.0];
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
                ViewMode::Scope | ViewMode::SpectrumEmber | ViewMode::Goniometer => {
                    // Ember-substrate views consume polylines built later in
                    // this method (synthetic sine for Scope, the active
                    // channel's spectrum frame for SpectrumEmber, synthetic
                    // stereo signals for Goniometer). The cell iteration is
                    // kept so Single-layout viewport math runs.
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
            ViewMode::Scope | ViewMode::SpectrumEmber | ViewMode::Goniometer => {}
        }

        let raw_input = egui_state.take_egui_input(&ctx.window);
        let show_labels = self.config.layout != LayoutMode::Grid || n_channels <= 8;
        let connected = self.source.as_ref().map(|s| s.connected()).unwrap_or(false);
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
        let time_integration_snap = build_time_integration_overlay(self.time_integration, &frames);
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
        // LF band is active only while the live N is below the daemon's LF N;
        // once the user raises N to/above it the live spectrum is already at
        // least as fine, so the LF augmentation (and its readout line) drops
        // back to the single-line fallback (#142).
        let lf_active = self
            .monitor_lf_fft_n
            .filter(|&lf_n| lf_n > self.monitor_fft_n);
        let monitor_params_snap = (self.analysis_mode == "fft").then_some(MonitorParamsInfo {
            interval_ms: self.monitor_interval_ms,
            fft_n: self.monitor_fft_n,
            lf_fft_n: lf_active,
            crossover_hz: lf_active.and(self.monitor_crossover_hz),
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
            let view = cell_views_snap.get(channel).copied().unwrap_or_default();
            let log_min = view.freq_min.max(1.0).log10();
            let log_max = view.freq_max.max(log_min.exp().max(1.1)).log10();
            let freq_hz = 10_f32.powf(log_min + nx * (log_max - log_min));
            let readout = if matches!(config_snap.layout, LayoutMode::Sweep) {
                let cursor = egui::pos2(cx, cy);
                let kind = sweep_kind_snap.unwrap_or(SweepKind::Frequency);
                match crate::render::sweep::hit_test(rect, cursor, kind) {
                    Some((crate::render::sweep::SweepHitPanel::Thd, v)) => HoverReadout::Thd(v),
                    Some((crate::render::sweep::SweepHitPanel::Gain, v)) => HoverReadout::Gain(v),
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
            } else if matches!(config_snap.view_mode, ViewMode::SpectrumEmber) {
                // Ember footer reads the trace magnitude at the hovered bin
                // (sampled from the frame spectrum in the overlay), not the
                // geometric cursor-Y. The dB is filled in there (#154).
                HoverReadout::SpectrumBin
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
                let time_axis = matches!(config_snap.view_mode, ViewMode::Waterfall).then(|| {
                    grid::WaterfallTimeAxis {
                        row_period_s,
                        rows_visible: view.rows_visible_f,
                    }
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
                    let top = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, split_y));
                    let bot = egui::Rect::from_min_max(egui::pos2(rect.min.x, split_y), rect.max);
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
                    cell.channel >= n_real_snap,
                );
                // Channel-identity affordances for ember views, which
                // otherwise paint every cell in the same thermal palette.
                // 2 px frame accent at the top edge in the cell's
                // desaturated channel hue, plus a top-left `CHn` /
                // `transferN` label. The pair view (Goniometer) renders
                // two halves: ref on the left, DUT (`meas`) on the right.
                // Skipped for non-ember views — they get their identity
                // from `draw_grid`'s axis labels.
                let is_ember_view = matches!(
                    config_snap.view_mode,
                    ViewMode::SpectrumEmber | ViewMode::Scope | ViewMode::Goniometer
                );
                if is_ember_view {
                    let is_pair_view = matches!(config_snap.view_mode, ViewMode::Goniometer);
                    let accent_h = 2.0;
                    let make_color = |idx: usize| {
                        let c = theme::desaturated_channel_color(idx);
                        egui::Color32::from_rgba_unmultiplied(
                            (c[0] * 255.0) as u8,
                            (c[1] * 255.0) as u8,
                            (c[2] * 255.0) as u8,
                            (c[3] * 220.0) as u8,
                        )
                    };
                    let top = rect.top();
                    let left = rect.left();
                    let right = rect.right();
                    if is_pair_view {
                        if let Some(pair) = bode_pair_snap {
                            let mid = 0.5 * (left + right);
                            let bar_left = egui::Rect::from_min_max(
                                egui::pos2(left, top),
                                egui::pos2(mid, top + accent_h),
                            );
                            let bar_right = egui::Rect::from_min_max(
                                egui::pos2(mid, top),
                                egui::pos2(right, top + accent_h),
                            );
                            painter.rect_filled(
                                bar_left,
                                egui::CornerRadius::ZERO,
                                make_color(pair.ref_ch as usize),
                            );
                            painter.rect_filled(
                                bar_right,
                                egui::CornerRadius::ZERO,
                                make_color(pair.meas as usize),
                            );
                        } else {
                            // No pair registered yet — single accent in
                            // the cell's own channel hue so the cell is
                            // still visually identifiable.
                            let bar = egui::Rect::from_min_max(
                                egui::pos2(left, top),
                                egui::pos2(right, top + accent_h),
                            );
                            painter.rect_filled(
                                bar,
                                egui::CornerRadius::ZERO,
                                make_color(cell.channel),
                            );
                        }
                    } else {
                        let bar = egui::Rect::from_min_max(
                            egui::pos2(left, top),
                            egui::pos2(right, top + accent_h),
                        );
                        painter.rect_filled(
                            bar,
                            egui::CornerRadius::ZERO,
                            make_color(cell.channel),
                        );
                    }
                    // Per-cell label, top-left, low-contrast grey, 10 px
                    // inset. Real channels read `CHn`; virtual transfer
                    // cells read `transferN`. Reuses the existing
                    // `channel_label` helper so the convention stays
                    // shared with the hover readout.
                    let label = crate::ui::overlay::channel_label(
                        cell.channel,
                        n_real_snap,
                        &virtual_pairs_snap,
                    );
                    painter.text(
                        egui::pos2(left + 10.0, top + 10.0),
                        egui::Align2::LEFT_TOP,
                        label,
                        egui::FontId::monospace(theme::STATUS_PX),
                        egui::Color32::from_rgba_unmultiplied(170, 170, 170, 200),
                    );
                }
                let is_selected = selected_snap.get(cell.channel).copied().unwrap_or(false);
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
                    if let (Some(Some(peak)), Some(Some(frame))) =
                        (peak_holds_snap.get(cell.channel), frames.get(cell.channel))
                    {
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
                                &painter,
                                bot,
                                &view,
                                tf,
                                show_labels,
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
                    virtual_transfer: &virtual_tf_snap,
                    active_palette: active_palette_snap,
                    smoothing_frac: smoothing_snap,
                    ioct_bpo: ioct_bpo_snap,
                    tier_badge: tier_badge_snap.clone(),
                    time_integration: time_integration_snap.clone(),
                    band_weighting: band_weighting_snap,
                    loudness: loudness_snap,
                    gonio_state: gonio_state_snap,
                    keytips: &keytips,
                    peak_hold: peak_hold_enabled_snap,
                    min_hold: min_hold_enabled_snap,
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

        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
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
        let egui_writes = ctx.timing.as_ref().map(|t| t.egui_writes());

        // Ember substrate: decay + deposit happen as their own off-screen
        // render passes ahead of the surface clear, so the display pass
        // inside the spectrum pass can sample the freshly written buffer.
        // The renderer is substrate-only — caller supplies the polyline
        // and scroll velocity per view kind.
        if matches!(
            view_mode,
            ViewMode::Scope | ViewMode::SpectrumEmber | ViewMode::Goniometer
        ) {
            let now = Instant::now();
            let dt = self
                .ember_last_tick
                .map(|t| now.saturating_duration_since(t).as_secs_f32())
                .unwrap_or(1.0 / 60.0)
                .clamp(0.0, 0.25);
            self.ember_last_tick = Some(now);

            // Force-clear the substrate when the layout configuration changes
            // so a prior config's phosphor cannot linger under the new one —
            // the grid-leak / stale-trace regression (#153). The key folds in
            // the active view, layout, and every visible cell's channel +
            // geometry, so a Single↔Grid swap, a page turn, a channel-set
            // change, or a view switch (e.g. scrolling Scope → static
            // SpectrumEmber) all trip a clear. Persistence within one config is
            // untouched: an unchanged key never clears.
            let gen = ember_layout_generation(view_mode, self.config.layout, &cells);
            if self.ember_layout_gen != Some(gen) {
                ember.request_clear();
                self.ember_layout_gen = Some(gen);
            }

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
                        &ctx.device,
                        &ctx.queue,
                        &mut encoder,
                        [0.0, 0.0, 1.0, 1.0],
                        &polyline,
                        scroll_dx,
                        dt,
                        &[],
                    );
                }
                ViewMode::SpectrumEmber => {
                    // Single layout (default): paint the active channel
                    // across the full canvas. Grid layout: paint every
                    // visible cell's channel into its own sub-rect by
                    // transforming each per-cell polyline from cell-
                    // local [0,1] to canvas-space [cell.x..cell.x+w] /
                    // [cell.y..cell.y+h], then concatenate. One
                    // `ember.advance` covers the whole grid in a single
                    // decay+deposit pass — multiple advance calls would
                    // each decay the substrate and over-fade.
                    //
                    // Read from the local `frames` copy, not
                    // `self.last_frames` — by this point in redraw it
                    // carries the daemon's weighting / time integration
                    // AND the UI-side fractional-octave smoothing
                    // applied earlier in this method.
                    // Per-cell scissor rects, populated only in Grid layout so
                    // each channel's phosphor stays inside its own sub-rect of
                    // the shared substrate (no cross-cell bleed, #153). Single
                    // layout leaves this empty → whole-surface deposit + decay.
                    let mut scissors: Vec<EmberScissor> = Vec::new();
                    let polyline: Vec<[f32; 3]> = if matches!(self.config.layout, LayoutMode::Grid)
                    {
                        // Cell-edge inset for the empty-cell border:
                        // 2 % of cell size keeps the rectangle off
                        // the layout's gap line so adjacent empty
                        // cells don't visually merge.
                        const EDGE_INSET: f32 = 0.02;
                        // Faint constant-amplitude weight for empty
                        // cells. Decay (τ_p = 1.2 s) and ember
                        // intensity (~0.003) balance: a steady
                        // deposit at w=0.35 settles into a dim
                        // outline, dimmer than active envelopes
                        // which carry w=1.0 with peaks > 0.5 amp.
                        // Weight constant: `EMBER_EMPTY_W` (module scope).
                        let mut combined = Vec::new();
                        for cell in &cells {
                            // Skip virtual transfer cells — their
                            // ember rendering is pair-keyed, not
                            // per-channel-spectrum.
                            if cell.channel >= n_real {
                                continue;
                            }
                            let view = self
                                .cell_views
                                .get(cell.channel)
                                .copied()
                                .unwrap_or_default();
                            let frame_opt = frames
                                .get(cell.channel)
                                .and_then(|f| f.as_ref())
                                .filter(|f| !f.spectrum.is_empty());
                            // The ember substrate inverts y between
                            // deposit and display: deposit y=0 ends
                            // up at the top of the screen, y=1 at
                            // the bottom (NDC + wgpu viewport
                            // convention; verified by tracing the
                            // deposit / display shaders). The trace is
                            // baseline-anchored at the cell floor, so
                            // per-cell positions on the canvas need the
                            // flip applied (`ember_cell_to_canvas_y`) for
                            // the baseline to land at each cell's own
                            // bottom edge.
                            let start = combined.len() as u32;
                            if let Some(frame) = frame_opt {
                                let mut local = build_ember_spectrum_trace(
                                    &frame.freqs,
                                    &frame.spectrum,
                                    &view,
                                    EMBER_LIVE_W,
                                );
                                // Peak / min hold render as additional
                                // baseline-anchored envelopes over the
                                // live trace, at lower deposit weight so
                                // they recede behind it. The per-bin held
                                // buffers are maintained every frame in
                                // `redraw` regardless of view; ember view
                                // simply never drew them before (#149).
                                if self.peak_hold_enabled {
                                    if let Some(Some(held)) = self.peak_holds.get(cell.channel) {
                                        local.extend(build_ember_spectrum_trace(
                                            &frame.freqs,
                                            held,
                                            &view,
                                            EMBER_PEAK_W,
                                        ));
                                    }
                                }
                                if self.min_hold_enabled {
                                    if let Some(Some(held)) = self.min_holds.get(cell.channel) {
                                        local.extend(build_ember_spectrum_trace(
                                            &frame.freqs,
                                            held,
                                            &view,
                                            EMBER_MIN_W,
                                        ));
                                    }
                                }
                                // Focus emphasis: focused cell renders
                                // at full deposit weight; non-focus
                                // cells at 0.85× so the steady-state
                                // luminance reads as "dimmer / fading"
                                // without touching `ember.advance`'s
                                // single-substrate τ_p.
                                let focus_w = if cell.channel == self.config.active_channel {
                                    1.0
                                } else {
                                    0.85
                                };
                                // Image zoom + clamp into this cell's canvas
                                // rect. `ember_pack_cell` drops segments fully
                                // outside the cell and clamps stragglers, so a
                                // zoomed trace can never deposit a vertex into a
                                // neighbouring cell (#153 leak #2).
                                ember_pack_cell(&mut combined, &local, cell, &view, focus_w);
                            } else {
                                // No data yet for this channel — mark the
                                // clickable hitbox with four dim corner
                                // brackets and no full-width edge. A spanning
                                // horizontal edge aliases a real silent
                                // channel (a flat trace pinned at the floor),
                                // which was the #148 / #153 two-line symptom
                                // reaching the idle path. The void shares no
                                // horizontal reference with a measurement
                                // (#156). Geometry lives in
                                // `ember_idle_brackets` so it can be tested.
                                combined.extend(ember_idle_brackets(
                                    cell.x,
                                    cell.y,
                                    cell.w,
                                    cell.h,
                                    EDGE_INSET,
                                    EMBER_EMPTY_W,
                                ));
                            }
                            let end = combined.len() as u32;
                            if end > start {
                                scissors.push(EmberScissor {
                                    rect_norm: [cell.x, cell.y, cell.w, cell.h],
                                    range: [start, end],
                                });
                            }
                        }
                        combined
                    } else {
                        let active = self.config.active_channel;
                        let view = self.cell_views.get(active).copied().unwrap_or_default();
                        let raw = frames
                            .get(active)
                            .and_then(|f| f.as_ref())
                            .filter(|f| !f.spectrum.is_empty())
                            .map(|f| {
                                let mut v = build_ember_spectrum_trace(
                                    &f.freqs,
                                    &f.spectrum,
                                    &view,
                                    EMBER_LIVE_W,
                                );
                                // Held peak / min envelopes over the live
                                // trace — same per-bin buffers `redraw`
                                // maintains for every view (#149).
                                if self.peak_hold_enabled {
                                    if let Some(Some(held)) = self.peak_holds.get(active) {
                                        v.extend(build_ember_spectrum_trace(
                                            &f.freqs,
                                            held,
                                            &view,
                                            EMBER_PEAK_W,
                                        ));
                                    }
                                }
                                if self.min_hold_enabled {
                                    if let Some(Some(held)) = self.min_holds.get(active) {
                                        v.extend(build_ember_spectrum_trace(
                                            &f.freqs,
                                            held,
                                            &view,
                                            EMBER_MIN_W,
                                        ));
                                    }
                                }
                                v
                            })
                            .unwrap_or_default();
                        // Full-canvas Single cell `(0,0,1,1)` routed through the
                        // same pack/flip path as Grid so the baseline lands at
                        // the bottom edge in both layouts (the Single arm
                        // formerly skipped the flip → mirrored second trace,
                        // #148 / #153). One cell, no neighbour, so `scissors`
                        // stays empty and the deposit covers the whole surface.
                        let full = layout::CellRect {
                            channel: active,
                            x: 0.0,
                            y: 0.0,
                            w: 1.0,
                            h: 1.0,
                        };
                        let mut transformed: Vec<[f32; 3]> = Vec::with_capacity(raw.len());
                        ember_pack_cell(&mut transformed, &raw, &full, &view, 1.0);
                        transformed
                    };
                    // tau_p 1.2 s — short enough that an old peak's
                    // afterglow is gone in ~3 s, fast enough to keep up
                    // with sweeping or moving sources. The single baseline
                    // trace deposits ~half the vertices the old mirrored
                    // envelope did, so intensity is doubled to 0.006 to
                    // keep steady-state luminance where it was.
                    ember.set_tau_p(1.2 * self.ember_tau_p_scale);
                    ember.set_intensity(0.006 * self.ember_intensity_scale);
                    ember.set_tone(0.6, 1.5);
                    ember.advance(
                        &ctx.device,
                        &ctx.queue,
                        &mut encoder,
                        [0.0, 0.0, 1.0, 1.0],
                        &polyline,
                        0.0,
                        dt,
                        &scissors,
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
                    let (status, real_pair) =
                        resolve_stereo_pair(bode_pair, self.scope_store.as_ref(), want);
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
                        real_pair
                            .as_ref()
                            .map(|(l, r)| (l.as_slice(), r.as_slice())),
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
                        &ctx.device,
                        &ctx.queue,
                        &mut encoder,
                        [0.0, 0.0, 1.0, 1.0],
                        &polyline,
                        0.0,
                        dt,
                        &[],
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
                ViewMode::Scope | ViewMode::SpectrumEmber | ViewMode::Goniometer => {
                    ember.draw(&mut pass)
                }
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
fn top_peaks(peak: &[f32], freqs: &[f32], view: &CellView, n: usize) -> Vec<(usize, f32, f32)> {
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
        let too_close = picked
            .iter()
            .any(|&(_, f, _)| (cand.1.max(1e-6) / f.max(1e-6)).log2().abs() < EXCLUSION_OCTAVES);
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
        egui::pos2(p0.x, p0.y - 2.0),
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
            egui::pos2(p.x, p.y - 2.0),
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
            egui::pos2(rect.left() + 4.0, block_top + (i + 1) as f32 * row_h),
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
    sine_phase: &mut f32,
    sample_rate: f32,
    sine_freq_hz: f32,
    window_s: f32,
    y_gain: f32,
    dt: f32,
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

/// Number of x-axis columns the spectrum is aggregated into before
/// deposition. Sized to match the ember substrate width so adjacent
/// columns fall on adjacent pixels and the LineList renderer doesn't pile
/// dozens of FFT bins into the same column. Smaller values make the trace
/// feel chunkier without changing the underlying signal interpretation.
const EMBER_SPECTRUM_COLS: usize = 512;

/// Per-vertex deposit weight for the live ember spectrum trace. Held
/// envelopes (peak/min) recede behind it at the lower weights below so the
/// reader can tell "now" from "held" without introducing a second colour.
const EMBER_LIVE_W: f32 = 1.0;
/// Peak-hold envelope weight — brighter-steady than min, dimmer than live.
const EMBER_PEAK_W: f32 = 0.6;
/// Min-hold envelope weight — the dimmest of the three so the noise-floor
/// outline sits furthest behind the live trace.
const EMBER_MIN_W: f32 = 0.45;

/// Baseline of the ember spectrum trace in deposit space. The substrate
/// inverts y between deposit and display (y=0 → screen top, y=1 → bottom),
/// so a baseline near y=1 anchors the trace at the cell *floor*. The trace
/// rises toward the cell top as level approaches `db_max`.
const EMBER_TRACE_BASE: f32 = 0.95;
/// Fraction of the cell the trace deflects across at full scale. A single
/// baseline-anchored line uses the whole cell for dynamic range (the old
/// mirror gave each of its two envelopes only half).
const EMBER_TRACE_FULL: f32 = 0.90;

/// Map an ember trace's cell-local y (0 = cell top, 1 = cell bottom; the
/// floor-anchored baseline sits at `EMBER_TRACE_BASE`) into canvas space
/// for a cell occupying `[cell_y, cell_y + cell_h]`. The ember substrate
/// inverts y between deposit and display, so the baseline must be flipped
/// to land at each cell's own bottom edge.
///
/// Both the Grid and Single arms route their y through this so the floor
/// baseline lands at the same cell edge in every layout. The Single arm
/// formerly pushed raw cell-local y (no flip), rendering the trace
/// mirrored versus Grid — which read as a second, inverted trace on a
/// mono channel (#148 / #153). A full-canvas Single cell is `(0.0, 1.0)`,
/// for which this reduces to `1.0 - ay`.
#[inline]
fn ember_cell_to_canvas_y(ay: f32, cell_y: f32, cell_h: f32) -> f32 {
    1.0 - cell_y - ay * cell_h
}

/// Transform a cell-local ember LineList (`[x, y, w]` in `[0,1]`) into
/// canvas space for `cell`, applying the per-cell image zoom and **clamping
/// every emitted vertex into the cell rect** so a zoomed trace cannot deposit
/// outside its own cell — the cross-cell bleed half of the grid-leak
/// regression (#153 leak #2). Segments with both endpoints outside the cell
/// frame are dropped; a segment straddling the edge keeps its inside endpoint
/// and clamps the straggler to the boundary. Every appended vertex therefore
/// satisfies `x ∈ [cell.x, cell.x+cell.w]` and `y ∈ [1-cell.y-cell.h,
/// 1-cell.y]`. `focus_w` scales the deposit weight (focused cell 1.0, others
/// dimmer). The y-flip (`ember_cell_to_canvas_y`) anchors the baseline at the
/// cell floor in every layout.
fn ember_pack_cell(
    out: &mut Vec<[f32; 3]>,
    local: &[[f32; 3]],
    cell: &layout::CellRect,
    view: &CellView,
    focus_w: f32,
) {
    let zx = view.zoom_x;
    let zy = view.zoom_y;
    let zf = view.zoom.max(1e-3);
    out.reserve(local.len());
    for chunk in local.chunks_exact(2) {
        let a = &chunk[0];
        let b = &chunk[1];
        let ax = zx + (a[0] - zx) * zf;
        let ay = zy + (a[1] - zy) * zf;
        let bx = zx + (b[0] - zx) * zf;
        let by = zy + (b[1] - zy) * zf;
        let in_bounds = |x: f32, y: f32| (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y);
        if !in_bounds(ax, ay) && !in_bounds(bx, by) {
            continue;
        }
        let cl = |v: f32| v.clamp(0.0, 1.0);
        let (ax, ay, bx, by) = (cl(ax), cl(ay), cl(bx), cl(by));
        out.push([
            cell.x + ax * cell.w,
            ember_cell_to_canvas_y(ay, cell.y, cell.h),
            a[2] * focus_w,
        ]);
        out.push([
            cell.x + bx * cell.w,
            ember_cell_to_canvas_y(by, cell.y, cell.h),
            b[2] * focus_w,
        ]);
    }
}

/// Hash the ember layout configuration into a generation key. The key changes
/// iff the active view, layout mode, or any visible cell's channel/geometry
/// changes — i.e. on a Single↔Grid swap, a grid page turn, a channel-set
/// change, a resize, or a view switch. `redraw` clears the substrate whenever
/// this differs from the previous frame's key so a stale configuration's
/// phosphor cannot bleed under the new one (#153 leak #1/#3). Persistence
/// within one configuration is preserved: an unchanged key never clears.
fn ember_layout_generation(
    view_mode: ViewMode,
    layout: LayoutMode,
    cells: &[layout::CellRect],
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::mem::discriminant(&view_mode).hash(&mut h);
    std::mem::discriminant(&layout).hash(&mut h);
    (cells.len() as u64).hash(&mut h);
    for cell in cells {
        (cell.channel as u64).hash(&mut h);
        // f32 has no Hash; fold the bit patterns so geometry changes (resize,
        // page, layout) move the key.
        cell.x.to_bits().hash(&mut h);
        cell.y.to_bits().hash(&mut h);
        cell.w.to_bits().hash(&mut h);
        cell.h.to_bits().hash(&mut h);
    }
    h.finish()
}

/// Faint, constant deposit weight for an idle cell's hitbox marker. Dimmer
/// than every live deposit (live 1.0 / peak 0.6 / min 0.45) so the marker
/// recedes into the void and never competes with a real ember.
const EMBER_EMPTY_W: f32 = 0.35;
/// Idle-bracket leg length as a fraction of the cell's smaller dimension.
const EMBER_IDLE_TICK_FRAC: f32 = 0.08;
/// Upper bound on a bracket leg, equal to a quarter-canvas grid cell's leg.
/// A 2×2 grid (the coarsest grid, `cols = ceil(sqrt(n))`) has 0.5-wide cells,
/// so its leg is `0.5 * TICK_FRAC` — that is the reference mark size. The cap
/// leaves every real grid cell untouched (all ≤ 0.5 dim) and only clamps a
/// full-canvas (1×1) Single cell down to it instead of ballooning. Display-
/// only clamp — no surface-pixel detection, only the `cell.w` / `cell.h`
/// already in hand.
const EMBER_IDLE_TICK_MAX: f32 = 0.5 * EMBER_IDLE_TICK_FRAC;

/// Idle-cell hitbox marker: four dim corner brackets, no full-width edge.
///
/// An empty-but-connected channel must read as "clickable empty," never as a
/// measurement. A real silent channel renders a flat trace pinned at the
/// floor, so any spanning horizontal edge in the idle marker aliases that
/// exact state — the #148 / #153 two-line symptom reaching the idle path.
/// Four crop-mark corners frame the rectangle without depositing any edge
/// long enough to read as a trace (#156).
///
/// Legs are equal-length — `min(w, h) * TICK_FRAC`, capped at `TICK_MAX` — so
/// every corner is a square bracket at any cell aspect (per-axis legs skewed
/// the L in non-square Grid cells) and a full-canvas Single cell shows the
/// same mark size as a grid cell. Returns a LineList (consecutive vertex
/// pairs) carrying `weight` as the ember deposit.
fn ember_idle_brackets(
    cell_x: f32,
    cell_y: f32,
    cell_w: f32,
    cell_h: f32,
    edge_inset: f32,
    weight: f32,
) -> Vec<[f32; 3]> {
    let inset_x = cell_w * edge_inset;
    let x0 = cell_x + inset_x;
    let x1 = cell_x + cell_w - inset_x;
    let leg = (cell_w.min(cell_h) * EMBER_IDLE_TICK_FRAC).min(EMBER_IDLE_TICK_MAX);
    // canvas y decreases as cell-local y rises (see `ember_cell_to_canvas_y`),
    // so top legs drop toward the interior (−leg) and bottom legs rise toward
    // it (+leg). Both leg arms use the same `leg` so corners read square.
    let y_top = ember_cell_to_canvas_y(edge_inset, cell_y, cell_h);
    let y_bot = ember_cell_to_canvas_y(1.0 - edge_inset, cell_y, cell_h);
    let tl = [x0, y_top, weight];
    let tr = [x1, y_top, weight];
    let bl = [x0, y_bot, weight];
    let br = [x1, y_bot, weight];
    vec![
        tl,
        [x0 + leg, y_top, weight],
        tl,
        [x0, y_top - leg, weight], // top-left
        tr,
        [x1 - leg, y_top, weight],
        tr,
        [x1, y_top - leg, weight], // top-right
        bl,
        [x0 + leg, y_bot, weight],
        bl,
        [x0, y_bot + leg, weight], // bottom-left
        br,
        [x1 - leg, y_bot, weight],
        br,
        [x1, y_bot + leg, weight], // bottom-right
    ]
}

/// Per-column centre frequencies for the log-spaced display grid, matching
/// `ac_core::visualize::aggregate`'s internal column geometry exactly (same
/// `f_min * (f_max/f_min)^((i+0.5)/n)` formula) so a virtual cell's
/// synthesized freq axis lines up 1:1 with the magnitudes
/// `samples_on_axis_to_columns` produces for it. Returns an empty vec for
/// degenerate input, mirroring `samples_on_axis_to_columns`'s own guard.
// Negated `>` comparisons are intentional NaN-aware guards (matches
// aggregate.rs's identical idiom).
#[allow(clippy::neg_cmp_op_on_partial_ord)]
fn column_centre_freqs_f32(f_min: f32, f_max: f32, n_columns: usize) -> Vec<f32> {
    if n_columns == 0 || !(f_min > 0.0) || !(f_max > f_min) {
        return Vec::new();
    }
    let log_ratio = (f_max / f_min).ln();
    let n = n_columns as f32;
    (0..n_columns)
        .map(|i| f_min * (log_ratio * (i as f32 + 0.5) / n).exp())
        .collect()
}

/// Coherence gate for the transfer-magnitude display (P2): bins the same way
/// `samples_on_axis_to_columns` bins magnitude, over the same log-column
/// grid, and NaNs out any column whose γ² falls below `theme::PHASE_COH_GATE`
/// — incoherent bands become a true gap instead of rendering with the same
/// visual authority as valid data.
///
/// A column with ≥1 directly-contributing coherence sample uses their
/// median (the same noise-robust order statistic `column_median` gives the
/// ember trace). A column with none — the same LF-end sparsity
/// `samples_on_axis_to_columns` handles by interpolating magnitude — falls
/// back to the same two-neighbour interpolation there, so gating covers the
/// display exactly as completely as the magnitude curve does: a display
/// with far more columns than the transfer measurement has bins (common —
/// `DEFAULT_WIRE_COLUMNS` vs. a modest transfer FFT size) would otherwise
/// leave most columns spuriously ungated for want of a bin landing exactly
/// inside them.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
fn gate_transfer_columns_by_coherence(
    mags: &mut [f32],
    freqs: &[f32],
    coherence: &[f32],
    f_min: f32,
    f_max: f32,
) {
    let n_columns = mags.len();
    let b = freqs.len().min(coherence.len());
    if n_columns == 0 || b == 0 || !(f_min > 0.0) || !(f_max > f_min) {
        return;
    }
    let log_ratio = (f_max / f_min).ln();
    let n = n_columns as f32;
    let col_lo = |i: usize| f_min * (log_ratio * i as f32 / n).exp();
    let col_centre = |i: usize| f_min * (log_ratio * (i as f32 + 0.5) / n).exp();

    let mut k = 0usize;
    let mut samples: Vec<f32> = Vec::new();
    for (i, mag) in mags.iter_mut().enumerate() {
        let lo = col_lo(i);
        let hi = col_lo(i + 1);
        while k < b && freqs[k] < lo {
            k += 1;
        }
        samples.clear();
        let mut j = k;
        while j < b && freqs[j] < hi {
            if coherence[j].is_finite() {
                samples.push(coherence[j]);
            }
            j += 1;
        }
        let gated_value = column_median(&mut samples).or_else(|| {
            let c = col_centre(i);
            let above = k.min(b - 1);
            let below = above.saturating_sub(1);
            if below == above {
                return coherence[below].is_finite().then_some(coherence[below]);
            }
            let f_below = freqs[below];
            let f_above = freqs[above];
            if !(f_below > 0.0) || !(f_above > f_below) {
                return None;
            }
            if !coherence[below].is_finite() || !coherence[above].is_finite() {
                return None;
            }
            let t = if c <= f_below {
                0.0
            } else if c >= f_above {
                1.0
            } else {
                let lb = f_below.log10();
                let la = f_above.log10();
                (c.log10() - lb) / (la - lb)
            };
            Some(coherence[below] * (1.0 - t) + coherence[above] * t)
        });
        if let Some(g) = gated_value {
            if g < theme::PHASE_COH_GATE {
                *mag = f32::NAN;
            }
        }
    }
}

/// Reduce a column of dB magnitudes to a single noise-robust level: the
/// lower-middle element of the sorted slice (`samples[(len - 1) / 2]`). This
/// is the median as an *order statistic* — it stays on a measured bin and is
/// deliberately NOT the mean of the two central values on even-length input,
/// which would synthesise an unmeasured level and re-introduce the kind of
/// excursion #158 removes. Kept a dB-domain order statistic, not a
/// power/RMS mean: a power mean would re-bias the level upward toward the
/// loud bins and partially reintroduce the upper-envelope symptom.
///
/// Returns `None` on an empty slice so the caller breaks the polyline (the
/// "no signal here" gap). Sorts `samples` in place; NaN-safe via an
/// `Ordering::Equal` fallback in `partial_cmp`.
pub(crate) fn column_median(samples: &mut [f32]) -> Option<f32> {
    if samples.is_empty() {
        return None;
    }
    samples.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(samples[(samples.len() - 1) / 2])
}

/// `(freqs, mags)` → single baseline-anchored ember LineList. Logarithmic
/// frequency axis on x; magnitude renders as one line growing from a
/// baseline at the cell floor (`EMBER_TRACE_BASE`) up toward the cell top
/// as the bin's normalised dB rises. A mono channel is one signal, so it
/// reads as one curve — this replaces the former mirror-about-y=0.5
/// envelope pair, which falsely implied a stereo/bipolar display. Bins
/// below `db_min` break the polyline so the trace disappears off-screen
/// rather than pinning a glowing baseline; bins above `db_max` clamp to
/// the top edge.
///
/// `weight` is the per-vertex deposit weight: live traces pass
/// `EMBER_LIVE_W`, held peak/min envelopes pass the lower weights so they
/// recede behind the live ember.
///
/// Bins are first aggregated by **per-column median** into
/// `EMBER_SPECTRUM_COLS` log-spaced columns. Without column-binning, linear
/// FFT output (~11.7 Hz/bin at 96 kHz / N=8192) collides ~15 bins per pixel
/// in the top decade of a log x-axis, producing visible moiré/aliasing in
/// the rendered trace. The statistic is the median, not max (#158): on an
/// unaveraged spectrum each column holds many bins carrying several dB of
/// per-bin noise, and max latches onto each column's noisiest bin so the
/// trace rides the *upper noise envelope* — a positively-biased estimate
/// that diverges from the true level toward HF (more bins per column) and,
/// held by the long SpectrumEmber τ_p, reads as a phantom second trace. The
/// median is an unbiased central estimate, so the line tracks the true
/// spectral level. Virtual (transfer) channels' frames are *also* already
/// log-spaced and pre-aggregated by this point — the virtual-snapshot
/// `DisplayFrame` construction in this module runs them through
/// `samples_on_axis_to_columns` before this function ever sees them (#162,
/// #163) — so bins-per-column stays low here too and the moiré artefact
/// this median statistic exists to fix doesn't reappear on the transfer
/// path. (A prior version of this comment claimed this was true of all
/// transfer frames without that aggregation step actually happening for the
/// live magnitude trace — it wasn't; #163 fixed the scrambled-axis bug that
/// exposed the gap between the claim and the code.)
fn build_ember_spectrum_trace(
    freqs: &[f32],
    mags: &[f32],
    view: &CellView,
    weight: f32,
) -> Vec<[f32; 3]> {
    let log_min = view.freq_min.max(1.0).log10();
    let log_max = view.freq_max.max(view.freq_min * 1.001).log10();
    let span_f = (log_max - log_min).max(1e-6);
    let span_db = (view.db_max - view.db_min).max(1e-3);
    let n_cols = EMBER_SPECTRUM_COLS;
    let mut col_samples: Vec<Vec<f32>> = vec![Vec::new(); n_cols];
    let n = freqs.len().min(mags.len());
    for i in 0..n {
        let f = freqs[i];
        let mag = mags[i];
        if !f.is_finite()
            || f < view.freq_min
            || f > view.freq_max
            || !mag.is_finite()
            || mag < view.db_min
        {
            continue;
        }
        let xn = (f.max(1.0).log10() - log_min) / span_f;
        if !(0.0..=1.0).contains(&xn) {
            continue;
        }
        let col = ((xn * n_cols as f32) as usize).min(n_cols - 1);
        col_samples[col].push(mag);
    }

    let mut pairs = Vec::with_capacity(n_cols * 2);
    let mut prev: Option<[f32; 3]> = None;
    #[allow(clippy::needless_range_loop)]
    for col in 0..n_cols {
        // Empty column → no bin landed here → break the polyline (preserves
        // the dB-floor "no signal here" gap; never bridged).
        let mag = match column_median(&mut col_samples[col]) {
            Some(m) => m,
            None => {
                prev = None;
                continue;
            }
        };
        let x = (col as f32 + 0.5) / n_cols as f32;
        let n_mag = ((mag - view.db_min) / span_db).clamp(0.0, 1.0);
        let y = EMBER_TRACE_BASE - EMBER_TRACE_FULL * n_mag;
        let cur = [x, y, weight];
        if let Some(prev_pt) = prev {
            pairs.push(prev_pt);
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

/// Resolve the (L, R) stereo pair for Goniometer from its live
/// (active, active+1) pair. The pair carries `meas` and `ref_ch`; we
/// map them to (L = ref_ch, R = meas) — stereo phase scope has no
/// semantic asymmetry, any consistent labelling reads correctly.
///
/// `pair = None` (no real neighbour channel, e.g. last/mono channel) →
/// `(NoTransferPair, None)`. The caller falls back to the synthetic
/// carrier and the overlay caption hints at needing a stereo pair.
///
/// Returns:
/// - `(Real { l, r }, Some((l_samples, r_samples)))` when both
///   channels have recent matching scope frames.
/// - `(NoTransferPair, None)` when there's no pair to resolve.
/// - `(NotStreamingYet { l, r }, None)` when there's a pair but scope
///   frames haven't arrived yet (cold start, or daemon stopped
///   streaming).
/// - `(NoAudio, None)` when there's no scope store at all
///   (synthetic / pre-connect).
#[allow(clippy::type_complexity)]
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
            (
                StereoStatus::Real {
                    l: phys_l,
                    r: phys_r,
                },
                Some((sl, sr_buf)),
            )
        }
        _ => (
            StereoStatus::NotStreamingYet {
                l: phys_l,
                r: phys_r,
            },
            None,
        ),
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

    // ---- #163 virtual transfer channel DisplayFrame construction ----

    /// A5: a contiguous low-coherence band gates its columns to NaN; columns
    /// backed by high-coherence bins stay finite. Mirrors the exact call the
    /// virtual-snapshot block makes (`f_min`/`f_max` match the daemon's
    /// monitor-frame convention).
    #[test]
    fn coherence_gate_nans_low_coherence_band_only() {
        let sr = 48_000.0_f32;
        let n = 4096;
        let len = n / 2 + 1;
        let df = sr / n as f32;
        let freqs: Vec<f32> = (0..len).map(|k| k as f32 * df).collect();
        let mags = vec![-20.0_f32; len];
        let coherence: Vec<f32> = freqs
            .iter()
            .map(|&f| {
                if (1_000.0..4_000.0).contains(&f) {
                    0.1
                } else {
                    0.95
                }
            })
            .collect();

        let f_min = theme::DEFAULT_FREQ_MIN;
        let f_max = (sr / 2.0).max(f_min + 1.0);
        let n_columns = ac_core::visualize::aggregate::DEFAULT_WIRE_COLUMNS;
        let mut cols = ac_core::visualize::aggregate::samples_on_axis_to_columns(
            &freqs, &mags, f_min, f_max, n_columns,
        );
        assert!(cols.iter().all(|v| v.is_finite()), "no gating applied yet");
        gate_transfer_columns_by_coherence(&mut cols, &freqs, &coherence, f_min, f_max);

        let centres = column_centre_freqs_f32(f_min, f_max, n_columns);
        let mut saw_gated = false;
        let mut saw_ungated = false;
        for (c, f) in cols.iter().zip(centres.iter()) {
            // Stay clear of the exact 1k/4k boundary columns, which can mix
            // gated and ungated bins (same edge-quantization caveat as the
            // ember gap test above).
            if (1_300.0..3_700.0).contains(f) {
                assert!(c.is_nan(), "column at {f} Hz should be gated, got {c}");
                saw_gated = true;
            } else if *f < 700.0 || *f > 4_700.0 {
                assert!(
                    c.is_finite(),
                    "column at {f} Hz should not be gated, got {c}"
                );
                saw_ungated = true;
            }
        }
        assert!(saw_gated && saw_ungated, "test band coverage too narrow");
    }

    /// Sparse-column fallback (no bin lands directly in the column) uses
    /// the same nearest-neighbour interpolation `samples_on_axis_to_columns`
    /// uses for magnitude — including its flat-extrapolation-at-the-edges
    /// behaviour — so a uniformly incoherent measurement gates the *entire*
    /// display, not just the columns a bin happens to land in.
    #[test]
    fn coherence_gate_sparse_fallback_matches_magnitude_aggregator_convention() {
        let freqs = vec![100.0_f32, 200.0, 300.0];
        let coherence = vec![0.05_f32, 0.05, 0.05]; // uniformly incoherent
        let f_min = 20.0_f32;
        let f_max = 20_000.0_f32;
        let n_columns = 64;
        let mut cols = vec![-10.0_f32; n_columns];
        gate_transfer_columns_by_coherence(&mut cols, &freqs, &coherence, f_min, f_max);
        assert!(
            cols.iter().all(|v| v.is_nan()),
            "uniformly incoherent input should gate every column, same as \
             `spectrum_to_columns`'s flat edge-extrapolation for magnitude"
        );
    }

    /// A2 (P1 foundation, integration-level): the virtual-snapshot block's
    /// two building blocks — `samples_on_axis_to_columns` and
    /// `column_centre_freqs_f32` — must describe the *same* column grid, so
    /// `spectrum[i]` and `freqs[i]` in the resulting `DisplayFrame` agree
    /// with each other the way they always have for real channels. This is
    /// the actual P1 regression: before #163, the uploaded spectrum values
    /// were on a linear axis while everything else in the cell (hover,
    /// gridlines, this freqs array) assumed log columns.
    #[test]
    fn virtual_frame_magnitude_and_freqs_describe_the_same_grid() {
        let sr = 48_000.0_f32;
        let n = 8192;
        let len = n / 2 + 1;
        let df = sr / n as f32;
        let freqs_in: Vec<f32> = (0..len).map(|k| k as f32 * df).collect();

        for &f0 in &[100.0_f32, 1_000.0, 10_000.0] {
            let bin = (f0 / df).round() as usize;
            let actual_f0 = bin as f32 * df;
            let mut mags_in = vec![-60.0_f32; len];
            mags_in[bin] = -10.0;

            let f_min = theme::DEFAULT_FREQ_MIN;
            let f_max = (sr / 2.0).max(f_min + 1.0);
            let n_columns = ac_core::visualize::aggregate::DEFAULT_WIRE_COLUMNS;
            let cols = ac_core::visualize::aggregate::samples_on_axis_to_columns(
                &freqs_in, &mags_in, f_min, f_max, n_columns,
            );
            let centres = column_centre_freqs_f32(f_min, f_max, n_columns);
            assert_eq!(cols.len(), centres.len());

            let (max_i, _) = cols
                .iter()
                .enumerate()
                .filter(|(_, v)| v.is_finite())
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .expect("at least one finite column");
            let col_width = {
                let log_ratio = (f_max / f_min).ln();
                let lo = f_min * (log_ratio * max_i as f32 / n_columns as f32).exp();
                let hi = f_min * (log_ratio * (max_i as f32 + 1.0) / n_columns as f32).exp();
                hi - lo
            };
            assert!(
                (centres[max_i] - actual_f0).abs() <= col_width.max(df),
                "f0={f0}: freqs[{max_i}]={} not within one column of the tone at {actual_f0}",
                centres[max_i],
            );
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
        let freqs: Vec<f32> = (0..11).map(|i| 50.0 * (2.0_f32).powi(i)).collect();
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
            &mut pl,
            &mut pr,
            48_000.0,
            true,
            EMBER_GONIO_AMP,
            1.0 / 60.0,
            None,
        );
        // Two consecutive vertices form one connected segment, so the
        // pair count is always even and every emitted vertex must sit
        // inside the substrate viewport.
        assert!(
            pairs.len().is_multiple_of(2),
            "LineList vertices must be even"
        );
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
        let pairs =
            build_goniometer_polyline(&mut pl, &mut pr, sr, true, EMBER_GONIO_AMP, dt, None);
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
            &mut pl,
            &mut pr,
            48_000.0,
            false,
            EMBER_GONIO_AMP,
            0.05,
            None,
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
            &mut pl,
            &mut pr,
            48_000.0,
            true,
            EMBER_GONIO_AMP,
            1.0 / 60.0,
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
            &mut pl,
            &mut pr,
            48_000.0,
            true,
            EMBER_GONIO_AMP,
            1.0 / 60.0,
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
            &mut pl,
            &mut pr,
            48_000.0,
            true,
            EMBER_GONIO_AMP,
            1.0 / 60.0,
            Some((&l, &r)),
        );
        assert_eq!(
            pl, pl_before,
            "carrier_phase must stay frozen in real branch"
        );
        assert_eq!(
            pr, pr_before,
            "phase_offset must stay frozen in real branch"
        );
    }

    // ---- resolve_stereo_pair (TransferPair-driven) ----

    fn scope_frame(
        channel: u32,
        frame_idx: u64,
        samples: Vec<f32>,
    ) -> crate::data::types::ScopeFrame {
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
        // l = ref_ch, r = meas (matches the resolver's docstring).
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

    // ---- Ember single-trace invariant (#148 / #153) ----
    //
    // A mono channel is one signal, so it must render as exactly one
    // floor-anchored ember curve — never a second, vertically-mirrored
    // trace. These guard the two layers that previously reintroduced the
    // mirror: the CPU geometry builder (`build_ember_spectrum_trace`) and
    // the Single/Grid canvas-flip parity (`ember_cell_to_canvas_y`).

    /// A realistic mono tone yields one connected, floor-anchored polyline:
    /// every vertex sits in the `[BASE-FULL, BASE]` band, all carry the live
    /// deposit weight, and consecutive LineList segments share endpoints
    /// (one polyline, not two). Asserted with peak/min hold OFF — held
    /// envelopes are #149-owned behaviour and are not part of this builder
    /// call.
    #[test]
    fn ember_mono_yields_single_floor_anchored_trace() {
        // 1 kHz tone over a ~-94 dBu noise floor, linear daemon bins.
        let freqs: Vec<f32> = (0..512).map(|i| 20.0 + i as f32 * 46.0).collect();
        let mut mags = vec![-94.0f32; 512];
        // 1 kHz lands near index (1000 - 20) / 46 ≈ 21.
        mags[20] = -28.0;
        mags[21] = -10.0;
        mags[22] = -28.0;
        let v = view(20.0, 24_000.0);
        let pairs = build_ember_spectrum_trace(&freqs, &mags, &v, EMBER_LIVE_W);
        assert!(!pairs.is_empty());

        let lo = EMBER_TRACE_BASE - EMBER_TRACE_FULL; // 0.05
        let hi = EMBER_TRACE_BASE; // 0.95
        for [_, y, w] in &pairs {
            assert!(
                *y >= lo - 1e-4 && *y <= hi + 1e-4,
                "vertex y = {y} outside floor band [{lo}, {hi}] — reflected/mirrored trace",
            );
            assert!(
                (*w - EMBER_LIVE_W).abs() < 1e-6,
                "unexpected deposit weight {w} (expected live weight only, hold off)",
            );
        }

        // Single trace = at most one y per x-column. A mirror would place a
        // second, reflected vertex at the same x (`y` and its reflection),
        // so any column carrying two distinct y values is the bug. (The
        // polyline legitimately breaks across empty log-columns — that is a
        // horizontal gap, not a second trace, so we check per-x uniqueness
        // rather than full connectivity.)
        assert_eq!(pairs.len() % 2, 0);
        for (i, a) in pairs.iter().enumerate() {
            for b in pairs.iter().skip(i + 1) {
                if (a[0] - b[0]).abs() < 1e-4 {
                    assert!(
                        (a[1] - b[1]).abs() < 1e-4,
                        "two vertices at x={} with different y ({} vs {}) — mirrored trace",
                        a[0],
                        a[1],
                        b[1],
                    );
                }
            }
        }
    }

    /// #163 (P2 coherence gate / gap rendering): `build_ember_spectrum_trace`
    /// already drops non-finite bins before they reach `column_median`
    /// (`!mag.is_finite()` in its per-bin filter), so an all-NaN column has
    /// no samples, `column_median` returns `None`, and the polyline breaks
    /// there — this verifies that behaviour holds for the actual gap
    /// sentinel the coherence-gated transfer path emits (`f32::NAN`), not
    /// just for the mirror/median regressions the other tests target.
    #[test]
    fn ember_breaks_polyline_on_nan_gap_columns() {
        // Two isolated tones either side of a NaN-gated band spanning
        // ~1..4 kHz — every bin in that band is NaN, as
        // `gate_transfer_columns_by_coherence` would leave them after a
        // sustained low-coherence run.
        let n = 2048;
        let freqs: Vec<f32> = (0..n).map(|i| 20.0 + i as f32 * 11.7).collect();
        let mut mags = vec![-90.0f32; n];
        for (i, f) in freqs.iter().enumerate() {
            if (500.0..800.0).contains(f) {
                mags[i] = -20.0; // tone below the gap
            } else if (1_000.0..4_000.0).contains(f) {
                mags[i] = f32::NAN; // gated band
            } else if (8_000.0..8_300.0).contains(f) {
                mags[i] = -20.0; // tone above the gap
            }
        }
        let v = view(20.0, 24_000.0);
        let pairs = build_ember_spectrum_trace(&freqs, &mags, &v, EMBER_LIVE_W);
        assert!(!pairs.is_empty());

        let log_min = v.freq_min.max(1.0).log10();
        let log_max = v.freq_max.log10();
        let span = log_max - log_min;
        let x_of = |f: f32| (f.log10() - log_min) / span;
        let gap_lo = x_of(1_000.0);
        let gap_hi = x_of(4_000.0);
        // A column straddling the 1 kHz/4 kHz boundary can still hold one
        // valid bin from just outside the NaN range, so check an inner
        // sub-band clear of column-quantization edge effects rather than
        // the exact NaN boundary.
        let safe_lo = x_of(1_300.0);
        let safe_hi = x_of(3_700.0);

        // No vertex should land inside the gap's safe inner x-range.
        for [x, ..] in &pairs {
            assert!(
                *x < safe_lo || *x > safe_hi,
                "vertex at x={x} falls inside the NaN-gated band [{safe_lo}, {safe_hi}]"
            );
        }
        // And no segment should bridge straight across it (a segment with
        // one endpoint below the gap and the other above it would render as
        // a connecting line through supposedly-gapped columns).
        for pair in pairs.chunks(2) {
            let (a, b) = (pair[0], pair[1]);
            let bridges = (a[0] < gap_lo && b[0] > gap_hi) || (b[0] < gap_lo && a[0] > gap_hi);
            assert!(
                !bridges,
                "segment {a:?} -> {b:?} bridges across the NaN gap"
            );
        }
    }

    // ---- Per-column median aggregation (#158) ----
    //
    // `build_ember_spectrum_trace` aggregates each log-column by median, not
    // per-column max. Max latches onto the noisiest bin and the trace rides
    // the upper noise envelope (a positively-biased, HF-divergent phantom
    // band); median is an unbiased central estimate that tracks true level.

    #[test]
    fn column_median_empty_is_none() {
        let mut xs: Vec<f32> = Vec::new();
        assert_eq!(column_median(&mut xs), None);
    }

    #[test]
    fn column_median_single_element() {
        let mut xs = vec![-42.0f32];
        assert_eq!(column_median(&mut xs), Some(-42.0));
    }

    #[test]
    fn column_median_even_len_returns_lower_middle() {
        // Even count: lower-middle element, NOT the mean of the two centre
        // values. sorted centre pair = (-58, -56); lower-middle = -58, the
        // mean would be -57 (an unmeasured level — the regression we forbid).
        let mut xs = vec![-54.0f32, -60.0, -56.0, -58.0];
        assert_eq!(column_median(&mut xs), Some(-58.0));
    }

    #[test]
    fn column_median_rejects_lone_outlier() {
        // A single loud spike (the max-build symptom) must not move the
        // median off the true level.
        let mut xs = vec![-60.0f32, -60.0, -60.0, -60.0, -10.0];
        assert_eq!(column_median(&mut xs), Some(-60.0));
    }

    /// Conformance: on a deterministic noisy spectrum the median trace sits
    /// meaningfully *below* the per-column max envelope (the old build) **and**
    /// tracks the noise-free reference within a tight tolerance. Both halves
    /// are required: offset-only could pass a trace biased low; truth-tracking
    /// alone could pass the max build on a quiet column. The metric is the
    /// vertical envelope offset (max-trace y minus median-trace y per column),
    /// NOT riser height — on a smooth envelope risers barely differ between
    /// max and median and would pass both builds, missing the bug.
    #[test]
    fn ember_median_trace_sits_below_max_envelope_and_tracks_truth() {
        // Flat -60 dB reference with ±4 dB deterministic LCG per-bin noise
        // over the full -120..0 dB window. Linear daemon bins (5.85 Hz) so HF
        // log-columns hold more bins — where the max bias is largest.
        let mut state: u32 = 0x1234_5678; // fixed seed, no rng dep
        let mut lcg = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((state >> 8) as f32 / 16_777_216.0) * 8.0 - 4.0
        };
        let truth = -60.0f32;
        let freqs: Vec<f32> = (0..4096).map(|i| 20.0 + i as f32 * 5.85).collect();
        let mags: Vec<f32> = freqs.iter().map(|_| truth + lcg()).collect();
        let v = view(20.0, 24_000.0);

        let med = build_ember_spectrum_trace(&freqs, &mags, &v, EMBER_LIVE_W);
        assert!(!med.is_empty());

        // y for the noise-free level, mapped exactly as the builder does.
        let span_db = (v.db_max - v.db_min).max(1e-3);
        let ref_y =
            EMBER_TRACE_BASE - EMBER_TRACE_FULL * ((truth - v.db_min) / span_db).clamp(0.0, 1.0);

        // Reconstruct per-column bin count + max envelope, mirroring the
        // builder's binning so columns line up 1:1 with the median trace.
        let n_cols = EMBER_SPECTRUM_COLS;
        let log_min = v.freq_min.max(1.0).log10();
        let log_max = v.freq_max.max(v.freq_min * 1.001).log10();
        let span_f = (log_max - log_min).max(1e-6);
        let mut col_count = vec![0u32; n_cols];
        let mut col_max = vec![f32::NEG_INFINITY; n_cols];
        for (f, m) in freqs.iter().zip(mags.iter()) {
            if *f < v.freq_min || *f > v.freq_max || *m < v.db_min {
                continue;
            }
            let xn = (f.max(1.0).log10() - log_min) / span_f;
            if !(0.0..=1.0).contains(&xn) {
                continue;
            }
            let col = ((xn * n_cols as f32) as usize).min(n_cols - 1);
            col_count[col] += 1;
            if *m > col_max[col] {
                col_max[col] = *m;
            }
        }
        let to_y = |mag: f32| {
            let n_mag = ((mag - v.db_min) / span_db).clamp(0.0, 1.0);
            EMBER_TRACE_BASE - EMBER_TRACE_FULL * n_mag
        };

        // The artefact lives in many-bin columns (HF), where max latches onto
        // the noisiest of many bins. Single/few-bin LF columns inherit one
        // bin's full ±4 dB noise no matter the statistic — that is upstream
        // jitter (out-of-scope follow-up), not this bug — so the tight
        // truth-tracking and envelope-offset assertions target multi-bin
        // columns. `MIN_BINS = 12` keeps a robust per-column median sample.
        const MIN_BINS: u32 = 12;
        let mut strays = Vec::new(); // |median_y - ref_y| on multi-bin cols
        let mut offsets = Vec::new(); // max_y - median_y on multi-bin cols
        for [x, y, _] in &med {
            let col = ((x * n_cols as f32) as usize).min(n_cols - 1);
            if col_count[col] < MIN_BINS {
                continue;
            }
            strays.push((*y - ref_y).abs());
            offsets.push(to_y(col_max[col]) - *y); // negative: max sits above median
        }
        assert!(
            strays.len() > 16,
            "too few multi-bin columns ({}) to exercise the artefact",
            strays.len(),
        );

        // Half 1 — on multi-bin columns the median tracks the noise-free
        // reference within a bounded tolerance. The old max build provably
        // exceeded this (triage: max p95 ≈ 0.037, max ≈ 0.055 of full scale);
        // the median's p95 sits decisively under that ceiling.
        strays.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p95_stray = strays[(strays.len() * 95) / 100];
        assert!(
            p95_stray < 0.015,
            "median trace p95 stray {p95_stray} from noise-free reference exceeds 1.5% of \
             full scale (max build measured p95 ≈ 0.037 — median must beat it decisively)",
        );

        // Half 2 — and on those same columns the median sits meaningfully
        // *below* the max envelope (smaller y = higher trace, so the offset is
        // negative). Median over columns resists the few where they coincide.
        offsets.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let med_offset = offsets[offsets.len() / 2];
        assert!(
            med_offset < -0.01,
            "median trace not meaningfully below the max envelope (median offset {med_offset}); \
             the per-column max build would have exceeded the truth-tracking ceiling above",
        );
    }

    /// Single/Grid flip parity: the floor-anchored baseline lands at each
    /// cell's own bottom edge in every layout, never reflected above it.
    /// The Single arm formerly skipped the flip and rendered the trace
    /// mirrored versus Grid — the #148 / #153 second-trace symptom.
    #[test]
    fn ember_floor_anchors_to_cell_bottom_in_both_layouts() {
        // (cell_y, cell_h): full-canvas Single, plus two Grid cells.
        let cells = [(0.0f32, 1.0f32), (0.0, 0.5), (0.5, 0.5)];
        for &(cell_y, cell_h) in &cells {
            let floor = ember_cell_to_canvas_y(EMBER_TRACE_BASE, cell_y, cell_h);
            let bottom = ember_cell_to_canvas_y(1.0, cell_y, cell_h);
            let top = ember_cell_to_canvas_y(0.0, cell_y, cell_h);
            // Floor sits within one trace-floor offset (5 % of cell height)
            // of the bottom edge — anchored at the floor, not the middle or
            // a reflection.
            assert!(
                (floor - bottom).abs() <= (1.0 - EMBER_TRACE_BASE) * cell_h + 1e-6,
                "floor {floor} not anchored to bottom {bottom} (cell_y={cell_y}, h={cell_h})",
            );
            // And strictly between the two cell edges — never reflected past
            // either one.
            let (edge_lo, edge_hi) = (bottom.min(top), bottom.max(top));
            assert!(
                floor >= edge_lo - 1e-6 && floor <= edge_hi + 1e-6,
                "floor {floor} escaped cell edges [{edge_lo}, {edge_hi}]",
            );
        }
    }

    /// Idle marker = four corner brackets, never a spanning edge. A
    /// full-width horizontal edge at the floor aliases a real silent
    /// channel's flat trace — the #148 / #153 symptom reaching the idle path
    /// — so the idle LineList must contain no segment spanning the cell width
    /// at a single y, must frame all four corners, and must carry only the
    /// dim empty weight (#156). Covers a full-canvas Single cell and two
    /// non-square Grid cells.
    #[test]
    fn ember_idle_brackets_have_no_spanning_edge() {
        const INSET: f32 = 0.02;
        let cells = [
            (0.0f32, 0.0f32, 1.0f32, 1.0f32), // full-canvas Single
            (0.0, 0.0, 0.5, 0.25),            // wide, short Grid cell
            (0.5, 0.75, 0.5, 0.25),           // offset wide, short Grid cell
        ];
        for &(cx, cy, cw, ch) in &cells {
            let v = ember_idle_brackets(cx, cy, cw, ch, INSET, EMBER_EMPTY_W);
            // LineList: even vertex count, segments are consecutive pairs.
            assert_eq!(v.len() % 2, 0, "LineList must be vertex pairs");

            let inset_x = cw * INSET;
            let x0 = cx + inset_x;
            let x1 = cx + cw - inset_x;
            let span = (x1 - x0).abs();
            // No horizontal segment spans more than half the cell width: every
            // arm is a short bracket leg, so nothing reads as a floor/top edge.
            for seg in v.chunks_exact(2) {
                let (a, b) = (seg[0], seg[1]);
                let dy = (a[1] - b[1]).abs();
                let dx = (a[0] - b[0]).abs();
                assert!(
                    !(dy < 1e-6 && dx > 0.5 * span),
                    "idle segment {a:?}-{b:?} spans the cell width — reads as a trace",
                );
            }

            // All four corners are framed.
            let y_top = ember_cell_to_canvas_y(INSET, cy, ch);
            let y_bot = ember_cell_to_canvas_y(1.0 - INSET, cy, ch);
            for &(qx, qy) in &[(x0, y_top), (x1, y_top), (x0, y_bot), (x1, y_bot)] {
                assert!(
                    v.iter()
                        .any(|p| (p[0] - qx).abs() < 1e-5 && (p[1] - qy).abs() < 1e-5),
                    "missing corner bracket at ({qx}, {qy})",
                );
            }

            // Every vertex carries the dim empty weight, nothing brighter.
            for p in &v {
                assert!(
                    (p[2] - EMBER_EMPTY_W).abs() < 1e-6,
                    "idle vertex weight {} != EMBER_EMPTY_W",
                    p[2],
                );
            }
        }
    }

    // ---- Grid-leak isolation (#153) ----
    //
    // A grid cell's ember must stay inside its own rect: a zoomed trace may
    // not deposit into a neighbour (leak #2), and the layout-generation key
    // must change on any config switch so the substrate clears and a stale
    // configuration cannot linger under the new one (leak #1 / #3).

    /// A heavily-zoomed view pushes parts of the trace outside the cell-local
    /// frame; `ember_pack_cell` must clamp/drop so every canvas vertex stays
    /// within the cell rect — no bleed across the seam.
    #[test]
    fn ember_pack_cell_contains_zoomed_trace_in_cell() {
        let cell = layout::CellRect {
            channel: 1,
            x: 0.5,
            y: 0.5,
            w: 0.5,
            h: 0.5,
        };
        // Zoom 2× around the cell centre — spreads the trace past the cell-
        // local [0,1] frame on both axes (exercising the clamp/drop path)
        // while the central span still lands in-bounds (non-empty output).
        let view = CellView {
            zoom: 2.0,
            zoom_x: 0.5,
            zoom_y: 0.5,
            ..CellView::default()
        };
        // Connected polyline sweeping x across the frame, y oscillating about
        // the anchor so segments straddle every edge.
        let cols: Vec<[f32; 3]> = (0..64)
            .map(|i| {
                let t = i as f32 / 63.0;
                [t, 0.5 + 0.2 * (t * 6.0).sin(), EMBER_LIVE_W]
            })
            .collect();
        let mut local = Vec::new();
        for w in cols.windows(2) {
            local.push(w[0]);
            local.push(w[1]);
        }
        let mut out = Vec::new();
        ember_pack_cell(&mut out, &local, &cell, &view, 1.0);
        assert!(!out.is_empty(), "zoomed trace produced no vertices");

        let x_lo = cell.x;
        let x_hi = cell.x + cell.w;
        let y_lo = 1.0 - cell.y - cell.h;
        let y_hi = 1.0 - cell.y;
        for p in &out {
            assert!(
                p[0] >= x_lo - 1e-5 && p[0] <= x_hi + 1e-5,
                "vertex x {} escaped cell x-range [{x_lo}, {x_hi}] — cross-cell bleed",
                p[0],
            );
            assert!(
                p[1] >= y_lo - 1e-5 && p[1] <= y_hi + 1e-5,
                "vertex y {} escaped cell y-range [{y_lo}, {y_hi}] — cross-cell bleed",
                p[1],
            );
        }
    }

    /// Two adjacent grid cells, each packed with a trace zoomed toward the
    /// shared seam: no vertex from one cell may land in the other's rect.
    #[test]
    fn ember_grid_cells_do_not_bleed_into_neighbours() {
        let c0 = layout::CellRect {
            channel: 0,
            x: 0.0,
            y: 0.0,
            w: 0.5,
            h: 1.0,
        };
        let c1 = layout::CellRect {
            channel: 1,
            x: 0.5,
            y: 0.0,
            w: 0.5,
            h: 1.0,
        };
        // Zoom 2× about the cell centre pushes the trace's flanks past the
        // cell-local frame — the clamp must keep every vertex on its own side
        // of the x=0.5 seam.
        let view = CellView {
            zoom: 2.0,
            zoom_x: 0.5,
            zoom_y: 0.5,
            ..CellView::default()
        };
        let cols: Vec<[f32; 3]> = (0..64)
            .map(|i| {
                let t = i as f32 / 63.0;
                [t, 0.5 + 0.2 * (t * 6.0).sin(), EMBER_LIVE_W]
            })
            .collect();
        let local: Vec<[f32; 3]> = cols.windows(2).flat_map(|w| [w[0], w[1]]).collect();

        let mut o0 = Vec::new();
        ember_pack_cell(&mut o0, &local, &c0, &view, 1.0);
        let mut o1 = Vec::new();
        ember_pack_cell(&mut o1, &local, &c1, &view, 1.0);
        assert!(
            !o0.is_empty() && !o1.is_empty(),
            "expected vertices in both cells"
        );

        // Interior-of-rect test on x (the cells split horizontally at x=0.5).
        let interior_x =
            |p: &[f32; 3], c: &layout::CellRect| p[0] > c.x + 1e-4 && p[0] < c.x + c.w - 1e-4;
        for p in &o0 {
            assert!(!interior_x(p, &c1), "cell0 vertex {p:?} bled into cell1");
        }
        for p in &o1 {
            assert!(!interior_x(p, &c0), "cell1 vertex {p:?} bled into cell0");
        }
    }

    /// The layout-generation key must change on every config transition that
    /// requires a substrate clear, and stay constant for an unchanged config.
    #[test]
    fn ember_layout_generation_distinguishes_configs() {
        let cell = |channel: usize, x: f32, h: f32| layout::CellRect {
            channel,
            x,
            y: 0.0,
            w: 0.5,
            h,
        };
        let cells_a = vec![cell(0, 0.0, 1.0)];
        let cells_b = vec![cell(0, 0.0, 0.5), cell(1, 0.5, 0.5)];

        let base = ember_layout_generation(ViewMode::SpectrumEmber, LayoutMode::Single, &cells_a);
        // Identical config → identical key (persistence is preserved).
        assert_eq!(
            base,
            ember_layout_generation(ViewMode::SpectrumEmber, LayoutMode::Single, &cells_a),
        );
        // Layout swap Single↔Grid.
        assert_ne!(
            base,
            ember_layout_generation(ViewMode::SpectrumEmber, LayoutMode::Grid, &cells_a),
        );
        // View switch (e.g. scrolling Scope → static SpectrumEmber).
        assert_ne!(
            base,
            ember_layout_generation(ViewMode::Scope, LayoutMode::Single, &cells_a),
        );
        // Channel-set / cell-count change (grid paging).
        assert_ne!(
            base,
            ember_layout_generation(ViewMode::SpectrumEmber, LayoutMode::Single, &cells_b),
        );
        // Geometry change (resize) at the same channel set.
        assert_ne!(
            base,
            ember_layout_generation(
                ViewMode::SpectrumEmber,
                LayoutMode::Single,
                &[cell(0, 0.0, 0.5)],
            ),
        );
    }
}
