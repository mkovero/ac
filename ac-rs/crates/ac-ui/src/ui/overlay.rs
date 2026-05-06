use egui::{Align2, Color32, Context, CornerRadius, FontId, Pos2, Rect, Stroke, StrokeKind};

use crate::data::smoothing;
use crate::data::types::{
    CellView, DisplayConfig, DisplayFrame, LayoutMode, LoudnessReadout, StereoStatus, TransferPair,
    ViewMode,
};
use crate::render::waterfall::COLORMAP_LUT;
use crate::theme;
use crate::ui::fmt::format_hz;
use crate::ui::keytips::KeytipChip;
use crate::ui::stats::StatsSnapshot;

#[derive(Clone)]
pub struct HoverInfo {
    pub channel: usize,
    pub rect:    Rect,
    pub cursor:  Pos2,
    pub freq_hz: f32,
    pub readout: HoverReadout,
}

/// The y-axis reading under the hover cursor. Spectrum/Compare emit `Db`;
/// Sweep layout classifies the cursor panel (THD vs Gain vs spectrum detail);
/// Waterfall/CWT emit seconds-ago instead of dB.
#[derive(Debug, Clone, Copy)]
pub enum HoverReadout {
    Db(f32),
    Thd(f32),
    Gain(f32),
    /// Waterfall/CWT cursor Y-axis is time, not dB. Payload is seconds-ago
    /// (0 at the top, newest row; grows downward toward older rows).
    TimeAgo(f32),
}

/// Colour hint for a single line of the loudness overlay — lets the R128
/// pass/fail state bleed through without hardcoding colours in the
/// formatter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoudnessTint {
    Default,
    Good,
    Warn,
    Bad,
}

/// One pre-rendered line of the loudness overlay, tagged with the colour
/// the painter should use. `Default` → regular overlay grey; the R128
/// variants colour the integrated-LKFS line.
pub struct LoudnessLine {
    pub text: String,
    pub tint: LoudnessTint,
}

pub struct OverlayInput<'a> {
    pub config: &'a DisplayConfig,
    pub frames: &'a [Option<DisplayFrame>],
    pub cell_views: &'a [CellView],
    pub selected: &'a [bool],
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
    /// Active waterfall palette row (0 = inferno, 1 = magma).
    /// The colorbar in the top-right samples this row of the
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
    /// Per-band time-integration status. `None` = off. When a mode is
    /// active, rendered alongside `smooth` / `ioct` as a `time` tag so
    /// the reader knows the trace has been integrated (EMA fast/slow)
    /// or accumulated (Leq) rather than shown instantaneously.
    pub time_integration: Option<TimeIntegrationOverlay>,
    /// Per-band frequency weighting — `"A"`, `"C"`, or `"Z"`. `None`
    /// when the user hasn't picked a curve (`Off` in the app state).
    /// Rendered as a `wt A` tag alongside the other per-band tags so
    /// the reader can distinguish a weighted trace at a glance.
    pub band_weighting: Option<&'static str>,
    /// Latest BS.1770-5 / R128 meter readout for the active hover / single
    /// channel. `None` suppresses the loudness status row entirely (either
    /// no monitor is running, or the channel hasn't received any frames
    /// yet). Rendered under the live-FFT-monitor line.
    pub loudness: Option<LoudnessReadout>,
    /// Transfer-derived views (BodeMag/Coherence/BodePhase/GroupDelay/
    /// Nyquist/IR): the registered pair the dispatch arm resolved for
    /// the current `active_channel`. `None` means no pair is
    /// registered yet — the caption hints at the Space+T workflow.
    pub bode_pair: Option<TransferPair>,
    /// Goniometer source state — drives the status caption
    /// so the reader sees whether the figure is real audio (and which
    /// physical channels) or one of the synthetic-fallback variants.
    /// Computed at the dispatch site each render frame; defaults to
    /// `NoAudio` for non-trajectory views.
    pub gonio_state: StereoStatus,
    /// Bottom keytip strip — RC-8, plan §4. The 3–6 contextual chips
    /// for the current view plus the universal `H help / S screenshot
    /// / Esc quit`. Built by `crate::ui::keytips::keytips_for` against
    /// the live state snapshot at render-pipeline time.
    pub keytips: &'a [KeytipChip],
}

