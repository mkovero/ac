//! Input handling — mouse drag/scroll, keyboard, selection. Methods land in
//! `impl App` here and are dispatched from the parent `app.rs` ApplicationHandler
//! (window_event / about_to_wait).

use std::time::{Duration, Instant};

use winit::dpi::PhysicalPosition;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::KeyCode;

use crate::data::smoothing;
use crate::data::types::{CellView, LayoutMode, TransferPair, ViewMode};
use crate::theme;
use crate::ui::layout;

use super::helpers::{
    auto_monitor_interval_ms, step_ladder, MONITOR_FFT_N_LADDER, MONITOR_INTERVAL_MAX_MS,
    MONITOR_INTERVAL_MIN_MS,
};
use super::App;

/// Slots in the Tab cycle. The cycle is *active-channel-typed*:
///
/// - On a real input channel: `SpectrumEmber` → `Waterfall` (FFT) →
///   `Cwt` (Morlet waterfall) → `SpectrumEmber`. CQT / Reassigned
///   waterfalls remain reachable only via `--view waterfall` plus a
///   manual `set_analysis_mode` — kept off the cycle to keep it short.
/// - On a virtual transfer channel: the 9 ember pair-derived slots
///   (`SpectrumEmber` + the 8 transfer / trajectory views).
#[derive(Copy, Clone, PartialEq)]
enum WSlot {
    SpectrumEmber,
    /// Waterfall view, FFT analysis mode. Real channels only.
    Waterfall,
    /// Waterfall view, Morlet CWT analysis mode. Real channels only.
    Cwt,
    Goniometer,
    IoTransfer,
    BodeMag,
    Coherence,
    BodePhase,
    GroupDelay,
    Nyquist,
    Ir,
}

/// §2 binding scroll-zoom rules. Pure decision function so the per-view
/// modifier mapping is unit-testable without spinning up `App`. Returns
/// `(zoom_freq, zoom_y, zoom_time)` — `zoom_y` means dB on spectrum-family
/// views, ignored on waterfall (where the time axis is the second knob).
///
/// - Spectrum / Bode / Phase / GroupDelay: plain = both axes, Shift = freq,
///   Ctrl = y (dB or signed-y).
/// - Waterfall: plain = freq + time, Shift = freq, Ctrl = time.
///
/// Ctrl+Shift is intercepted earlier as the dB-window pan, so this helper
/// is only called for the (none) / Shift / Ctrl combos.
pub(super) fn decide_zoom_axes(view: ViewMode, shift: bool, ctrl: bool) -> (bool, bool, bool) {
    if matches!(view, ViewMode::Waterfall) {
        match (shift, ctrl) {
            (false, false) => (true, false, true),
            (true, false) => (true, false, false),
            (false, true) => (false, false, true),
            (true, true) => (false, false, false),
        }
    } else {
        match (shift, ctrl) {
            (false, false) => (true, true, false),
            (true, false) => (true, false, false),
            (false, true) => (false, true, false),
            (true, true) => (false, false, false),
        }
    }
}

/// User-facing short label for a view, used in the "no zoom on <view>"
/// chip. Matches the keytip-strip naming from RC-8.
pub(super) fn view_label(view: ViewMode) -> &'static str {
    match view {
        ViewMode::Spectrum => "spectrum",
        ViewMode::Waterfall => "waterfall",
        ViewMode::Scope => "scope",
        ViewMode::SpectrumEmber => "spectrum",
        ViewMode::Goniometer => "goniometer",
        ViewMode::IoTransfer => "io-transfer",
        ViewMode::BodeMag => "bode-mag",
        ViewMode::Coherence => "coherence",
        ViewMode::BodePhase => "bode-phase",
        ViewMode::GroupDelay => "group-delay",
        ViewMode::Nyquist => "nyquist",
        ViewMode::Ir => "ir",
    }
}

#[derive(Clone)]
pub(super) struct DragState {
    pub(super) start: PhysicalPosition<f64>,
    pub(super) targets: Vec<usize>,
    pub(super) start_log_min: f32,
    pub(super) start_log_max: f32,
    pub(super) start_db_min: f32,
    pub(super) start_db_max: f32,
    pub(super) cell_w_px: f32,
    pub(super) cell_h_px: f32,
}

impl App {
    /// Scroll-to-resize handler, only active in Grid layout when the cursor
    /// sits outside any cell (the empty band around / between cells). Seeds
    /// from the current auto layout so the first tick is a continuous step
    /// from wherever the user currently sees, then pins `grid_page` into the
    /// new page range so the visible content doesn't jump off-screen.
    pub(super) fn adjust_grid_size(&mut self, scroll_y: f32) {
        let n = self.cell_views.len();
        if n == 0 {
            return;
        }
        let current = self.grid_cell_size.unwrap_or_else(|| {
            let cols = (n as f32).sqrt().ceil().max(1.0);
            1.0 / cols
        });
        // Scroll up (positive) = larger cells (fewer per page). Clamped so we
        // never produce zero-width cells or fewer than 1 col.
        let factor = 1.15_f32.powf(scroll_y);
        let new_size = (current * factor).clamp(1.0 / 8.0, 1.0);
        self.grid_cell_size = Some(new_size);
        let (cols, rows, _page_size, pages) =
            layout::grid_dims(n, self.grid_params());
        self.grid_page = self.grid_page.min(pages.saturating_sub(1));
        self.notify(&format!(
            "grid {}×{} · page {}/{}",
            cols,
            rows,
            self.grid_page + 1,
            pages,
        ));
    }

    /// Source-of-truth selection slice fed to `layout::compute`. In
    /// Compare layout the snapshot taken at C-press is authoritative —
    /// the live `selected` is reset by C / T so the workflow can start
    /// fresh, but Compare needs to keep painting the locked-in set.
    pub(super) fn layout_selection(&self) -> &[bool] {
        if matches!(self.config.layout, LayoutMode::Compare) {
            &self.compare_set
        } else {
            &self.selected
        }
    }

    /// Clear the live channel selection. Called by `T` and `Shift+C`
    /// after they "consume" the selection, so the user can start a
    /// fresh workflow without the previous set lingering as cell-
    /// border highlights.
    fn clear_selection(&mut self) {
        for s in self.selected.iter_mut() {
            *s = false;
        }
        self.selection_order.clear();
    }

