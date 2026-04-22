//! Input handling — mouse drag/scroll, keyboard, selection. Methods land in
//! `impl App` here and are dispatched from the parent `app.rs` ApplicationHandler
//! (window_event / about_to_wait).

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
            &self.selected,
            &self.selection_order,
            self.active_meas_idx,
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
            // Transfer cell carries the active meas channel slot; zoom/pan
            // acts on that meas' CellView (phase/coh sub-panels inherit the
            // frequency axis from it). Last-selected is REF, everything
            // before it is a meas — `active_meas_idx` picks the current one.
            LayoutMode::Transfer => {
                if let Some(meas) = self.transfer_active_meas() {
                    vec![meas.min(n - 1)]
                } else {
                    Vec::new()
                }
            }
            LayoutMode::Sweep => vec![0],
        }
    }

    pub(super) fn apply_zoom(&mut self, scroll_y: f32) {
        // Alt+Scroll cycles the waterfall colormap palette (inferno → viridis
        // → magma → plasma → inferno). Alt is otherwise unused in scroll
        // handling so this is non-breaking; Shift keeps dB-gain zoom semantics.
        // Spectrum mode ignores the cycle — palette only affects the LUT.
        if self.modifiers.alt_key()
            && matches!(self.config.view_mode, ViewMode::Waterfall)
            && scroll_y != 0.0
        {
            self.alt_scroll_accum += scroll_y;
            let steps = self.alt_scroll_accum.trunc() as i32;
            if steps != 0 {
                self.alt_scroll_accum -= steps as f32;
                let new_idx = self.waterfall.as_mut().map(|wf| {
                    let n = crate::render::waterfall::N_PALETTES as i32;
                    let cur = wf.active_palette() as i32;
                    let next = ((cur + steps).rem_euclid(n)) as u32;
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
            return;
        }
        // Alt released (or not held) — drop any leftover fractional scroll so
        // the next Alt+Scroll session starts from zero instead of firing on
        // the first tick.
        if !self.modifiers.alt_key() && self.alt_scroll_accum != 0.0 {
            self.alt_scroll_accum = 0.0;
        }

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
        let waterfall = matches!(self.config.view_mode, ViewMode::Waterfall);

        // Hard floor/ceiling on the visible freq window: the spectrum data
        // only covers ~20 Hz..Nyquist, so letting the user zoom out past the
        // data just shows empty space. Ceiling grows to match the largest
        // `freqs.last()` we've seen from the producer (96 kHz sessions etc.).
        let data_log_min = theme::DEFAULT_FREQ_MIN.log10();
        let data_log_max = self.data_freq_ceiling.max(theme::DEFAULT_FREQ_MAX).log10();
        let data_ceiling = 10_f32.powf(data_log_max);
        let data_span = (data_log_max - data_log_min).max(0.001);
        // In waterfall mode: plain scroll = freq, Ctrl+scroll = time (rows
        // shown), Shift+scroll = gain (colormap dB). Spectrum mode keeps the
        // "plain scroll zooms both axes at once" feel.
        let (zoom_freq, zoom_db, zoom_time) = if waterfall {
            (!shift && !ctrl, shift, ctrl)
        } else {
            (!shift, !ctrl, false)
        };

        for idx in targets {
            let view = match self.cell_views.get_mut(idx) {
                Some(v) => v,
                None => continue,
            };
            if zoom_freq {
                let log_min = view.freq_min.max(1.0).log10();
                let log_max = view.freq_max.max(log_min.exp().max(10.0)).log10();
                let anchor = log_min + nx * (log_max - log_min);
                let new_span = ((log_max - log_min) * factor).clamp(0.15, data_span);
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
                let new_span = ((db_max - db_min) * factor).clamp(10.0, 240.0);
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
            &self.selection_order,
            self.active_meas_idx,
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
        let in_transfer = matches!(self.config.layout, LayoutMode::Transfer);
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
        if in_transfer {
            // Selection change while live: clamp the active meas into the
            // (possibly shrunk) meas list, then hot-swap the running
            // transfer_stream to match the new pair.
            let meas_count = self.selection_order.len().saturating_sub(1);
            if meas_count == 0 {
                self.active_meas_idx = 0;
            } else if self.active_meas_idx >= meas_count {
                self.active_meas_idx = meas_count - 1;
            }
            self.restart_transfer_stream();
        }
    }

    /// Pick the next layout in the cycle given current selection state.
    /// Compare and Transfer are only visited when the user has selected
    /// enough channels (Compare: any; Transfer: >= 2).
    fn next_layout(&self, from: LayoutMode) -> LayoutMode {
        let any_selected = self.selected.iter().any(|s| *s);
        // Transfer is reachable when either a fresh L-layout meas/ref pair is
        // available (≥ 2 selected) or virtual channels are already registered
        // — the layout still has content to render from the virtual pairs
        // even if the user has since cleared their selection.
        let transfer_ready =
            self.selection_order.len() >= 2 || !self.virtual_channels.is_empty();
        let raw_cycle = [
            LayoutMode::Grid,
            LayoutMode::Single,
            LayoutMode::Compare,
            LayoutMode::Transfer,
        ];
        let start = raw_cycle
            .iter()
            .position(|m| *m == from)
            .map(|i| (i + 1) % raw_cycle.len())
            .unwrap_or(0);
        for offset in 0..raw_cycle.len() {
            let candidate = raw_cycle[(start + offset) % raw_cycle.len()];
            let allowed = match candidate {
                LayoutMode::Compare => any_selected,
                LayoutMode::Transfer => transfer_ready,
                _ => true,
            };
            if allowed {
                return candidate;
            }
        }
        LayoutMode::Grid
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
            KeyCode::Space => {
                self.toggle_selection();
            }
            KeyCode::KeyH => {
                self.show_help = !self.show_help;
            }
            KeyCode::KeyS => {
                self.pending_screenshot = true;
            }
            KeyCode::KeyL => {
                let prev = self.config.layout;
                self.config.layout = self.next_layout(prev);
                self.on_layout_changed(prev, self.config.layout);
                self.notify(match self.config.layout {
                    LayoutMode::Grid => "layout: grid",
                    LayoutMode::Single => "layout: single",
                    LayoutMode::Compare => "layout: compare",
                    LayoutMode::Transfer => "layout: transfer",
                    LayoutMode::Sweep => "layout: sweep",
                });
            }
            KeyCode::KeyT => {
                if self.selection_order.len() < 2 {
                    self.notify("T: select ≥ 2 channels first (last = REF)");
                    return;
                }
                let meas = self.selection_order[0] as u32;
                let ref_ch = *self.selection_order.last().unwrap() as u32;
                let pair = TransferPair { meas, ref_ch };
                if self.virtual_channels.remove(pair) {
                    self.notify(&format!(
                        "T: removed transfer (CH{meas}←CH{ref_ch})"
                    ));
                } else {
                    self.virtual_channels.add(pair);
                    let idx = self.virtual_channels.len().saturating_sub(1);
                    self.notify(&format!(
                        "T: added transfer{idx} (CH{meas}←CH{ref_ch})"
                    ));
                }
                self.restart_transfer_stream();
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
            }
            KeyCode::Equal | KeyCode::NumpadAdd => {
                self.adjust_hovered_db_span(-20.0);
            }
            KeyCode::Minus | KeyCode::NumpadSubtract => {
                self.adjust_hovered_db_span(20.0);
            }
            KeyCode::KeyD => {
                self.show_timing = !self.show_timing;
                self.notify(if self.show_timing { "timing on" } else { "timing off" });
            }
            KeyCode::KeyW => {
                // W cycles three states so the waterfall view can toggle
                // between the linear FFT and Morlet CWT analysis paths
                // without needing a second hotkey:
                //   Spectrum(fft) → Waterfall(fft) → Waterfall(cwt) → Spectrum(fft)
                let (next_view, next_mode, label): (ViewMode, &str, &str) =
                    match (self.config.view_mode, self.analysis_mode.as_str()) {
                        (ViewMode::Spectrum, _) => (ViewMode::Waterfall, "fft", "view: waterfall (fft)"),
                        (ViewMode::Waterfall, "fft") => (ViewMode::Waterfall, "cwt", "view: waterfall (cwt)"),
                        (ViewMode::Waterfall, _) => (ViewMode::Spectrum, "fft", "view: spectrum"),
                    };
                if self.analysis_mode != next_mode && !self.send_set_analysis_mode(next_mode) {
                    // Mode change refused — don't advance the view so the
                    // key keeps meaning "next state" on the next press.
                    return;
                }
                self.config.view_mode = next_view;
                // Re-arm waterfall auto-init on every switch into waterfall
                // (or between FFT ↔ CWT where the dB distribution shifts) so
                // a fresh dB window gets picked from the current signal.
                if matches!(self.config.view_mode, ViewMode::Waterfall) {
                    for init in &mut self.waterfall_inited {
                        *init = false;
                    }
                }
                // FFT ↔ CWT changes the bin grid; a stale peak buffer would
                // mis-align with the new frames. Spectrum ↔ waterfall keeps
                // the grid but a peak marker is meaningless in waterfall, so
                // resetting in all W cycles keeps the state simple.
                self.reset_peak_holds();
                self.notify(label);
            }
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
                self.cwt_n_scales = (self.cwt_n_scales * 2).min(2048);
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
            KeyCode::BracketLeft => {
                self.shift_hovered_db_floor(-5.0);
            }
            KeyCode::BracketRight => {
                self.shift_hovered_db_floor(5.0);
            }
            KeyCode::KeyR if self.modifiers.control_key() => {
                self.reset_all_views();
            }
            KeyCode::Tab => {
                let n_real = self.store.as_ref().map(|s| s.len()).unwrap_or(0);
                let n_virt = self.virtual_channels.len();
                // Virtual transfer channels participate in Tab cycling so the
                // user can drop into Single / Compare for any `transfer{n}`
                // without first having to re-select the pair.
                let n = (n_real + n_virt).max(1);
                // In Grid layout Tab pages through the grid (when more than
                // one page exists). Other layouts still cycle the active
                // channel for Single / overlay channel-of-interest.
                if matches!(self.config.layout, LayoutMode::Grid) {
                    let (_, _, _, pages) = layout::grid_dims(n, self.grid_params());
                    if pages > 1 {
                        let delta = if self.modifiers.shift_key() {
                            pages - 1
                        } else {
                            1
                        };
                        self.grid_page = (self.grid_page + delta) % pages;
                        self.notify(&format!("page {}/{}", self.grid_page + 1, pages));
                        return;
                    }
                }
                // Transfer layout: Tab/Shift+Tab rotates the active meas
                // channel and hot-swaps the running transfer_stream worker.
                // With only one meas selected this is a no-op.
                if matches!(self.config.layout, LayoutMode::Transfer) {
                    let meas_count = self.selection_order.len().saturating_sub(1);
                    if meas_count > 1 {
                        let delta = if self.modifiers.shift_key() {
                            meas_count - 1
                        } else {
                            1
                        };
                        self.active_meas_idx =
                            (self.active_meas_idx + delta) % meas_count;
                        let meas = self
                            .transfer_active_meas()
                            .unwrap_or(self.config.active_channel);
                        self.notify(&format!(
                            "MEAS CH{} ({}/{})",
                            meas,
                            self.active_meas_idx + 1,
                            meas_count,
                        ));
                        self.restart_transfer_stream();
                    }
                    return;
                }
                let delta = if self.modifiers.shift_key() { n - 1 } else { 1 };
                self.config.active_channel = (self.config.active_channel + delta) % n;
                let label = if self.config.active_channel < n_real {
                    format!("CH{}", self.config.active_channel)
                } else {
                    format!("transfer{}", self.config.active_channel - n_real)
                };
                self.notify(&label);
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

    fn adjust_hovered_db_span(&mut self, delta: f32) {
        for idx in self.key_targets() {
            if let Some(view) = self.cell_views.get_mut(idx) {
                let span = (view.db_max - view.db_min + delta).clamp(20.0, 240.0);
                view.db_min = (view.db_max - span).max(-240.0);
            }
        }
    }

    fn shift_hovered_db_floor(&mut self, delta: f32) {
        let mut last = 0.0_f32;
        for idx in self.key_targets() {
            if let Some(view) = self.cell_views.get_mut(idx) {
                view.db_min = (view.db_min + delta).clamp(-240.0, view.db_max - 10.0);
                last = view.db_min;
            }
        }
        self.notify(&format!("db min {}", last));
    }
}