/// Format the goniometer status line. `view_label` is the short view
/// name interpolated into every variant of the message; today only
/// Goniometer uses this — PhaseScope3D was dropped in favour of
/// keeping the substrate simple.
fn format_stereo_status_line(view_label: &str, status: StereoStatus) -> String {
    match status {
        StereoStatus::Real { l, r } => {
            format!("{view_label} (ember) │ ch {l} + {r}")
        }
        StereoStatus::NoTransferPair => format!(
            "{view_label} (ember) │ synthetic — Space-select L + R, then T",
        ),
        StereoStatus::NotStreamingYet { l, r } => format!(
            "{view_label} (ember) │ synthetic — daemon not streaming scope yet (ch {l}+{r})"
        ),
        StereoStatus::NoAudio => {
            format!("{view_label} (ember) │ synthetic 1 kHz + 0.3 Hz phase walk")
        }
    }
}

/// Format the IoTransfer status line. Reuses StereoStatus — the
/// `Real { l, r }` variant carries `l = ref-input channel` and
/// `r = DUT-output channel`.
fn format_iotransfer_status_line(status: StereoStatus) -> String {
    match status {
        StereoStatus::Real { l, r } => {
            format!("iotransfer (ember) │ ref ch {l} → dut ch {r}")
        }
        StereoStatus::NoTransferPair => {
            "iotransfer (ember) │ synthetic — Space-select REF + DUT, then T".to_string()
        }
        StereoStatus::NotStreamingYet { l, r } => format!(
            "iotransfer (ember) │ synthetic — daemon not streaming scope yet (ref ch {l} → dut ch {r})"
        ),
        StereoStatus::NoAudio => {
            "iotransfer (ember) │ synthetic — no daemon source".to_string()
        }
    }
}

/// Format the BodeMag / Coherence status line. `view_label` is the
/// short view name; `pair` carries the meas+ref channel ids the
/// dispatch arm resolved (None when no transfer pair is registered).
/// y range is appended for at-a-glance scale: dB window for Bode,
/// [0,1] for coherence.
fn format_transfer_status_line(
    view_label: &str,
    pair: Option<TransferPair>,
    y_min: f32,
    y_max: f32,
) -> String {
    match pair {
        Some(p) => format!(
            "{view_label} (ember) │ meas ch {} → ref ch {}  │  Y {:.0}..{:.0}",
            p.meas, p.ref_ch, y_min, y_max,
        ),
        None => format!(
            "{view_label} (ember) │ no transfer pair — Space-select MEAS + REF, then T",
        ),
    }
}