    /// Step through the Tab views forward (or backward). The cycle is
    /// *active-channel-typed*:
    ///
    /// - Active on a real input channel → 3-slot cycle: SpectrumEmber
    ///   → Waterfall (FFT) → Cwt (Morlet waterfall) → SpectrumEmber.
    ///   The trajectory / transfer views are excluded here — they all
    ///   paint a stereo or transfer pair, which would silently swap the
    ///   user away from the channel they explicitly picked.
    /// - Active on a virtual transfer channel → 9-slot cycle of the
    ///   ember pair-derived views. Each transfer / trajectory view
    ///   paints the pair behind that virtual channel via
    ///   `resolve_transfer_pair_for_active`.
    ///
    /// CQT and Reassigned waterfall sub-modes stay reachable only via
    /// `--view waterfall --mode <mode>` to keep the cycle short.
    /// Shared by Tab / Shift+Tab.
    fn cycle_ember_view(&mut self, forward: bool) {
        let n_real = self.store.as_ref().map(|s| s.len()).unwrap_or(0);
        let on_virtual = self.config.active_channel >= n_real
            && (self.config.active_channel - n_real) < self.virtual_channels.len();
        let next = if on_virtual {
            if forward {
                match self.current_w_slot() {
                    Some(WSlot::SpectrumEmber) => WSlot::Goniometer,
                    Some(WSlot::Goniometer)    => WSlot::IoTransfer,
                    Some(WSlot::IoTransfer)    => WSlot::BodeMag,
                    Some(WSlot::BodeMag)       => WSlot::Coherence,
                    Some(WSlot::Coherence)     => WSlot::BodePhase,
                    Some(WSlot::BodePhase)     => WSlot::GroupDelay,
                    Some(WSlot::GroupDelay)    => WSlot::Nyquist,
                    Some(WSlot::Nyquist)       => WSlot::Ir,
                    Some(WSlot::Ir)            => WSlot::SpectrumEmber,
                    Some(WSlot::Waterfall) | Some(WSlot::Cwt)
                                               => WSlot::SpectrumEmber,
                    None                       => WSlot::SpectrumEmber,
                }
            } else {
                match self.current_w_slot() {
                    Some(WSlot::SpectrumEmber) => WSlot::Ir,
                    Some(WSlot::Goniometer)    => WSlot::SpectrumEmber,
                    Some(WSlot::IoTransfer)    => WSlot::Goniometer,
                    Some(WSlot::BodeMag)       => WSlot::IoTransfer,
                    Some(WSlot::Coherence)     => WSlot::BodeMag,
                    Some(WSlot::BodePhase)     => WSlot::Coherence,
                    Some(WSlot::GroupDelay)    => WSlot::BodePhase,
                    Some(WSlot::Nyquist)       => WSlot::GroupDelay,
                    Some(WSlot::Ir)            => WSlot::Nyquist,
                    Some(WSlot::Waterfall) | Some(WSlot::Cwt)
                                               => WSlot::SpectrumEmber,
                    None                       => WSlot::SpectrumEmber,
                }
            }
        } else if forward {
            match self.current_w_slot() {
                Some(WSlot::SpectrumEmber) => WSlot::Waterfall,
                Some(WSlot::Waterfall)     => WSlot::Cwt,
                Some(WSlot::Cwt)           => WSlot::SpectrumEmber,
                // Coming from a transfer view whose pair was just
                // unregistered (or `--view <transfer>` startup without
                // a pair) — drop to SpectrumEmber.
                _                          => WSlot::SpectrumEmber,
            }
        } else {
            match self.current_w_slot() {
                Some(WSlot::SpectrumEmber) => WSlot::Cwt,
                Some(WSlot::Waterfall)     => WSlot::SpectrumEmber,
                Some(WSlot::Cwt)           => WSlot::Waterfall,
                _                          => WSlot::SpectrumEmber,
            }
        };
        let (layout, view_mode, mode, label) = match next {
            WSlot::SpectrumEmber => (LayoutMode::Single, ViewMode::SpectrumEmber, "fft", "view: spectrum (ember)"),
            WSlot::Waterfall     => (LayoutMode::Single, ViewMode::Waterfall,     "fft", "view: waterfall (fft)"),
            WSlot::Cwt           => (LayoutMode::Single, ViewMode::Waterfall,     "cwt", "view: waterfall (cwt)"),
            WSlot::Goniometer    => (LayoutMode::Single, ViewMode::Goniometer,    "fft", "view: goniometer (ember)"),
            WSlot::IoTransfer    => (LayoutMode::Single, ViewMode::IoTransfer,    "fft", "view: iotransfer (ember)"),
            WSlot::BodeMag       => (LayoutMode::Single, ViewMode::BodeMag,       "fft", "view: bode mag (ember)"),
            WSlot::Coherence     => (LayoutMode::Single, ViewMode::Coherence,     "fft", "view: coherence (ember)"),
            WSlot::BodePhase     => (LayoutMode::Single, ViewMode::BodePhase,     "fft", "view: bode phase (ember)"),
            WSlot::GroupDelay    => (LayoutMode::Single, ViewMode::GroupDelay,    "fft", "view: group delay (ember)"),
            WSlot::Nyquist       => (LayoutMode::Single, ViewMode::Nyquist,       "fft", "view: nyquist (ember)"),
            WSlot::Ir            => (LayoutMode::Single, ViewMode::Ir,            "fft", "view: ir (ember)"),
        };
        if self.analysis_mode != mode && !self.send_set_analysis_mode(mode) {
            return;
        }
        let prev_view = self.config.view_mode;
        self.config.layout = layout;
        self.config.view_mode = view_mode;
        // Entering Waterfall view (FFT or CWT): wipe the history texture
        // so old rows from the previous analysis source don't bleed
        // into the new one (the waterfall renderer accumulates rows
        // over time; switching FFT ↔ CWT changes the bin axis).
        if matches!(view_mode, ViewMode::Waterfall) {
            for init in &mut self.waterfall_inited {
                *init = false;
            }
            if let (Some(ctx), Some(wf)) =
                (self.render_ctx.as_ref(), self.waterfall.as_mut())
            {
                wf.clear_history(&ctx.queue);
            }
        }
        let prev_default = crate::theme::default_db_window_for_view(prev_view);
        let next_default = crate::theme::default_db_window_for_view(view_mode);
        if prev_default != next_default {
            for view in self.cell_views.iter_mut() {
                view.db_min = next_default.0;
                view.db_max = next_default.1;
            }
        }
        self.reset_peak_holds();
        self.notify(label);
        self.mark_ui_dirty();
    }

