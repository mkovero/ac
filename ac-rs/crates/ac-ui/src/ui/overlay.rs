use egui::{Align2, Color32, Context, CornerRadius, FontId, Pos2, Rect, Stroke, StrokeKind};

use crate::data::smoothing;
use crate::data::types::{
    CellView, DisplayConfig, DisplayFrame, LayoutMode, TransferFrame, TransferPair, ViewMode,
};
use crate::render::waterfall::COLORMAP_LUT;
use crate::theme;
use crate::ui::fmt::format_hz;
use crate::ui::stats::StatsSnapshot;

#[derive(Clone)]
pub struct HoverInfo {
    pub channel: usize,
    pub rect:    Rect,
    pub cursor:  Pos2,
    pub freq_hz: f32,
    pub readout: HoverReadout,
}

/// The y-axis reading under the hover cursor. Non-transfer layouts always
/// emit `Db`; Transfer layout classifies which sub-panel the cursor is in and
/// emits `Phase` (deg) or `Coherence` (0..1) accordingly.
#[derive(Debug, Clone, Copy)]
pub enum HoverReadout {
    Db(f32),
    Phase(f32),
    Coherence(f32),
    Thd(f32),
    Gain(f32),
}

pub struct OverlayInput<'a> {
    pub config: &'a DisplayConfig,
    pub frames: &'a [Option<DisplayFrame>],
    pub cell_views: &'a [CellView],
    pub selected: &'a [bool],
    pub selection_order: &'a [usize],
    pub transfer: Option<&'a TransferFrame>,
    /// Currently active meas channel in the Transfer layout (after clamping
    /// `active_meas_idx` to the meas list). `None` when the layout isn't
    /// Transfer or the selection has fewer than 2 channels.
    pub active_meas: Option<usize>,
    /// Index into the meas list (`selection_order[..len-1]`) that the user
    /// last cycled to via Tab. Carried through so the overlay can show
    /// "MEAS (n/N)" when there are multiple meas channels.
    pub active_meas_idx: usize,
    pub connected: bool,
    pub notification: Option<&'a str>,
    pub timing: Option<StatsSnapshot>,
    pub gpu_supported: bool,
    pub hover: Option<HoverInfo>,
    pub show_help: bool,
    /// Live FFT monitor knobs — shown in the Spectrum top-right stack when in
    /// FFT mode so the reader sees `Left/Right` and `Up/Down` arrow effects
    /// directly (tick cadence, FFT N, resulting Δf). `None` suppresses the
    /// line (CWT mode or no spectrum frame yet).
    pub monitor_params: Option<MonitorParamsInfo>,
    /// Number of real capture channels. Channel indices `< n_real` are
    /// regular captures and label as `CHn`; indices `>= n_real` are virtual
    /// transfer channels and label via `virtual_pairs`.
    pub n_real: usize,
    /// Parallel to cells `n_real..n_real + virtual_pairs.len()`. An entry
    /// `i` corresponds to the cell at channel index `n_real + i`.
    pub virtual_pairs: &'a [TransferPair],
    /// Active waterfall palette row (0 = inferno, 1 = viridis, 2 = magma,
    /// 3 = plasma). The colorbar in the top-right samples this row of the
    /// baked LUT so the legend matches what the GPU is actually rendering.
    pub active_palette: u32,
    /// Current fractional-octave smoothing denominator. `None` when the user
    /// has toggled smoothing off. Shown as a small status tag in the top-right
    /// so the reader knows the trace they're looking at has been averaged.
    pub smoothing_frac: Option<u32>,
    /// Fractional-octave aggregation bins-per-octave for CWT view. `None`
    /// = disabled. `Some(N)` = the spectrum the user sees is the daemon's
    /// `fractional_octave` aggregation of the CWT column. Shown alongside
    /// `smooth` so the reader can tell raw CWT from per-band aggregation.
    pub ioct_bpo: Option<u32>,
    /// Tier 2 technique badge — one-line label describing the active
    /// live-analysis method (e.g. `"FFT · N=16384 · Hann"`). `None`
    /// suppresses the line; callers set it when the current view
    /// corresponds to a live-analysis frame. See `ARCHITECTURE.md`.
    pub tier_badge: Option<String>,
}

