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

#[derive(Copy, Clone, PartialEq)]
enum WSlot {
    Matrix,
    Single,
    Waterfall,
    Cwt,
    Cqt,
    Reassigned,
    Scope,
    SpectrumEmber,
    Goniometer,
    IoTransfer,
    BodeMag,
    Coherence,
    BodePhase,
    GroupDelay,
    Nyquist,
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

/// Rubber-band box-zoom state. Right-press seeds it with the hovered cell's
/// pixel rect plus the view axes at that moment; CursorMoved updates
/// `current`; right-release maps the selected sub-rect back onto the view's
/// freq/dB/time ranges. `current` starting equal to `start` lets the overlay
/// draw a zero-size rect on the first frame without a None branch.
#[derive(Clone)]
pub(super) struct BoxZoomState {
    pub(super) start: PhysicalPosition<f64>,
    pub(super) current: PhysicalPosition<f64>,
    pub(super) targets: Vec<usize>,
    pub(super) cell_left_px: f32,
    pub(super) cell_top_px: f32,
    pub(super) cell_w_px: f32,
    pub(super) cell_h_px: f32,
    pub(super) start_log_min: f32,
    pub(super) start_log_max: f32,
    pub(super) start_db_min: f32,
    pub(super) start_db_max: f32,
    pub(super) start_rows_f: f32,
    pub(super) waterfall: bool,
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
        // Shift+Scroll cycles the waterfall colormap palette (inferno → magma
        // → inferno). Used to live on Alt+Scroll, but Alt is
        // consumed by the window manager (meta) on common Linux desktops and
        // fights the UI. Gain zoom lost the Shift+Scroll binding — use
        // `[`/`]` (shift dB floor) and `+`/`-` (adjust dB span) instead.
        // Spectrum mode ignores the cycle — palette only affects the LUT.
        if self.modifiers.shift_key()
            && !self.modifiers.control_key()
            && matches!(self.config.view_mode, ViewMode::Waterfall)
            && scroll_y != 0.0
        {
            self.palette_scroll_accum += scroll_y;
            let steps = self.palette_scroll_accum.trunc() as i32;
            if steps != 0 {
                self.palette_scroll_accum -= steps as f32;
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
        // Shift released (or not held) — drop any leftover fractional scroll so
        // the next Shift+Scroll session starts from zero instead of firing on
        // the first tick.
        if !self.modifiers.shift_key() && self.palette_scroll_accum != 0.0 {
            self.palette_scroll_accum = 0.0;
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

        // Scope: scroll = y-amplitude (vertical gain), Ctrl+Scroll =
        // strip-chart window (time per width). The renderer-side cell_view
        // freq/dB axes don't apply here — there's no calibrated dBFS or
        // freq axis on the scope, so we route both knobs to the synthetic-
        // generator parameters that *do* mean something visually.
        if matches!(self.config.view_mode, ViewMode::Scope) {
            if ctrl {
                let new_w = (self.ember_scope_window_s * factor).clamp(0.005, 2.0);
                self.ember_scope_window_s = new_w;
                self.notify(&format!("scope window: {:.0} ms", new_w * 1000.0));
            } else {
                let new_g = (self.ember_scope_y_gain * factor).clamp(0.02, 0.5);
                self.ember_scope_y_gain = new_g;
                self.notify(&format!("scope y-gain: {:.2}", new_g));
            }
            return;
        }

        // Trajectory views (unified.md Phase 1) — per-view scroll mappings.
        // The cell freq/dB axes don't apply to these substrate views, so
        // scroll routes to the view's own meaningful knob.
        match self.config.view_mode {
            ViewMode::Goniometer => {
                // M/S↔raw rotation lives on the `R` key (no modifier).
                // Trackpads emit dozens of scroll deltas per physical
                // gesture, so a binary scroll-toggle flipped on every
                // micro-event and looked broken; the keyboard variant
                // is one keypress = one toggle. Swallow scroll here so
                // it doesn't fall through to spectrum-style freq/dB zoom
                // on the Goniometer cell (which has neither axis).
                return;
            }
            ViewMode::IoTransfer => {
                // No axes on the IoTransfer cell — swallow scroll so it
                // doesn't fall through to spectrum-style freq/dB zoom.
                // (Future: Ctrl+scroll could pan the auto-gain target
                // if the user wants finer control over how much of the
                // cell the trace fills.)
                return;
            }
            ViewMode::Coherence => {
                // Coherence y is fixed [0, 1] and the builder uses a
                // hardcoded full-band x range — neither axis takes
                // user input. Swallow scroll.
                return;
            }
            ViewMode::BodeMag | ViewMode::BodePhase | ViewMode::GroupDelay => {
                // These views have both freq (x) and signed-y axes —
                // let the standard spectrum-style scroll-zoom path
                // handle it (plain = both axes; Shift = freq only;
                // Ctrl = y only). Falls through.
            }
            ViewMode::Nyquist => {
                // Nyquist axes are Re(H) / Im(H) — neither maps onto
                // the cell freq/dB axes the standard scroll path
                // works on. Auto-gain handles scale; swallow scroll.
                return;
            }
            _ => {}
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
        // In waterfall mode: plain scroll = freq, Ctrl+scroll = time (rows
        // shown). Shift+Scroll is reserved for palette cycling (handled above,
        // already returned). Gain zoom moved to `[` / `]` / `+` / `-`.
        // Spectrum mode: plain scroll zooms both axes, Shift = freq only.
        let (zoom_freq, zoom_db, zoom_time) = if waterfall {
            (!ctrl, false, ctrl)
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
        if clicked >= n_real {
            return;
        }
        self.config.active_channel = clicked;
        self.config.layout = LayoutMode::Single;
        self.notify(&format!("zoom: CH{clicked}"));
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

    /// Right-button press. Starts a rubber-band zoom on the hovered cell. A
    /// release without meaningful movement falls through to the legacy
    /// "reset hovered view" behaviour (see `end_box_zoom`). Skipped in Sweep
    /// layout where the Y-axis is not a zoomable freq/dB plane.
    pub(super) fn begin_box_zoom(&mut self) {
        let pos = match self.cursor_pos {
            Some(p) => p,
            None => return,
        };
        if matches!(self.config.layout, LayoutMode::Sweep) {
            return;
        }
        let (hovered, nx, ny, cell_w, cell_h) = match self.cell_at(pos) {
            Some(v) => v,
            None => return,
        };
        // Reconstruct the cell's pixel origin so the overlay can draw the
        // rubber-band directly and end_box_zoom can compute normalized
        // coords without re-hitting the layout solver.
        let cell_left = pos.x as f32 - nx * cell_w;
        let cell_top = pos.y as f32 - (1.0 - ny) * cell_h;
        let targets = self.targets_for_channel(hovered);
        let seed = match targets.first().and_then(|&i| self.cell_views.get(i)) {
            Some(v) => *v,
            None => return,
        };
        let log_min = seed.freq_min.max(1.0).log10();
        let log_max = seed.freq_max.max(10.0).log10();
        self.box_zoom = Some(BoxZoomState {
            start: pos,
            current: pos,
            targets,
            cell_left_px: cell_left,
            cell_top_px: cell_top,
            cell_w_px: cell_w,
            cell_h_px: cell_h,
            start_log_min: log_min,
            start_log_max: log_max,
            start_db_min: seed.db_min,
            start_db_max: seed.db_max,
            start_rows_f: seed.rows_visible_f.max(1.0),
            waterfall: matches!(self.config.view_mode, ViewMode::Waterfall),
        });
    }

    pub(super) fn update_box_zoom(&mut self, pos: PhysicalPosition<f64>) {
        if let Some(bz) = self.box_zoom.as_mut() {
            bz.current = pos;
        }
    }

    /// Right-button release. Below the 5 px dead-zone the gesture is
    /// treated as a plain right-click and falls through to the legacy
    /// "reset hovered cell" behaviour. Above it, map the selected sub-rect
    /// (clamped to the cell bounds) back onto the view's freq axis plus
    /// either the dB axis (spectrum) or the waterfall time window.
    pub(super) fn end_box_zoom(&mut self) {
        let bz = match self.box_zoom.take() {
            Some(b) => b,
            None => return,
        };
        let dx = bz.current.x - bz.start.x;
        let dy = bz.current.y - bz.start.y;
        if dx * dx + dy * dy <= 25.0 {
            self.reset_hovered_view();
            return;
        }
        let data_log_min = theme::DEFAULT_FREQ_MIN.log10();
        let data_log_max = self.data_freq_ceiling.max(theme::DEFAULT_FREQ_MAX).log10();
        let data_ceiling = 10_f32.powf(data_log_max);
        let w = bz.cell_w_px.max(1.0);
        let h = bz.cell_h_px.max(1.0);
        let x0 = ((bz.start.x as f32 - bz.cell_left_px) / w).clamp(0.0, 1.0);
        let x1 = ((bz.current.x as f32 - bz.cell_left_px) / w).clamp(0.0, 1.0);
        // Screen Y grows downward; ny uses the same flip as cell_at so
        // ny=0 is the bottom of the plot and ny=1 is the top.
        let y0 = 1.0 - ((bz.start.y as f32 - bz.cell_top_px) / h).clamp(0.0, 1.0);
        let y1 = 1.0 - ((bz.current.y as f32 - bz.cell_top_px) / h).clamp(0.0, 1.0);
        let nx_lo = x0.min(x1);
        let nx_hi = x0.max(x1);
        let ny_lo = y0.min(y1);
        let ny_hi = y0.max(y1);
        let log_span = bz.start_log_max - bz.start_log_min;
        let new_log_min = (bz.start_log_min + nx_lo * log_span).max(data_log_min);
        let new_log_max = (bz.start_log_min + nx_hi * log_span).min(data_log_max);
        // Hard floor at ~0.15 decades so a micro-selection doesn't collapse
        // the freq axis to a sliver the user can't escape without Ctrl+R.
        let (new_log_min, new_log_max) = if (new_log_max - new_log_min) < 0.15 {
            let centre = 0.5 * (new_log_min + new_log_max);
            (
                (centre - 0.075).max(data_log_min),
                (centre + 0.075).min(data_log_max),
            )
        } else {
            (new_log_min, new_log_max)
        };
        let db_span = bz.start_db_max - bz.start_db_min;
        let new_db_min = (bz.start_db_min + ny_lo * db_span).max(-240.0);
        let new_db_max = (bz.start_db_min + ny_hi * db_span).min(20.0);
        let (new_db_min, new_db_max) = if (new_db_max - new_db_min) < 10.0 {
            let centre = 0.5 * (new_db_min + new_db_max);
            ((centre - 5.0).max(-240.0), (centre + 5.0).min(20.0))
        } else {
            (new_db_min, new_db_max)
        };
        // Waterfall/CWT: Y selection shrinks the visible time window. Top of
        // the cell is t=0 (newest); moving toward the bottom grows t_ago.
        // ny=1 top, ny=0 bottom — so t_ago spans from (1-ny_hi) to (1-ny_lo)
        // of the current rows_visible. Collapsing to the newest sliver is
        // only useful as a zoom-out primitive, so clamp to ≥2 rows.
        let rows_span = bz.start_rows_f;
        let new_rows_f = ((ny_hi - ny_lo) * rows_span).max(2.0);
        let max_rows = crate::render::waterfall::ROWS_PER_CHANNEL as f32;
        let new_rows_f = new_rows_f.min(max_rows);
        for &idx in &bz.targets {
            if let Some(view) = self.cell_views.get_mut(idx) {
                view.freq_min = 10.0_f32.powf(new_log_min).max(theme::DEFAULT_FREQ_MIN);
                view.freq_max = 10.0_f32.powf(new_log_max).min(data_ceiling);
                if bz.waterfall {
                    view.rows_visible_f = new_rows_f;
                    view.rows_visible = new_rows_f.round().clamp(2.0, max_rows) as u32;
                } else {
                    view.db_min = new_db_min;
                    view.db_max = new_db_max;
                }
            }
        }
        self.notify("zoom: box");
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
        )
    }

    fn current_w_slot(&self) -> Option<WSlot> {
        match (self.config.layout, self.config.view_mode, self.analysis_mode.as_str()) {
            (LayoutMode::Grid,   ViewMode::Spectrum,   _)     => Some(WSlot::Matrix),
            (LayoutMode::Single, ViewMode::Spectrum,   _)     => Some(WSlot::Single),
            (LayoutMode::Single, ViewMode::Waterfall, "fft")        => Some(WSlot::Waterfall),
            (LayoutMode::Single, ViewMode::Waterfall, "cwt")        => Some(WSlot::Cwt),
            (LayoutMode::Single, ViewMode::Waterfall, "cqt")        => Some(WSlot::Cqt),
            (LayoutMode::Single, ViewMode::Waterfall, "reassigned") => Some(WSlot::Reassigned),
            (LayoutMode::Single, ViewMode::Scope,         _)         => Some(WSlot::Scope),
            (LayoutMode::Single, ViewMode::SpectrumEmber, _)         => Some(WSlot::SpectrumEmber),
            (LayoutMode::Single, ViewMode::Goniometer,    _)         => Some(WSlot::Goniometer),
            (LayoutMode::Single, ViewMode::IoTransfer,    _)         => Some(WSlot::IoTransfer),
            (LayoutMode::Single, ViewMode::BodeMag,       _)         => Some(WSlot::BodeMag),
            (LayoutMode::Single, ViewMode::Coherence,     _)         => Some(WSlot::Coherence),
            (LayoutMode::Single, ViewMode::BodePhase,     _)         => Some(WSlot::BodePhase),
            (LayoutMode::Single, ViewMode::GroupDelay,    _)         => Some(WSlot::GroupDelay),
            (LayoutMode::Single, ViewMode::Nyquist,       _)         => Some(WSlot::Nyquist),
            _ => None,
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
            KeyCode::Space => {
                self.toggle_selection();
            }
            KeyCode::KeyH => {
                self.show_help = !self.show_help;
            }
            KeyCode::KeyS => {
                self.pending_screenshot = true;
            }
            KeyCode::KeyC => {
                // Jump into Compare on selected channels. Nothing selected →
                // no-op so an accidental press doesn't swap the user out of
                // their current view into an empty Compare grid.
                if !self.selected.iter().any(|s| *s) {
                    self.notify("C: select ≥ 1 channel first (Space over cell)");
                    return;
                }
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
            KeyCode::KeyW => {
                // W cycles the six canonical views:
                //   Matrix (Grid, spectrum)       → many channels at a glance
                //   Single (spectrum)             → one channel, FFT
                //   Waterfall (Single, FFT)       → one channel, time × freq (FFT)
                //   CWT (Single, Morlet)          → one channel, time × freq (CWT)
                //   CQT (Single, constant-Q)      → one channel, time × freq (CQT)
                //   Reassigned (Single, AF-STFT)  → one channel, time × freq (reassigned)
                // Non-cycled layouts (Compare / Transfer / Sweep) jump back to
                // Matrix so the key always advances deterministically.
                let next = match self.current_w_slot() {
                    Some(WSlot::Matrix)        => WSlot::Single,
                    Some(WSlot::Single)        => WSlot::Waterfall,
                    Some(WSlot::Waterfall)     => WSlot::Cwt,
                    Some(WSlot::Cwt)           => WSlot::Cqt,
                    Some(WSlot::Cqt)           => WSlot::Reassigned,
                    Some(WSlot::Reassigned)    => WSlot::Scope,
                    Some(WSlot::Scope)         => WSlot::SpectrumEmber,
                    Some(WSlot::SpectrumEmber) => WSlot::Goniometer,
                    Some(WSlot::Goniometer)    => WSlot::IoTransfer,
                    Some(WSlot::IoTransfer)    => WSlot::BodeMag,
                    Some(WSlot::BodeMag)       => WSlot::Coherence,
                    Some(WSlot::Coherence)     => WSlot::BodePhase,
                    Some(WSlot::BodePhase)     => WSlot::GroupDelay,
                    Some(WSlot::GroupDelay)    => WSlot::Nyquist,
                    Some(WSlot::Nyquist)       => WSlot::Matrix,
                    None                       => WSlot::Matrix,
                };
                let (layout, view_mode, mode, label) = match next {
                    WSlot::Matrix        => (LayoutMode::Grid,   ViewMode::Spectrum,      "fft",        "view: matrix"),
                    WSlot::Single        => (LayoutMode::Single, ViewMode::Spectrum,      "fft",        "view: single"),
                    WSlot::Waterfall     => (LayoutMode::Single, ViewMode::Waterfall,     "fft",        "view: waterfall (fft)"),
                    WSlot::Cwt           => (LayoutMode::Single, ViewMode::Waterfall,     "cwt",        "view: waterfall (cwt)"),
                    WSlot::Cqt           => (LayoutMode::Single, ViewMode::Waterfall,     "cqt",        "view: waterfall (cqt)"),
                    WSlot::Reassigned    => (LayoutMode::Single, ViewMode::Waterfall,     "reassigned", "view: waterfall (reassigned)"),
                    WSlot::Scope         => (LayoutMode::Single, ViewMode::Scope,         "fft",        "view: scope (ember)"),
                    WSlot::SpectrumEmber => (LayoutMode::Single, ViewMode::SpectrumEmber, "fft",        "view: spectrum (ember)"),
                    WSlot::Goniometer    => (LayoutMode::Single, ViewMode::Goniometer,    "fft",        "view: goniometer (ember)"),
                    WSlot::IoTransfer    => (LayoutMode::Single, ViewMode::IoTransfer,    "fft",        "view: iotransfer (ember)"),
                    WSlot::BodeMag       => (LayoutMode::Single, ViewMode::BodeMag,       "fft",        "view: bode mag (ember)"),
                    WSlot::Coherence     => (LayoutMode::Single, ViewMode::Coherence,     "fft",        "view: coherence (ember)"),
                    WSlot::BodePhase     => (LayoutMode::Single, ViewMode::BodePhase,     "fft",        "view: bode phase (ember)"),
                    WSlot::GroupDelay    => (LayoutMode::Single, ViewMode::GroupDelay,    "fft",        "view: group delay (ember)"),
                    WSlot::Nyquist       => (LayoutMode::Single, ViewMode::Nyquist,       "fft",        "view: nyquist (ember)"),
                };
                if self.analysis_mode != mode && !self.send_set_analysis_mode(mode) {
                    // Daemon refused the analysis-mode change — stay put so
                    // the next W press keeps meaning "advance".
                    return;
                }
                let prev_view = self.config.view_mode;
                self.config.layout = layout;
                self.config.view_mode = view_mode;
                if matches!(view_mode, ViewMode::Waterfall) {
                    for init in &mut self.waterfall_inited {
                        *init = false;
                    }
                    // Wipe the history texture so old rows from the
                    // previous analysis source (e.g. FFT) don't bleed
                    // into the new view (e.g. CWT). Each W press into
                    // a Waterfall sub-mode starts with a clean slate
                    // tied to the measurement at hand.
                    if let (Some(ctx), Some(wf)) =
                        (self.render_ctx.as_ref(), self.waterfall.as_mut())
                    {
                        wf.clear_history(&ctx.queue);
                    }
                }
                // Apply the per-view default dB window when the view
                // family actually changes (line-plot ↔ colormap), so
                // cycling W lands on a sane gain instead of the user
                // re-tweaking every time. Keep the cell's current
                // window when staying within the same family — they
                // share the same default and the user may have
                // adjusted intentionally.
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
            KeyCode::BracketLeft => {
                self.shift_hovered_db_floor(-5.0);
            }
            KeyCode::BracketRight => {
                self.shift_hovered_db_floor(5.0);
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