    /// Identify the cell the cursor is in. Returns `(channel, nx, ny, w_px, h_px)`
    /// where `(nx, ny)` are normalized cell-local coords (y up) and `channel` is
    /// the cell's primary channel. In Overlay mode every cell shares the same
    /// rect so this returns the first hit; call [`targets_for_channel`] to
    /// resolve the full set of cell_views to mutate.
    pub(super) fn cell_at(&self, pos: PhysicalPosition<f64>) -> Option<(usize, f32, f32, f32, f32)> {
        let ctx = self.render_ctx.as_ref()?;
        let w = ctx.config.width as f32;
        let h = ctx.config.height as f32;
        let n_real = self.store.as_ref().map(|s| s.len()).unwrap_or(0);
        if n_real == 0 {
            return None;
        }
        // Include virtual transfer channels so hover / scroll / drag work on
        // their cells just like real ones.
        let n = n_real + self.virtual_render_pairs.len();
        let cells = layout::compute(
            self.config.layout,
            n,
            self.config.active_channel,
            self.layout_selection(),
            self.grid_params(),
        );
        for c in &cells {
            let r = layout::to_pixel_rect(c, w, h);
            let x = pos.x as f32;
            let y = pos.y as f32;
            if x >= r.left() && x <= r.right() && y >= r.top() && y <= r.bottom() {
                let nx = (x - r.left()) / r.width().max(1.0);
                let ny = 1.0 - (y - r.top()) / r.height().max(1.0);
                return Some((c.channel, nx, ny, r.width(), r.height()));
            }
        }
        None
    }

    /// Which `cell_views` indices should a mouse/key interaction under the
    /// given hovered channel mutate. Grid → just that cell. Overlay → all
    /// cells (their rects are stacked). Single → whichever channel is active.
    fn targets_for_channel(&self, hovered: usize) -> Vec<usize> {
        let n = self.cell_views.len();
        if n == 0 {
            return Vec::new();
        }
        match self.config.layout {
            LayoutMode::Grid => vec![hovered.min(n - 1)],
            LayoutMode::Single => vec![self.config.active_channel.min(n - 1)],
            // Compare stacks the selected set in one rect, so zoom/pan should
            // move every selected channel together to keep the overlay coherent.
            LayoutMode::Compare => self
                .selected
                .iter()
                .enumerate()
                .filter_map(|(i, sel)| sel.then_some(i))
                .collect(),
            LayoutMode::Sweep => vec![0],
        }
    }

    pub(super) fn apply_zoom(&mut self, scroll_y: f32) {
        let pos = match self.cursor_pos {
            Some(p) => p,
            None => return,
        };
        let (hovered, nx, ny, _, _) = match self.cell_at(pos) {
            Some(v) => v,
            None => return,
        };
        let targets = self.targets_for_channel(hovered);
        let factor = 0.85_f32.powf(scroll_y);
        let shift = self.modifiers.shift_key();
        let ctrl = self.modifiers.control_key();

        // Scope: plain scroll = strip-chart window (time per width),
        // Ctrl+scroll = y-amplitude (vertical gain). Cell freq/dB axes
        // don't apply on the scope, so we route to the synthetic-generator
        // parameters that mean something visually. Order is consistent
        // with the §2 binding: plain = primary axis, Ctrl = secondary.
        if matches!(self.config.view_mode, ViewMode::Scope) {
            if ctrl {
                let new_g = (self.ember_scope_y_gain * factor).clamp(0.02, 0.5);
                self.ember_scope_y_gain = new_g;
                self.notify(&format!("scope y-gain: {:.2}", new_g));
            } else {
                let new_w = (self.ember_scope_window_s * factor).clamp(0.005, 2.0);
                self.ember_scope_window_s = new_w;
                self.notify(&format!("scope window: {:.0} ms", new_w * 1000.0));
            }
            return;
        }

        // Axisless trajectory views — Goniometer (Re/Im trace), IoTransfer
        // (in/out scatter), Coherence (γ² fixed [0,1]), Nyquist (Re/Im(H)),
        // Ir (auto-gained polyline). None of these map onto the cell
        // freq/dB axes, so scroll has no meaningful target. Emit a one-shot
        // throttled notification so the user knows the gesture was seen
        // rather than silently swallowed. Throttle 2 s — a continuous
        // trackpad scroll over the cell shouldn't keep re-firing.
        if matches!(
            self.config.view_mode,
            ViewMode::Goniometer
                | ViewMode::IoTransfer
                | ViewMode::Coherence
                | ViewMode::Nyquist
                | ViewMode::Ir
        ) {
            let now = Instant::now();
            let recent = self
                .last_axisless_scroll_notify
                .is_some_and(|t| now.saturating_duration_since(t) < Duration::from_secs(2));
            if !recent {
                self.notify(&format!(
                    "no zoom on {}",
                    view_label(self.config.view_mode)
                ));
                self.last_axisless_scroll_notify = Some(now);
            }
            return;
        }

        // Ctrl+Shift+Scroll — "gain knob": pan the dB window up/down without
        // changing its span. Scroll up = trace rides higher in the cell
        // (floor+ceiling both shift down by the same amount). Step is 2 dB
        // per tick so a fast flick feels like an analog trim, not a jump.
        // Waterfall shares the behaviour: dB window is the colormap range,
        // so the same pan reveals quieter detail without re-zooming.
        if ctrl && shift {
            const GAIN_DB_PER_TICK: f32 = 2.0;
            let delta = -scroll_y * GAIN_DB_PER_TICK;
            let mut last = (0.0_f32, 0.0_f32);
            for idx in &targets {
                if let Some(view) = self.cell_views.get_mut(*idx) {
                    let span = view.db_max - view.db_min;
                    let mut new_min = (view.db_min + delta).max(-240.0);
                    let mut new_max = new_min + span;
                    if new_max > 20.0 {
                        new_max = 20.0;
                        new_min = (new_max - span).max(-240.0);
                    }
                    view.db_min = new_min;
                    view.db_max = new_max;
                    last = (new_min, new_max);
                }
            }
            self.notify(&format!("dB {:.0} … {:.0}", last.0, last.1));
            return;
        }

        // Hard floor/ceiling on the visible freq window: the spectrum data
        // only covers ~20 Hz..Nyquist, so letting the user zoom out past the
        // data just shows empty space. Ceiling grows to match the largest
        // `freqs.last()` we've seen from the producer (96 kHz sessions etc.).
        let data_log_min = theme::DEFAULT_FREQ_MIN.log10();
        let data_log_max = self.data_freq_ceiling.max(theme::DEFAULT_FREQ_MAX).log10();
        let data_ceiling = 10_f32.powf(data_log_max);
        let data_span = (data_log_max - data_log_min).max(0.001);
        // §2 binding rules: plain = both axes, Shift = freq only, Ctrl = Y
        // only (dB on spectrum-family, time-rows on waterfall). Pulled out
        // into a pure helper so the per-view modifier mapping is unit-
        // testable without mocking the full App state.
        let (zoom_freq, zoom_db, zoom_time) =
            decide_zoom_axes(self.config.view_mode, shift, ctrl);

        for idx in targets {
            let view = match self.cell_views.get_mut(idx) {
                Some(v) => v,
                None => continue,
            };
            if zoom_freq {
                let log_min = view.freq_min.max(1.0).log10();
                let log_max = view.freq_max.max(log_min.exp().max(10.0)).log10();
                let anchor = log_min + nx * (log_max - log_min);
                // Min span 0.01 decades (≈ 2.3 % bandwidth, e.g. ~23 Hz
                // wide at 1 kHz) — was 0.15 (≈ 41 % bandwidth) which
                // capped the zoom after just a few scroll ticks. Tight
                // enough now to resolve individual spectral peaks.
                let new_span = ((log_max - log_min) * factor).clamp(0.01, data_span);
                let mut new_min = anchor - nx * new_span;
                let mut new_max = new_min + new_span;
                if new_min < data_log_min {
                    new_min = data_log_min;
                    new_max = (new_min + new_span).min(data_log_max);
                }
                if new_max > data_log_max {
                    new_max = data_log_max;
                    new_min = (new_max - new_span).max(data_log_min);
                }
                view.freq_min = 10.0_f32.powf(new_min).max(theme::DEFAULT_FREQ_MIN);
                view.freq_max = 10.0_f32.powf(new_max).min(data_ceiling);
            }
            if zoom_db {
                let db_min = view.db_min;
                let db_max = view.db_max;
                let anchor = db_min + ny * (db_max - db_min);
                // Min span 2 dB (was 10) so zooming a peak/notch
                // actually reaches sub-dB resolution.
                let new_span = ((db_max - db_min) * factor).clamp(2.0, 240.0);
                let new_min = (anchor - ny * new_span).max(-240.0);
                let new_max = (new_min + new_span).min(20.0);
                view.db_min = new_min;
                view.db_max = new_max;
            }
            if zoom_time {
                // Fractional zoom: the f32 is authoritative so consecutive
                // scroll ticks don't lose precision to integer rounding,
                // giving a smoothly growing/shrinking time window instead of
                // stepped jumps. The u32 copy tracks round(f32) for the
                // shader and label consumers.
                let current = view.rows_visible_f.max(1.0);
                let max_rows = crate::render::waterfall::ROWS_PER_CHANNEL as f32;
                let new_rows = (current * factor).clamp(2.0, max_rows);
                view.rows_visible_f = new_rows;
                view.rows_visible = new_rows.round().clamp(2.0, max_rows) as u32;
            }
        }
    }