/// Label used in overlays / legends for a given cell index. Real channels
/// stay `CHn`; virtual transfer cells read as `M{m}←R{r}` so the pair is
/// visible at a glance.
pub(crate) fn channel_label(idx: usize, n_real: usize, virtual_pairs: &[TransferPair]) -> String {
    if idx < n_real {
        format!("CH{idx}")
    } else {
        let vi = idx - n_real;
        // Use a stable display name (`transfer{n}`) so these read as distinct
        // from the raw audio channels. The pair mapping is still visible in
        // the hover readout and the T-press toast.
        let _ = virtual_pairs.get(vi);
        format!("transfer{vi}")
    }
}

#[derive(Clone, Copy)]
pub struct MonitorParamsInfo {
    pub interval_ms: u32,
    pub fft_n: u32,
}

const HELP_LINES: &[&str] = &[
    "Keybindings",
    "─────────────────────────────",
    "Esc / Q        quit",
    "Enter          freeze",
    "S              screenshot + CSV",
    "W              cycle view (spec/water)",
    "L              cycle layout (grid/sng/cmp*/xfer*)",
    "F              fullscreen",
    "D              timing overlay",
    "H              toggle this help",
    "Space          select channel",
    "Tab            next page / channel",
    "Shift+Tab      prev page / channel",
    "[ / ]          shift dB floor",
    "+ / -          adjust dB range",
    "← / →          monitor interval (fft)",
    "↑ / ↓          fft N (fft)",
    "Ctrl+R         reset all views",
    "",
    "Mouse",
    "─────────────────────────────",
    "Scroll (cell)  zoom both axes",
    "Scroll (bg)    resize grid cells",
    "Shift+Scroll   zoom dB (gain)",
    "Ctrl+Scroll    zoom freq (spec) / time (water)",
    "Left-drag      pan",
    "Right-click    reset hovered cell",
];