/// Overlay payload for the per-band time-integration status.
#[derive(Debug, Clone)]
pub struct TimeIntegrationOverlay {
    /// Short label for the keyed mode (`"fast"`, `"slow"`, `"Leq"`).
    pub mode: &'static str,
    /// EMA time constant in seconds. `None` for Leq.
    pub tau_s: Option<f64>,
    /// Leq-accumulator wall-clock duration in seconds, if the mode is
    /// Leq and the daemon reported it on the most recent frame.
    pub duration_s: Option<f64>,
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

// Help overlay — RC-9 trim, ≤30 lines, organized by view family. The
// per-view chip strip at the bottom (RC-8) is the primary cheat sheet;
// this panel is the reference card for everything not on the strip.
const HELP_LINES: &[&str] = &[
    "Keybindings — H to toggle",
    "─────────────────────────────",
    "Esc / Q        quit",
    "S              screenshot",
    "F              fullscreen",
    "D              timing overlay",
    "W              cycle ember view",
    "G              grid ↔ single (per-channel views)",
    "Enter          freeze",
    "Space          toggle channel select",
    "Tab / Sh+Tab   next / prev channel",
    "T              add virtual transfer",
    "C              compare selected",
    "Ctrl+R         reset all views",
    "",
    "Spectrum / Waterfall",
    "─────────────────────────────",
    "A              weighting: off / A / C / Z",
    "I / Sh+I       time integration / Leq reset",
    "O / Sh+O       smoothing / CWT 1/N-oct",
    "P / M          peak / min hold",
    ";              cycle waterfall palette",
    "↑↓ / ←→        FFT N / interval (fft)",
    "Sh+↑↓ / Sh+←→  CWT sigma / scales (cwt)",
    "Sh+L           reset BS.1770 loudness",
    "",
    "Trajectory / Transfer",
    "─────────────────────────────",
    "R              goniometer M/S toggle",
    ",/. / Sh+,/.   ember intensity / τ_p",
    "K              coherence γ²-weight ±",
    "",
    "Mouse",
    "─────────────────────────────",
    "Scroll         zoom (Sh=freq, Ctrl=Y)",
    "Ctrl+Sh+Scroll dB window pan",
    "Drag           pan freq/dB",
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
        let wt_tag = match input.band_weighting {
            Some(t) => format!("  │  wt {t}"),
            None => String::new(),
        };
        let time_tag = match input.time_integration.as_ref() {
            None => String::new(),
            Some(t) => match (t.mode, t.tau_s, t.duration_s) {
                (mode, Some(tau), _) => format!(
                    "  │  time {mode} τ={:.0} ms",
                    tau * 1000.0,
                ),
                (mode, None, Some(d)) if d.is_finite() => {
                    format!("  │  {mode} {:.1} s", d)
                }
                (mode, _, _) => format!("  │  {mode}"),
            },
        };
        // Mic-curve tag: surfaced loudly so the user knows the spectrum
        // they're looking at has been frequency-corrected (or not). Three
        // states match the daemon's `mic_correction` field.
        let mic_tag = match frame.meta.mic_correction.as_deref() {
            Some("on")  => "  │  mic-cal".to_string(),
            Some("off") => "  │  mic-cal: off".to_string(),
            _ => String::new(),
        };
        let gain_line = match input.config.view_mode {
            ViewMode::Spectrum => format!(
                "Y {:.0}..{:.0} dB  │  {}..{}{}{}{}{}{}",
                view.db_min,
                view.db_max,
                format_hz(view.freq_min).trim(),
                format_hz(view.freq_max).trim(),
                smooth_tag,
                ioct_tag,
                wt_tag,
                time_tag,
                mic_tag,
            ),
            ViewMode::Waterfall => format!(
                "color {:.0}..{:.0} dB  │  {}..{}{}{}{}{}{}",
                view.db_min,
                view.db_max,
                format_hz(view.freq_min).trim(),
                format_hz(view.freq_max).trim(),
                smooth_tag,
                ioct_tag,
                wt_tag,
                time_tag,
                mic_tag,
            ),
            ViewMode::Scope => "scope (ember) │ synthetic 1 kHz".to_string(),
            ViewMode::SpectrumEmber => format!(
                "spectrum (ember) │ {}..{}{}{}",
                format_hz(view.freq_min).trim(),
                format_hz(view.freq_max).trim(),
                smooth_tag,
                mic_tag,
            ),
            ViewMode::Goniometer => format_stereo_status_line("goniometer", input.gonio_state),
            ViewMode::IoTransfer => format_iotransfer_status_line(input.gonio_state),
            ViewMode::BodeMag => format_transfer_status_line(
                "bode mag", input.bode_pair, view.db_min, view.db_max,
            ),
            ViewMode::Coherence => format_transfer_status_line(
                "coherence", input.bode_pair, 0.0, 1.0,
            ),
            ViewMode::BodePhase => format_transfer_status_line(
                "bode phase", input.bode_pair, view.db_min, view.db_max,
            ),
            ViewMode::GroupDelay => format_transfer_status_line(
                "group delay", input.bode_pair, view.db_min, view.db_max,
            ),
            ViewMode::Nyquist => match input.bode_pair {
                Some(p) => format!(
                    "nyquist (ember) │ meas ch {} → ref ch {}  │  unit circle = |H|=1",
                    p.meas, p.ref_ch,
                ),
                None => "nyquist (ember) │ no transfer pair — Space-select MEAS + REF, then T"
                    .to_string(),
            },
            ViewMode::Ir => match input.bode_pair {
                Some(p) => format!(
                    "ir (ember) │ meas ch {} → ref ch {}  │  t=0 mid-cell",
                    p.meas, p.ref_ch,
                ),
                None => "ir (ember) │ no transfer pair — Space-select MEAS + REF, then T"
                    .to_string(),
            },
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
            if !matches!(input.config.view_mode, ViewMode::Scope | ViewMode::SpectrumEmber) {
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
                stack_row += 1.0;
            }
        }

        // Loudness strip — BS.1770-5 / R128 meter for the active channel.
        // Colour-codes the integrated value per the R128 delivery target
        // (-23 LUFS ±0.5 LU green, ±2 LU yellow, outside red, pre-gate
        // neutral).
        if let Some(l) = input.loudness {
            let lines = super::fmt::loudness_readout_lines(&l);
            for line in lines {
                let color = match line.tint {
                    LoudnessTint::Default => text_color,
                    LoudnessTint::Good => Color32::from_rgb(120, 220, 120),
                    LoudnessTint::Warn => Color32::from_rgb(230, 200, 90),
                    LoudnessTint::Bad => Color32::from_rgb(240, 110, 100),
                };
                painter.text(
                    Pos2::new(
                        screen.right() - 8.0,
                        screen.top() + 6.0 + stack_row * (theme::STATUS_PX + 2.0),
                    ),
                    Align2::RIGHT_TOP,
                    line.text,
                    FontId::monospace(theme::STATUS_PX),
                    color,
                );
                stack_row += 1.0;
            }
        }

        // Waterfall colorbar removed: the gradient + dB tick labels
        // pinned to the right edge took up real estate without earning
        // it — the colormap range is already stated compactly in the
        // top-right status line ("color X..Y dB"), and per-view dB-window
        // defaults (theme::default_db_window_for_view) plus +/- and
        // [/] keys cover any retuning needed. Removing it gives the
        // waterfall trace the full pane width.

        // Footer readout: ONLY the cursor's value when hovering this
        // channel. No broadband fallback — toggling between cursor and
        // broadband stats every time the mouse crosses the cell edge
        // was confusing, and the broadband peak/floor/span info doesn't
        // describe the cursor's position anyway. Suppressed in Scope /
        // SpectrumEmber where the substrate owns the cell.
        if !matches!(input.config.view_mode, ViewMode::Scope | ViewMode::SpectrumEmber) {
            if let Some(hover) = input.hover.as_ref().filter(|h| h.channel == display_ch) {
                // For colormap views (Waterfall / CWT / CQT / reassigned)
                // the cursor's Y is time and the magnitude isn't on any
                // axis — sample the latest frame's spectrum at the cursor
                // freq so the footer surfaces the dB the colorbar legend
                // used to show. For Spectrum view this falls through
                // unused; the Db variant carries the value directly.
                let sampled_db = super::fmt::sample_spectrum_db_at_freq(
                    &frame.spectrum, &frame.freqs, hover.freq_hz,
                );
                let text = super::fmt::cursor_readout(
                    hover.freq_hz,
                    &hover.readout,
                    sampled_db,
                    &frame.meta.peaks,
                    frame.meta.dbu_offset_db,
                    frame.meta.spl_offset_db,
                );
                // Hover sits one line above the keytip strip (RC-8).
                // The strip owns `bottom - 6`, so hover lifts up by
                // STATUS_PX + 4 px breathing room.
                painter.text(
                    Pos2::new(
                        screen.left() + 8.0,
                        screen.bottom() - 6.0 - theme::STATUS_PX - 4.0,
                    ),
                    Align2::LEFT_BOTTOM,
                    text,
                    FontId::monospace(theme::READOUT_PX),
                    text_color,
                );
            }
        }
    }

    // Bottom keytip strip — RC-8, plan §4. Painted before the connected
    // indicator so the right-aligned indicator overlays the strip's tail
    // (the strip is left-aligned, indicator is right-aligned, no overlap
    // in practice unless the strip is unusually long).
    if !input.keytips.is_empty() {
        let line = crate::ui::keytips::format_strip(input.keytips);
        painter.text(
            Pos2::new(screen.left() + 8.0, screen.bottom() - 6.0),
            Align2::LEFT_BOTTOM,
            line,
            FontId::monospace(theme::STATUS_PX),
            text_color,
        );
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
        // Crosshair only — every cursor value (dBFS / dBu / time-ago /
        // THD / gain) lands in the bottom-left footer via cursor_readout
        // so labels never obstruct the trace or compete with peak-hold /
        // fundamental annotations.
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
        let panel_w = 520.0;
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