    /// Left-button release. If the press+release happened without meaningful
    /// movement, treat as a click: in Matrix (Grid) layout, this "zooms in"
    /// — sets the active channel to the one under the cursor and swaps into
    /// Single layout, preserving the current view_mode (spectrum/waterfall/
    /// cwt). Everywhere else the click is a no-op beyond clearing drag.
    pub(super) fn end_drag(&mut self) {
        let drag = match self.drag.take() {
            Some(d) => d,
            None => return,
        };
        let pos = match self.cursor_pos {
            Some(p) => p,
            None => return,
        };
        let dx = pos.x - drag.start.x;
        let dy = pos.y - drag.start.y;
        // 5 px dead-zone — a shaky hand / trackpad jitter during a click
        // shouldn't smuggle in a 1-pixel pan and disable zoom-in.
        if dx * dx + dy * dy > 25.0 {
            return;
        }
        if !matches!(self.config.layout, LayoutMode::Grid) {
            return;
        }
        let clicked = match self.cell_at(pos) {
            Some((ch, _, _, _, _)) => ch,
            None => return,
        };
        let n_real = self.store.as_ref().map(|s| s.len()).unwrap_or(0);
        self.config.active_channel = clicked;
        self.config.layout = LayoutMode::Single;
        if clicked >= n_real {
            // Virtual transfer cell — index it relative to the start of
            // the virtual range so the notification matches the
            // "transferN" naming used elsewhere.
            let v_idx = clicked - n_real;
            self.notify(&format!("zoom: transfer{v_idx}"));
        } else {
            self.notify(&format!("zoom: CH{clicked}"));
        }
    }

    pub(super) fn begin_drag(&mut self) {
        let pos = match self.cursor_pos {
            Some(p) => p,
            None => return,
        };
        if matches!(self.config.layout, LayoutMode::Sweep) {
            self.handle_sweep_click(pos);
            return;
        }
        let (hovered, _nx, _ny, cell_w, cell_h) = match self.cell_at(pos) {
            Some(v) => v,
            None => return,
        };
        let targets = self.targets_for_channel(hovered);
        // Capture the seed view from the first target so every cell in the
        // set pans by the same amount regardless of where they started.
        let seed = match targets.first().and_then(|&i| self.cell_views.get(i)) {
            Some(v) => *v,
            None => return,
        };
        let log_min = seed.freq_min.max(1.0).log10();
        let log_max = seed.freq_max.max(10.0).log10();
        self.drag = Some(DragState {
            start: pos,
            targets,
            start_log_min: log_min,
            start_log_max: log_max,
            start_db_min: seed.db_min,
            start_db_max: seed.db_max,
            cell_w_px: cell_w,
            cell_h_px: cell_h,
        });
    }

    fn handle_sweep_click(&mut self, pos: PhysicalPosition<f64>) {
        let kind = match self.sweep_kind {
            Some(k) => k,
            None => return,
        };
        let cells = layout::compute(
            self.config.layout,
            1,
            0,
            &self.selected,
            self.grid_params(),
        );
        let Some(cell) = cells.first() else { return };
        let ctx = self.render_ctx.as_ref().unwrap();
        let w = ctx.config.width as f32;
        let h = ctx.config.height as f32;
        let rect = layout::to_pixel_rect(cell, w, h);
        let cursor = egui::pos2(pos.x as f32, pos.y as f32);
        if let Some(idx) = crate::render::sweep::nearest_point(rect, kind, &self.sweep_last, cursor) {
            self.sweep_selected_idx = Some(idx);
            self.needs_redraw = true;
        }
    }