pub fn draw(ctx: &Context, input: OverlayInput<'_>) {
    let screen = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("ac-ui-overlay"),
    ));

    let text_color = Color32::from_rgb(theme::TEXT[0], theme::TEXT[1], theme::TEXT[2]);
    let clip_color = Color32::from_rgb(
        theme::CLIP_LED[0],
        theme::CLIP_LED[1],
        theme::CLIP_LED[2],
    );

    let display_ch = match input.config.layout {
        LayoutMode::Single => input.config.active_channel,
        _ => 0,
    };

    let primary = input.frames.get(display_ch).and_then(|f| f.as_ref());
    let anyclip = input.frames.iter().flatten().any(|f| f.meta.clipping);

    if let Some(frame) = primary {
        let sr = frame.meta.sr;
        let view = input
            .cell_views
            .get(display_ch)
            .copied()
            .unwrap_or_default();
        let top_right = super::fmt::top_right_status(
            sr,
            &channel_label(display_ch, input.n_real, input.virtual_pairs),
        );
        painter.text(
            Pos2::new(screen.right() - 8.0, screen.top() + 6.0),
            Align2::RIGHT_TOP,
            top_right,
            FontId::monospace(theme::STATUS_PX),
            text_color,
        );
        // Gain / zoom indicator: show the active cell's dB and frequency
        // windows directly below the sample-rate line. In Spectrum mode the
        // dB range is the Y axis; in Waterfall mode it's the colormap range.
        // Smoothing state is appended as a compact tag ("│ smooth 1/6 oct")
        // whenever fractional-octave smoothing is active, so the reader sees
        // at a glance that the trace isn't raw FFT grass.
        let smooth_tag = match input.smoothing_frac {
            Some(_) => format!("  │  smooth {}", smoothing::label(input.smoothing_frac)),
            None => String::new(),
        };
        let ioct_tag = match input.ioct_bpo {
            Some(n) => format!("  │  ioct 1/{n} oct"),
            None => String::new(),
        };
        let gain_line = match input.config.view_mode {
            ViewMode::Spectrum => format!(
                "Y {:.0}..{:.0} dB  │  {}..{}{}{}",
                view.db_min,
                view.db_max,
                format_hz(view.freq_min).trim(),
                format_hz(view.freq_max).trim(),
                smooth_tag,
                ioct_tag,
            ),
            ViewMode::Waterfall => format!(
                "color {:.0}..{:.0} dB  │  {}..{}{}{}",
                view.db_min,
                view.db_max,
                format_hz(view.freq_min).trim(),
                format_hz(view.freq_max).trim(),
                smooth_tag,
                ioct_tag,
            ),
        };
        painter.text(
            Pos2::new(screen.right() - 8.0, screen.top() + 6.0 + theme::STATUS_PX + 2.0),
            Align2::RIGHT_TOP,
            gain_line,
            FontId::monospace(theme::STATUS_PX),
            text_color,
        );

        // Live FFT monitor readout (Spectrum + FFT only). Shows the knobs
        // adjusted by plain Left/Right and Up/Down so the user sees the
        // effect of each key press directly.
        let mut stack_row: f32 = 2.0;
        if matches!(input.config.view_mode, ViewMode::Spectrum) {
            if let Some(mp) = input.monitor_params {
                let mon_line = super::fmt::monitor_knobs_readout(mp.interval_ms, mp.fft_n, sr);
                painter.text(
                    Pos2::new(
                        screen.right() - 8.0,
                        screen.top() + 6.0 + stack_row * (theme::STATUS_PX + 2.0),
                    ),
                    Align2::RIGHT_TOP,
                    mon_line,
                    FontId::monospace(theme::STATUS_PX),
                    text_color,
                );
                stack_row += 1.0;
            }
        }
        if let Some(badge) = input.tier_badge.as_deref() {
            painter.text(
                Pos2::new(
                    screen.right() - 8.0,
                    screen.top() + 6.0 + stack_row * (theme::STATUS_PX + 2.0),
                ),
                Align2::RIGHT_TOP,
                badge,
                FontId::monospace(theme::STATUS_PX),
                text_color,
            );
        }

        // Waterfall colorbar: vertical gradient sampled from the same
        // inferno LUT the GPU uses, with dB tick labels every 20 dB. Anchored
        // under the gain line so the reader sees "color X..Y dB" above and
        // the actual scale below.
        if matches!(input.config.view_mode, ViewMode::Waterfall) {
            let bar_top = screen.top() + 6.0 + 2.0 * (theme::STATUS_PX + 2.0) + 6.0;
            let bar_h = 120.0_f32;
            let bar_w = 12.0_f32;
            let label_col_w = 40.0_f32;
            let bar_right = screen.right() - 8.0 - label_col_w;
            let bar_left = bar_right - bar_w;
            let strips = 48_usize;
            // COLORMAP_LUT is laid out as `[palette 0 row, palette 1 row, …]`,
            // each row 256 RGBA8 texels. Offset into the active row so the
            // legend follows Alt+Scroll palette cycling.
            let palette_off = (input.active_palette as usize) * 256 * 4;
            for i in 0..strips {
                // Top strip = max dB (hottest) so the bar visually matches
                // the "loud up, quiet down" mental model.
                let t = 1.0 - (i as f32 + 0.5) / strips as f32;
                let lut_idx = ((t * 255.0).round() as usize).min(255);
                let off = palette_off + lut_idx * 4;
                let color = Color32::from_rgb(
                    COLORMAP_LUT[off],
                    COLORMAP_LUT[off + 1],
                    COLORMAP_LUT[off + 2],
                );
                let y0 = bar_top + (i as f32) * bar_h / strips as f32;
                let y1 = bar_top + (i as f32 + 1.0) * bar_h / strips as f32;
                painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(bar_left, y0),
                        Pos2::new(bar_right, y1),
                    ),
                    CornerRadius::ZERO,
                    color,
                );
            }
            painter.rect_stroke(
                Rect::from_min_max(
                    Pos2::new(bar_left, bar_top),
                    Pos2::new(bar_right, bar_top + bar_h),
                ),
                CornerRadius::ZERO,
                Stroke::new(1.0, text_color),
                StrokeKind::Inside,
            );
            // Labels: db_max at top, db_min at bottom, ~3 ticks between.
            let tick_dbs = [
                view.db_max,
                view.db_min + (view.db_max - view.db_min) * 0.75,
                view.db_min + (view.db_max - view.db_min) * 0.50,
                view.db_min + (view.db_max - view.db_min) * 0.25,
                view.db_min,
            ];
            for (i, db) in tick_dbs.iter().enumerate() {
                let t = i as f32 / (tick_dbs.len() as f32 - 1.0);
                let y = bar_top + t * bar_h;
                painter.text(
                    Pos2::new(bar_right + 4.0, y),
                    Align2::LEFT_CENTER,
                    format!("{:+.0}", db),
                    FontId::monospace(theme::GRID_LABEL_PX),
                    text_color,
                );
            }
        }

        // Broadband stats derived from the displayed spectrum — honest for
        // any input (music, speech, noise, room response). Falls back
        // gracefully when the frame arrived with an empty spectrum.
        if let Some(stats) = super::fmt::broadband_stats(&frame.spectrum, &frame.freqs) {
            let bottom_left = super::fmt::spectrum_readout(&stats, frame.meta.in_dbu);
            painter.text(
                Pos2::new(screen.left() + 8.0, screen.bottom() - 6.0),
                Align2::LEFT_BOTTOM,
                bottom_left,
                FontId::monospace(theme::READOUT_PX),
                text_color,
            );
        }
    }

    let conn_label = if input.connected {
        ("● connected", text_color)
    } else {
        ("● disconnected", clip_color)
    };
    painter.text(
        Pos2::new(screen.right() - 8.0, screen.bottom() - 6.0),
        Align2::RIGHT_BOTTOM,
        conn_label.0,
        FontId::monospace(theme::STATUS_PX),
        conn_label.1,
    );

    if anyclip {
        painter.text(
            Pos2::new(screen.center().x, screen.top() + 6.0),
            Align2::CENTER_TOP,
            "CLIP",
            FontId::monospace(theme::READOUT_PX),
            clip_color,
        );
    }

    if input.config.frozen {
        painter.text(
            Pos2::new(screen.center().x, screen.top() + 22.0),
            Align2::CENTER_TOP,
            "FROZEN",
            FontId::monospace(theme::STATUS_PX),
            text_color,
        );
    }

    if let Some(note) = input.notification {
        painter.text(
            screen.center(),
            Align2::CENTER_CENTER,
            note,
            FontId::monospace(theme::READOUT_PX),
            text_color,
        );
    }

    if matches!(input.config.layout, LayoutMode::Transfer) {
        let n_sel = input.selection_order.len();
        if let Some(&refc) = input.selection_order.last().filter(|_| n_sel >= 2) {
            let meas_ch = input.active_meas;
            if let Some(tf) = input.transfer {
                let delay_text = super::fmt::transfer_delay(tf.delay_ms, tf.delay_samples);
                painter.text(
                    Pos2::new(screen.center().x, screen.top() + 10.0),
                    Align2::CENTER_TOP,
                    delay_text,
                    FontId::monospace(theme::READOUT_PX * 1.4),
                    text_color,
                );
                let meas_count = n_sel - 1;
                let active_idx = input.active_meas_idx.min(meas_count.saturating_sub(1));
                let x0 = screen.left() + 12.0;
                let mut y = screen.top() + 12.0;
                let row_h = theme::READOUT_PX + 4.0;
                let meas_label = if meas_count > 1 {
                    format!("MEAS ({}/{})", active_idx + 1, meas_count)
                } else {
                    "MEAS".to_string()
                };
                let entries: [(String, usize); 2] = [
                    (meas_label, meas_ch.unwrap_or(0)),
                    ("REF".to_string(), refc),
                ];
                for (label, ch) in &entries {
                    let rgba = theme::channel_color(*ch);
                    let swatch = Color32::from_rgb(
                        (rgba[0] * 255.0) as u8,
                        (rgba[1] * 255.0) as u8,
                        (rgba[2] * 255.0) as u8,
                    );
                    painter.rect_filled(
                        Rect::from_min_size(Pos2::new(x0, y + 2.0), egui::vec2(12.0, 12.0)),
                        CornerRadius::ZERO,
                        swatch,
                    );
                    painter.text(
                        Pos2::new(x0 + 18.0, y),
                        Align2::LEFT_TOP,
                        format!("{label}: CH{ch}"),
                        FontId::monospace(theme::READOUT_PX),
                        text_color,
                    );
                    y += row_h;
                }
            } else {
                painter.text(
                    screen.center(),
                    Align2::CENTER_CENTER,
                    "waiting for transfer_stream…",
                    FontId::monospace(theme::READOUT_PX),
                    text_color,
                );
            }
        } else {
            painter.text(
                screen.center(),
                Align2::CENTER_CENTER,
                "Select ≥ 2 channels — last pick is REF (Tab cycles MEAS)",
                FontId::monospace(theme::READOUT_PX),
                text_color,
            );
        }
    }

    if matches!(input.config.layout, LayoutMode::Compare) {
        let any_selected = input.selected.iter().any(|s| *s);
        if !any_selected {
            painter.text(
                screen.center(),
                Align2::CENTER_CENTER,
                "compare mode — press Space to select channels",
                FontId::monospace(theme::READOUT_PX),
                text_color,
            );
        } else {
            let x0 = screen.left() + 12.0;
            let mut y = screen.top() + 12.0;
            let row_h = theme::READOUT_PX + 4.0;
            for (i, &sel) in input.selected.iter().enumerate() {
                if !sel {
                    continue;
                }
                let rgba = theme::channel_color(i);
                let swatch = Color32::from_rgb(
                    (rgba[0] * 255.0) as u8,
                    (rgba[1] * 255.0) as u8,
                    (rgba[2] * 255.0) as u8,
                );
                let swatch_rect = Rect::from_min_size(
                    Pos2::new(x0, y + 2.0),
                    egui::vec2(12.0, 12.0),
                );
                painter.rect_filled(swatch_rect, CornerRadius::ZERO, swatch);
                painter.text(
                    Pos2::new(x0 + 18.0, y),
                    Align2::LEFT_TOP,
                    channel_label(i, input.n_real, input.virtual_pairs),
                    FontId::monospace(theme::READOUT_PX),
                    text_color,
                );
                y += row_h;
            }
        }
    }

    if let Some(hover) = input.hover.as_ref() {
        let crosshair = Stroke::new(
            1.0,
            Color32::from_rgba_unmultiplied(255, 255, 255, (0.55 * 255.0) as u8),
        );
        painter.line_segment(
            [
                Pos2::new(hover.rect.left(), hover.cursor.y),
                Pos2::new(hover.rect.right(), hover.cursor.y),
            ],
            crosshair,
        );
        painter.line_segment(
            [
                Pos2::new(hover.cursor.x, hover.rect.top()),
                Pos2::new(hover.cursor.x, hover.rect.bottom()),
            ],
            crosshair,
        );
        let label = super::fmt::hover_label(hover.channel, hover.freq_hz, &hover.readout);
        // Pin the readout just above-right of the cursor, clamped so it
        // stays inside the hovered cell.
        let anchor = Pos2::new(
            (hover.cursor.x + 8.0).min(hover.rect.right() - 4.0),
            (hover.cursor.y - 8.0).max(hover.rect.top() + 4.0),
        );
        painter.text(
            anchor,
            Align2::LEFT_BOTTOM,
            label,
            FontId::monospace(theme::READOUT_PX),
            text_color,
        );
    }

    if let Some(snap) = input.timing {
        let gpu = snap.gpu;
        let line1 = format!(
            "fps {:>5.1}   frame {:>5.2} ms   p95 {:>5.2}   p99 {:>5.2}",
            snap.fps, snap.frame_mean_ms, snap.frame_p95_ms, snap.frame_p99_ms,
        );
        let line2 = if input.gpu_supported {
            format!(
                "cpu {:>5.2} ms   gpu {:>5.2}   spec {:>5.2}   egui {:>5.2}",
                snap.cpu_mean_ms, gpu.gpu_ms, gpu.spectrum_ms, gpu.egui_ms,
            )
        } else {
            format!("cpu {:>5.2} ms   gpu n/a (TIMESTAMP_QUERY unsupported)", snap.cpu_mean_ms)
        };
        let x = screen.left() + 8.0;
        let y0 = screen.top() + 8.0;
        let dy = theme::READOUT_PX + 2.0;
        painter.text(Pos2::new(x, y0),        Align2::LEFT_TOP, line1, FontId::monospace(theme::READOUT_PX), text_color);
        painter.text(Pos2::new(x, y0 + dy),   Align2::LEFT_TOP, line2, FontId::monospace(theme::READOUT_PX), text_color);
    }

    if input.show_help {
        let line_h = theme::READOUT_PX + 4.0;
        let pad = 16.0;
        let panel_w = 380.0;
        let panel_h = HELP_LINES.len() as f32 * line_h + pad * 2.0;
        let panel = Rect::from_center_size(
            screen.center(),
            egui::vec2(panel_w, panel_h),
        );
        painter.rect_filled(
            panel,
            CornerRadius::same(4),
            Color32::from_rgba_unmultiplied(0, 0, 0, 220),
        );
        let border = Color32::from_rgb(
            theme::SELECT_BORDER[0],
            theme::SELECT_BORDER[1],
            theme::SELECT_BORDER[2],
        );
        painter.rect_stroke(
            panel,
            CornerRadius::same(4),
            Stroke::new(1.0, border),
            StrokeKind::Inside,
        );
        let mut y = panel.top() + pad;
        for line in HELP_LINES {
            painter.text(
                Pos2::new(panel.left() + pad, y),
                Align2::LEFT_TOP,
                *line,
                FontId::monospace(theme::READOUT_PX),
                text_color,
            );
            y += line_h;
        }
    }
}