    pub(super) fn update_drag(&mut self, pos: PhysicalPosition<f64>) {
        let drag = match self.drag.clone() {
            Some(d) => d,
            None => return,
        };
        let waterfall = matches!(self.config.view_mode, ViewMode::Waterfall);
        let data_log_min = theme::DEFAULT_FREQ_MIN.log10();
        let data_log_max = self.data_freq_ceiling.max(theme::DEFAULT_FREQ_MAX).log10();
        let data_ceiling = 10_f32.powf(data_log_max);
        let dx_px = (pos.x - drag.start.x) as f32;
        let dy_px = (pos.y - drag.start.y) as f32;
        let log_span = drag.start_log_max - drag.start_log_min;
        let db_span = drag.start_db_max - drag.start_db_min;
        let d_log = -(dx_px / drag.cell_w_px.max(1.0)) * log_span;
        let d_db = -(dy_px / drag.cell_h_px.max(1.0)) * db_span;
        let new_log_min = (drag.start_log_min + d_log)
            .clamp(data_log_min, (data_log_max - log_span).max(data_log_min));
        let new_log_max = (new_log_min + log_span).min(data_log_max);
        let new_db_min = (drag.start_db_min + d_db).max(-240.0);
        let new_db_max = (new_db_min + db_span).min(20.0);
        for &idx in &drag.targets {
            if let Some(view) = self.cell_views.get_mut(idx) {
                view.freq_min = 10.0_f32.powf(new_log_min).max(theme::DEFAULT_FREQ_MIN);
                view.freq_max = 10.0_f32.powf(new_log_max).min(data_ceiling);
                if !waterfall {
                    view.db_min = new_db_min;
                    view.db_max = new_db_max;
                }
            }
        }
    }

    pub(super) fn reset_hovered_view(&mut self) {
        let pos = match self.cursor_pos {
            Some(p) => p,
            None => {
                self.reset_all_views();
                return;
            }
        };
        let hovered = match self.cell_at(pos) {
            Some((ch, _, _, _, _)) => ch,
            None => {
                self.reset_all_views();
                return;
            }
        };
        for idx in self.targets_for_channel(hovered) {
            if let Some(view) = self.cell_views.get_mut(idx) {
                *view = CellView::default();
            }
        }
        self.notify("view reset");
    }

    fn reset_all_views(&mut self) {
        for view in &mut self.cell_views {
            *view = CellView::default();
        }
        for init in &mut self.waterfall_inited {
            *init = false;
        }
        self.grid_cell_size = None;
        self.grid_page = 0;
        self.reset_peak_holds();
        self.notify("all views reset");
    }

    /// Clear every channel's peak-hold buffer. Leaves `peak_hold_enabled`
    /// alone — reset triggers (Enter, Ctrl+R, FFT-N / analysis-mode change)
    /// just drop the stale accumulator; the next fresh frame re-seeds it.
    fn reset_peak_holds(&mut self) {
        for slot in &mut self.peak_holds {
            *slot = None;
        }
        for slot in &mut self.peak_last_update {
            *slot = None;
        }
        for slot in &mut self.peak_last_tick {
            *slot = None;
        }
        for slot in &mut self.min_holds {
            *slot = None;
        }
        for slot in &mut self.min_last_update {
            *slot = None;
        }
        for slot in &mut self.min_last_tick {
            *slot = None;
        }
    }

    /// Which channel does Space act on. Single mode → the active channel (the
    /// one visible). Any other layout → the hovered cell, or the active
    /// channel as a fallback when the cursor sits outside the plot area.
    /// Clamps to the real channel count — Space over a virtual transfer
    /// cell is a no-op, because virtual channels can't themselves be used
    /// as MEAS/REF for a nested transfer.
    fn selection_target(&self) -> Option<usize> {
        let n_real = self.store.as_ref().map(|s| s.len()).unwrap_or(0);
        if n_real == 0 {
            return None;
        }
        let idx = match self.config.layout {
            LayoutMode::Single => self.config.active_channel,
            _ => self
                .cursor_pos
                .and_then(|p| self.cell_at(p))
                .map(|(ch, _, _, _, _)| ch)
                .unwrap_or(self.config.active_channel),
        };
        if idx >= n_real {
            return None;
        }
        Some(idx)
    }

    fn toggle_selection(&mut self) {
        let target = match self.selection_target() {
            Some(t) => t,
            None => return,
        };
        let now_selected = {
            let slot = &mut self.selected[target];
            *slot = !*slot;
            *slot
        };
        if now_selected {
            if !self.selection_order.contains(&target) {
                self.selection_order.push(target);
            }
        } else {
            self.selection_order.retain(|&i| i != target);
        }
        let count = self.selected.iter().filter(|s| **s).count();
        self.notify(&format!(
            "CH{} {} ({} selected)",
            target,
            if now_selected { "selected" } else { "unselected" },
            count,
        ));
    }

    /// Which of the four canonical W-cycle slots we're currently in.
    /// Returns None when the app is in a non-cycled layout (Compare, Transfer,
    /// Sweep); pressing W from any of those jumps back to the start of the
    /// cycle (Matrix).
    fn is_ember_view(&self) -> bool {
        matches!(
            self.config.view_mode,
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
        )
    }

    /// Map the current view state onto its Tab-cycle slot. Hidden /
    /// out-of-cycle views (`Spectrum`, `Scope`, plus the CQT and
    /// Reassigned waterfall sub-modes) and non-Single layouts return
    /// `None` — the cycle treats `None` as "jump to SpectrumEmber" so
    /// landing is deterministic even if the user opened the UI via
    /// `--view waterfall --mode reassigned` or similar.
    fn current_w_slot(&self) -> Option<WSlot> {
        if !matches!(self.config.layout, LayoutMode::Single) {
            return None;
        }
        match self.config.view_mode {
            ViewMode::SpectrumEmber => Some(WSlot::SpectrumEmber),
            ViewMode::Waterfall => match self.analysis_mode.as_str() {
                "fft" => Some(WSlot::Waterfall),
                "cwt" => Some(WSlot::Cwt),
                _     => None,
            },
            ViewMode::Goniometer    => Some(WSlot::Goniometer),
            ViewMode::IoTransfer    => Some(WSlot::IoTransfer),
            ViewMode::BodeMag       => Some(WSlot::BodeMag),
            ViewMode::Coherence     => Some(WSlot::Coherence),
            ViewMode::BodePhase     => Some(WSlot::BodePhase),
            ViewMode::GroupDelay    => Some(WSlot::GroupDelay),
            ViewMode::Nyquist       => Some(WSlot::Nyquist),
            ViewMode::Ir            => Some(WSlot::Ir),
            ViewMode::Spectrum
            | ViewMode::Scope => None,
        }
    }

    pub(super) fn handle_key(&mut self, elwt: &ActiveEventLoop, code: KeyCode) {
        match code {
            KeyCode::Escape | KeyCode::KeyQ => elwt.exit(),
            KeyCode::Enter => {
                self.config.frozen = !self.config.frozen;
                self.reset_peak_holds();
                self.notify(if self.config.frozen { "FROZEN" } else { "live" });
            }
            KeyCode::KeyP => {
                self.peak_hold_enabled = !self.peak_hold_enabled;
                self.reset_peak_holds();
                self.notify(if self.peak_hold_enabled {
                    "peak hold: on"
                } else {
                    "peak hold: off"
                });
            }
            KeyCode::KeyM if self.modifiers.shift_key() => {
                // Toggle daemon-side mic-curve correction. The flag is
                // process-wide; the daemon stamps the per-channel state
                // (`on` / `off` / `none`) on every monitor frame so the
                // overlay tag follows automatically without local mirror.
                self.mic_correction_enabled = !self.mic_correction_enabled;
                self.send_mic_correction_enabled();
                self.notify(if self.mic_correction_enabled {
                    "mic-cal: on"
                } else {
                    "mic-cal: off"
                });
            }
            KeyCode::KeyM => {
                self.min_hold_enabled = !self.min_hold_enabled;
                self.reset_peak_holds();
                self.notify(if self.min_hold_enabled {
                    "min hold: on"
                } else {
                    "min hold: off"
                });
            }
            KeyCode::KeyO if self.modifiers.shift_key() && self.analysis_mode == "cwt" => {
                self.ioct_bpo = match self.ioct_bpo {
                    None => Some(1),
                    Some(1) => Some(3),
                    Some(3) => Some(6),
                    Some(6) => Some(12),
                    Some(12) => Some(24),
                    Some(_) => None,
                };
                self.send_ioct_bpo();
                self.notify(&match self.ioct_bpo {
                    Some(n) => format!("ioct: 1/{n} oct"),
                    None => "ioct: off".into(),
                });
                self.needs_redraw = true;
            }
            KeyCode::KeyO => {
                self.smoothing_frac = smoothing::next(self.smoothing_frac);
                // Rebuilds on next frame — drop the cache so the new window
                // factor takes effect immediately even if n_bins/sr haven't
                // changed.
                self.smoothing_cache = None;
                // Stale peak/min buffers were taken over the old smoothing;
                // clear them so the user immediately sees traces matching
                // the new resolution.
                self.reset_peak_holds();
                self.notify(&format!(
                    "smoothing: {}",
                    smoothing::label(self.smoothing_frac),
                ));
            }
            KeyCode::KeyA => {
                self.band_weighting = self.band_weighting.next();
                self.send_band_weighting();
                self.notify(self.band_weighting.label());
            }
            KeyCode::KeyI if self.modifiers.shift_key() => {
                // Shift+I — zero Leq accumulators on the daemon. Only
                // meaningful in Leq mode; in other modes the flag is
                // latched but the integrator ignores it.
                self.send_reset_leq();
                self.notify("Leq: reset");
            }
            KeyCode::KeyI => {
                self.time_integration = self.time_integration.next();
                self.send_time_integration();
                self.notify(self.time_integration.label());
            }
            KeyCode::KeyH => {
                self.show_help = !self.show_help;
            }
            KeyCode::KeyS => {
                self.pending_screenshot = true;
            }
            // C and Space both toggle the channel selection at the
            // hovered cell. Builds the set used by Shift+C (compare)
            // and T (transfer pair). C is the documented binding; Space
            // is kept as a muscle-memory alias.
            KeyCode::KeyC if !self.modifiers.shift_key() => {
                self.toggle_selection();
            }
            KeyCode::Space => {
                self.toggle_selection();
            }
            // Shift+C — enter Compare on the selected channels. Empty
            // selection → no-op so an accidental press doesn't swap the
            // user out of their current view into an empty Compare grid.
            KeyCode::KeyC if self.modifiers.shift_key() => {
                if !self.selected.iter().any(|s| *s) {
                    self.notify("Shift+C: select ≥ 1 channel first (C over cell)");
                    return;
                }
                self.compare_set = self.selected.clone();
                self.clear_selection();
                self.config.layout = LayoutMode::Compare;
                self.notify("layout: compare");
            }
            KeyCode::KeyL if self.modifiers.shift_key() => {
                // Shift+L — zero the BS.1770-5 loudness accumulators
                // (integrated, LRA, true-peak) on the daemon and clear
                // the local readout so the overlay snaps to '—'.
                self.send_reset_loudness();
                self.notify("loudness: reset");
            }
            KeyCode::KeyT => {
                if self.selection_order.len() < 2 {
                    self.notify("T: select ≥ 2 channels first (C over cell; last = REF)");
                    return;
                }
                let meas = self.selection_order[0] as u32;
                let ref_ch = *self.selection_order.last().unwrap() as u32;
                let pair = TransferPair { meas, ref_ch };
                let n_real = self.store.as_ref().map(|s| s.len()).unwrap_or(0);
                let added = if self.virtual_channels.remove(pair) {
                    self.notify(&format!(
                        "T: removed transfer (CH{meas}←CH{ref_ch})"
                    ));
                    false
                } else {
                    self.virtual_channels.add(pair);
                    let idx = self.virtual_channels.len().saturating_sub(1);
                    self.notify(&format!(
                        "T: added transfer{idx} (CH{meas}←CH{ref_ch})"
                    ));
                    true
                };
                self.restart_transfer_stream();
                // Reset the live selection so a follow-up T/C starts fresh.
                self.clear_selection();
                // On add, jump to the new virtual transfer channel — that's
                // what the user just expressed intent to look at — so the
                // W cycle's pair-gated views immediately have a meaningful
                // active context.
                if added {
                    let new_virtual_idx = n_real + self.virtual_channels.len() - 1;
                    self.config.active_channel = new_virtual_idx;
                }
            }
            KeyCode::KeyF => {
                if let Some(ctx) = self.render_ctx.as_ref() {
                    let is_full = ctx.window.fullscreen().is_some();
                    ctx.window.set_fullscreen(if is_full {
                        None
                    } else {
                        Some(winit::window::Fullscreen::Borderless(None))
                    });
                }
                // Phase 6: persist the fullscreen flip — `snapshot_ui_state`
                // reads the live window state at flush time, so we just
                // need to mark dirty.
                self.mark_ui_dirty();
            }
            // Ember-substrate live tuning. Geometric ×1.25 step so a few
            // presses span the order of magnitude that separates extremes.
            // Bare , / .  → deposit intensity (brightness).
            // Shift + , / .  → τ_p (fade rate; lower = snappier trail).
            // Active in every ember view (Scope, SpectrumEmber, Goniometer,
            // IoTransfer); ignored elsewhere.
            KeyCode::Comma if self.is_ember_view() => {
                if self.modifiers.shift_key() {
                    self.ember_tau_p_scale =
                        (self.ember_tau_p_scale / 1.25).clamp(0.1, 10.0);
                    self.notify(&format!(
                        "ember τ_p ×{:.2}",
                        self.ember_tau_p_scale
                    ));
                } else {
                    self.ember_intensity_scale =
                        (self.ember_intensity_scale / 1.25).clamp(0.05, 20.0);
                    self.notify(&format!(
                        "ember intensity ×{:.2}",
                        self.ember_intensity_scale
                    ));
                }
                self.mark_ui_dirty();
            }
            KeyCode::Period if self.is_ember_view() => {
                if self.modifiers.shift_key() {
                    self.ember_tau_p_scale =
                        (self.ember_tau_p_scale * 1.25).clamp(0.1, 10.0);
                    self.notify(&format!(
                        "ember τ_p ×{:.2}",
                        self.ember_tau_p_scale
                    ));
                } else {
                    self.ember_intensity_scale =
                        (self.ember_intensity_scale * 1.25).clamp(0.05, 20.0);
                    self.notify(&format!(
                        "ember intensity ×{:.2}",
                        self.ember_intensity_scale
                    ));
                }
                self.mark_ui_dirty();
            }
            KeyCode::KeyD => {
                self.show_timing = !self.show_timing;
                self.notify(if self.show_timing { "timing on" } else { "timing off" });
            }
            // Snap to the ember matrix overview (SpectrumEmber + Grid)
            // from any view. The legacy Spectrum + Grid line plot is
            // reachable only via `--view spectrum` for empirical work
            // on the legacy renderer. Pair with left-click on a cell to
            // pick a channel: matrix → click → Single+SpectrumEmber on
            // that channel → Tab cycles SpectrumEmber → Waterfall (FFT)
            // → Waterfall (CWT) from there.
            KeyCode::KeyG => {
                let prev_view = self.config.view_mode;
                let already_matrix = matches!(prev_view, ViewMode::SpectrumEmber)
                    && matches!(self.config.layout, LayoutMode::Grid);
                if already_matrix {
                    return;
                }
                if self.analysis_mode != "fft" && !self.send_set_analysis_mode("fft") {
                    // Daemon refused FFT — stay put so a retry is meaningful.
                    return;
                }
                self.config.view_mode = ViewMode::SpectrumEmber;
                self.config.layout = LayoutMode::Grid;
                let prev_default = crate::theme::default_db_window_for_view(prev_view);
                let next_default = crate::theme::default_db_window_for_view(ViewMode::SpectrumEmber);
                if prev_default != next_default {
                    for view in self.cell_views.iter_mut() {
                        view.db_min = next_default.0;
                        view.db_max = next_default.1;
                    }
                }
                self.reset_peak_holds();
                self.notify("view: matrix");
                self.mark_ui_dirty();
            }
            // Cycle the waterfall colormap palette. `;` advances; Ctrl+`;`
            // cycles backward. Only meaningful in Waterfall view — in other
            // views, notify so the user knows the key was seen.
            KeyCode::Semicolon => {
                if !matches!(self.config.view_mode, ViewMode::Waterfall) {
                    self.notify("palette: only in waterfall view");
                    return;
                }
                let step: i32 = if self.modifiers.control_key() { -1 } else { 1 };
                let new_idx = self.waterfall.as_mut().map(|wf| {
                    let n = crate::render::waterfall::N_PALETTES as i32;
                    let cur = wf.active_palette() as i32;
                    let next = ((cur + step).rem_euclid(n)) as u32;
                    wf.set_palette(next);
                    next as usize
                });
                if let Some(idx) = new_idx {
                    let name = crate::render::waterfall::PALETTE_NAMES
                        .get(idx)
                        .copied()
                        .unwrap_or("?");
                    self.notify(&format!("palette: {name}"));
                    self.needs_redraw = true;
                }
            }
            // W is gone — Tab takes over the ember-cycle (see KeyCode::Tab
            // arm below). The cycle body lives in `cycle_ember_view`.
            KeyCode::ArrowUp if self.modifiers.shift_key() && self.analysis_mode == "cwt" => {
                self.cwt_sigma = (self.cwt_sigma + 1.0).min(24.0);
                self.send_cwt_params();
                self.notify(&format!("cwt sigma: {:.0}", self.cwt_sigma));
            }
            KeyCode::ArrowDown if self.modifiers.shift_key() && self.analysis_mode == "cwt" => {
                self.cwt_sigma = (self.cwt_sigma - 1.0).max(5.0);
                self.send_cwt_params();
                self.notify(&format!("cwt sigma: {:.0}", self.cwt_sigma));
            }
            KeyCode::ArrowRight if self.modifiers.shift_key() && self.analysis_mode == "cwt" => {
                self.cwt_n_scales = (self.cwt_n_scales * 2).min(8192);
                self.send_cwt_params();
                self.notify(&format!("cwt scales: {}", self.cwt_n_scales));
            }
            KeyCode::ArrowLeft if self.modifiers.shift_key() && self.analysis_mode == "cwt" => {
                self.cwt_n_scales = (self.cwt_n_scales / 2).max(64);
                self.send_cwt_params();
                self.notify(&format!("cwt scales: {}", self.cwt_n_scales));
            }
            KeyCode::ArrowLeft
                if !self.modifiers.shift_key() && self.analysis_mode == "fft" =>
            {
                self.monitor_interval_ms =
                    (self.monitor_interval_ms + 1).clamp(MONITOR_INTERVAL_MIN_MS, MONITOR_INTERVAL_MAX_MS);
                self.send_monitor_params();
                self.notify(&format!("interval: {} ms", self.monitor_interval_ms));
            }
            KeyCode::ArrowRight
                if !self.modifiers.shift_key() && self.analysis_mode == "fft" =>
            {
                self.monitor_interval_ms =
                    self.monitor_interval_ms.saturating_sub(1).max(MONITOR_INTERVAL_MIN_MS);
                self.send_monitor_params();
                self.notify(&format!("interval: {} ms", self.monitor_interval_ms));
            }
            KeyCode::ArrowUp
                if !self.modifiers.shift_key() && self.analysis_mode == "fft" =>
            {
                self.monitor_fft_n = step_ladder(MONITOR_FFT_N_LADDER, self.monitor_fft_n, 1);
                self.monitor_interval_ms =
                    auto_monitor_interval_ms(self.monitor_fft_n, self.current_sr());
                self.send_monitor_params();
                self.reset_peak_holds();
                self.notify(&format!(
                    "fft N: {} @ {} ms",
                    self.monitor_fft_n, self.monitor_interval_ms
                ));
            }
            KeyCode::ArrowDown
                if !self.modifiers.shift_key() && self.analysis_mode == "fft" =>
            {
                self.monitor_fft_n = step_ladder(MONITOR_FFT_N_LADDER, self.monitor_fft_n, -1);
                self.monitor_interval_ms =
                    auto_monitor_interval_ms(self.monitor_fft_n, self.current_sr());
                self.send_monitor_params();
                self.reset_peak_holds();
                self.notify(&format!(
                    "fft N: {} @ {} ms",
                    self.monitor_fft_n, self.monitor_interval_ms
                ));
            }
            // unified.md Phase 0b — Goniometer-only `R` toggles M/S vs raw
            // L/R rotation. MUST come before the unguarded `KeyR` arm
            // below so the more-specific match wins; the existing Ctrl+R
            // reset stays distinct because of its `control_key()` guard.
            KeyCode::KeyR
                if !self.modifiers.control_key()
                    && !self.modifiers.shift_key()
                    && matches!(self.config.view_mode, ViewMode::Goniometer) =>
            {
                self.ember_gonio_rotation_ms = !self.ember_gonio_rotation_ms;
                let label = if self.ember_gonio_rotation_ms {
                    "M/S"
                } else {
                    "raw L/R"
                };
                self.notify(&format!("goniometer rotation: {label}"));
                self.mark_ui_dirty();
            }
            KeyCode::KeyR if self.modifiers.control_key() => {
                self.reset_all_views();
            }
            // Wipe the ember substrate to black + reset the stereo
            // auto-gain peak so the next signal autoscales fresh. Useful
            // when A/B-ing test signals: without this the prior content
            // hangs around for ~1 s of τ_p decay and bleeds into what
            // looks like the new signal.
            KeyCode::KeyZ
                if !self.modifiers.control_key()
                    && !self.modifiers.shift_key()
                    && self.is_ember_view() =>
            {
                if let Some(ember) = self.ember.as_mut() {
                    ember.request_clear();
                }
                self.ember_stereo_peak = 0.5;
                self.notify("ember: cleared");
            }
            // Coherence-weighting sharpness cycle for transfer-derived
            // ember views. `K` advances {off, k=1, k=2, k=4}: each step
            // makes the weighting more punishing (γ²^k → tiny for low
            // γ²). Default lands on k=2 — moderate dim of low-coherence
            // fuzz without over-extinguishing borderline bins. Coherence
            // and IR views ignore this knob (would self-extinguish or
            // have no per-sample γ²).
            KeyCode::KeyK
                if !self.modifiers.control_key()
                    && !self.modifiers.shift_key()
                    && self.is_ember_view() =>
            {
                let next = match self.ember_coherence_k {
                    k if k <= 0.0    => 1.0,
                    k if k < 1.5     => 2.0,
                    k if k < 3.0     => 4.0,
                    _                => 0.0,
                };
                self.ember_coherence_k = next;
                if next <= 0.0 {
                    self.notify("ember coherence weight: off");
                } else {
                    self.notify(&format!("ember coherence weight: γ²^{:.0}", next));
                }
                self.mark_ui_dirty();
            }
            KeyCode::Tab => {
                let n_real = self.store.as_ref().map(|s| s.len()).unwrap_or(0);
                let n_virt = self.virtual_channels.len();
                let n = (n_real + n_virt).max(1);
                let forward = !self.modifiers.shift_key();
                // Grid layout: Tab pages through the grid when there's
                // more than one page. Single page → fall through to the
                // ember-cycle below so Tab still does *something* useful.
                if matches!(self.config.layout, LayoutMode::Grid) {
                    let (_, _, _, pages) = layout::grid_dims(n, self.grid_params());
                    if pages > 1 {
                        let delta = if forward { 1 } else { pages - 1 };
                        self.grid_page = (self.grid_page + delta) % pages;
                        self.notify(&format!("page {}/{}", self.grid_page + 1, pages));
                        return;
                    }
                }
                // Non-Grid (and single-page Grid): Tab cycles the ember
                // view forward, Shift+Tab back. Pair-gated; collapses to
                // SpectrumEmber + unlock hint when no transfer pair is
                // resolvable. Channel-cycling moved off Tab — left-click
                // on a Grid cell handles channel pickup, and `C` builds
                // the multi-channel selection used by Shift+C / T.
                self.cycle_ember_view(forward);
            }
            _ => {}
        }
    }

    /// Resolve the set of cell_views a non-mouse key interaction targets:
    /// hovered cell when the cursor is over one, otherwise every cell so the
    /// keybind still does *something* useful when the mouse is outside.
    fn key_targets(&self) -> Vec<usize> {
        match self.cursor_pos.and_then(|p| self.cell_at(p)) {
            Some((ch, _, _, _, _)) => self.targets_for_channel(ch),
            None => (0..self.cell_views.len()).collect(),
        }
    }

}

#[cfg(test)]
mod tests {
    use super::{decide_zoom_axes, view_label};
    use crate::data::types::ViewMode;

    /// Spectrum-family modifier matrix from §2: plain = both axes, Shift =
    /// freq, Ctrl = dB. Locks the public binding so future refactors of
    /// `apply_zoom` can't silently drift.
    #[test]
    fn spectrum_family_modifiers_match_binding_rules() {
        for view in [
            ViewMode::Spectrum,
            ViewMode::SpectrumEmber,
            ViewMode::BodeMag,
            ViewMode::BodePhase,
            ViewMode::GroupDelay,
        ] {
            assert_eq!(decide_zoom_axes(view, false, false), (true, true, false));
            assert_eq!(decide_zoom_axes(view, true, false), (true, false, false));
            assert_eq!(decide_zoom_axes(view, false, true), (false, true, false));
        }
    }

    /// Waterfall: plain = freq + time, Shift = freq only, Ctrl = time only.
    /// dB is the colormap range — Ctrl+Shift+Scroll pans it elsewhere, this
    /// helper never returns dB-axis zoom for waterfall.
    #[test]
    fn waterfall_modifiers_match_binding_rules() {
        let v = ViewMode::Waterfall;
        assert_eq!(decide_zoom_axes(v, false, false), (true, false, true));
        assert_eq!(decide_zoom_axes(v, true, false), (true, false, false));
        assert_eq!(decide_zoom_axes(v, false, true), (false, false, true));
    }

    /// `view_label` covers every ViewMode variant — exhaustive coverage
    /// check by exercising each variant. If a new variant is added without
    /// updating view_label, this test fails to compile or returns "" for
    /// the missing arm.
    #[test]
    fn view_label_is_exhaustive_and_nonempty() {
        for view in [
            ViewMode::Spectrum,
            ViewMode::Waterfall,
            ViewMode::Scope,
            ViewMode::SpectrumEmber,
            ViewMode::Goniometer,
            ViewMode::IoTransfer,
            ViewMode::BodeMag,
            ViewMode::Coherence,
            ViewMode::BodePhase,
            ViewMode::GroupDelay,
            ViewMode::Nyquist,
            ViewMode::Ir,
        ] {
            assert!(!view_label(view).is_empty(), "empty label for {view:?}");
        }
    }
}
