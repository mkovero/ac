//! Transfer view (M4b): magnitude pane stacked over phase pane, shared
//! log-f axis, gap rendering for masked columns, delay readout, input
//! meters, the stimulus banner, and the stored-run comparison overlay
//! (#321) — every string and coordinate from `ac-scene`, drawn verbatim.
//!
//! The draw is split into a layout pass and one function per band of the
//! screen. [`TransferLayout`] is the single place the vertical budget is
//! divided; every function below takes it rather than re-deriving a
//! pane's geometry, which is what kept the banner band, the legend band
//! and the panes agreeing on where each other ended.

use egui::{Align2, FontId, Painter, Rect, Stroke, Ui};

use crate::geometry::{scene_to_screen, Viewport};

use super::ir::draw_ir_panel;
use super::paint::{draw_freq_labels, draw_input_meters, draw_pane_grid, draw_trace, text};
use super::palette::{COLOR_LABEL, COLOR_SIGNAL, COLOR_STRUCTURAL, COLOR_VALUE};
use super::state::{Focus, StimState, TransferViewState};

/// Height of one text row in the magnitude pane's annotation stack and in
/// the comparison legend. Both bands are laid out in multiples of it, and
/// the legend band's height has to be known before the panes are sized,
/// so it is a module constant rather than a local in any one of them.
const ROW_H: f32 = 16.0;

/// One stored run as the drawing layer sees it (#321): its identity, its
/// built scene, and whether it currently holds focus.
///
/// A named struct rather than the positional 4-tuple this used to be —
/// `(&str, &str, &TransferScene, bool)` gave no clue which `&str` was the
/// filename and which the timestamp, and every read site destructured it
/// with `_` placeholders.
pub struct StoredTrace<'a> {
    /// Attribution (acceptance criterion 1) — the file's own name.
    pub label: &'a str,
    /// RFC3339 UTC capture instant; disambiguates two runs sharing a
    /// basename (QA #336 correctness issue 2).
    pub captured_at_utc: &'a str,
    pub scene: &'a ac_scene::TransferScene,
    /// Whether this run is what `N` edits and what the delay readout
    /// names right now.
    pub focused: bool,
    /// Drawn or hidden (`V`, #256).
    pub visible: bool,
    /// Its comparison colour (#256).
    pub color_slot: usize,
    /// Its slot (`Ctrl`+1…9), or `None` for a run opened from a file.
    pub slot: Option<u8>,
}

/// How the content band is divided this frame.
struct TransferLayout {
    /// Everything below the stimulus banner's reserved band.
    content: Rect,
    mag: Viewport,
    phase: Viewport,
    /// Top of the comparison legend band, or `None` when nothing is
    /// loaded — an operator who has opened nothing pays zero height for
    /// the feature and sees the pre-#321 layout exactly.
    legend_top: Option<f32>,
}

impl TransferLayout {
    fn new(content: Rect, file_runs: usize, any_stored: bool) -> Self {
        // The legend reserves one strip row (live + the nine slot boxes,
        // #256) plus one row per run opened from a file, and is carved out
        // before the panes are sized. Nothing stored, no legend.
        let legend_band = if any_stored {
            (file_runs as f32 + 1.0) * ROW_H + 8.0
        } else {
            0.0
        };

        // Two stacked panes: magnitude (top), phase (bottom). Shared x
        // (log-f); each pane maps its own normalized y over its own band.
        let gap = 8.0;
        let pane_h = (content.height() - gap - legend_band) / 2.0;
        TransferLayout {
            content,
            mag: Viewport {
                y: content.min.y,
                height: pane_h,
                ..Viewport::from(content)
            },
            phase: Viewport {
                y: content.min.y + pane_h + gap,
                height: pane_h,
                ..Viewport::from(content)
            },
            legend_top: (legend_band > 0.0).then_some(content.max.y - legend_band),
        }
    }
}

pub(super) fn draw_transfer(
    state: &TransferViewState,
    ui: &mut Ui,
    scene: Option<&ac_scene::TransferScene>,
    stored: &[StoredTrace<'_>],
    ir: Option<&ac_scene::IrScene>,
) {
    let rect = ui.available_rect_before_wrap();
    let painter = ui.painter();

    // "No session" only when there is truly nothing to show — a viewer
    // that has loaded stored runs for comparison (#321, AC1) must still
    // see them with no live frame yet (fresh session, idle daemon, or a
    // purely offline before/after review). Gating the whole comparison
    // feature on an unrelated live-frame precondition made it unreachable
    // in exactly that state (QA #336 correctness issue 1).
    if scene.is_none() && stored.is_empty() {
        text(
            painter,
            rect.center(),
            Align2::CENTER_CENTER,
            "no session — transfer view",
            COLOR_STRUCTURAL,
        );
        return;
    }

    // Everything else lives below the banner's reserved band.
    let banner_band = draw_banner(painter, rect, state);
    let content = Rect::from_min_max(egui::pos2(rect.min.x, rect.min.y + banner_band), rect.max);

    // The IR panel (`H`, #286) replaces the mag/phase panes rather than
    // sharing the content band with them — it is an on-demand accessory
    // view of the same session, not a third pane the other two must make
    // room for. Input meters stay: gain staging is still relevant while
    // looking at h(t). The fault indicator does not — it is drawn
    // relative to the magnitude pane's geometry, which does not exist in
    // this branch.
    if state.ir_panel_open() {
        match ir {
            Some(ir_scene) => draw_ir_panel(painter, content, ir_scene),
            None => text(
                painter,
                content.center(),
                Align2::CENTER_CENTER,
                "no IR frame yet",
                COLOR_STRUCTURAL,
            ),
        }
        if let Some(scene) = scene {
            draw_input_meters(painter, content, scene);
        }
        return;
    }

    let layout = TransferLayout::new(
        content,
        stored.iter().filter(|r| r.slot.is_none()).count(),
        !stored.is_empty(),
    );
    let live_focused = matches!(state.focus, Focus::Live);

    draw_axes(painter, &layout, scene, stored);
    draw_traces(
        painter,
        &layout,
        scene.filter(|_| state.live_trace_shown()),
        stored,
        live_focused,
    );
    draw_mag_annotations(painter, &layout, scene);
    draw_delay_readout(painter, &layout, state, scene, stored);
    draw_legend(painter, &layout, state, stored, live_focused);
    if state.paused {
        // #256: said plainly, on the row under the delay readout (row 2 —
        // the band labels, caption and readout own rows 0–2, the meters the
        // right edge), so a held picture is never mistaken for a live one.
        text(
            painter,
            layout.content.left_top() + egui::vec2(0.0, 3.0 * ROW_H),
            Align2::LEFT_TOP,
            "PAUSED \u{2014} Enter resumes",
            COLOR_SIGNAL,
        );
    }

    // Input-level meters: always on (D6), no toggle. Live-only — there is
    // no input-level reading without a live frame.
    if let Some(scene) = scene {
        draw_input_meters(painter, content, scene);
    }

    // Last, so the fault indicator is over the traces rather than under
    // them.
    draw_fault(painter, &layout, scene);
}

/// The stimulus banner (safety UI) owns a reserved top band that nothing
/// else draws into — UX finding: it must be top-center and must not
/// collide with the delay readout or the meter labels. Drawn first so its
/// height carves the band out before anything else is placed; the
/// returned height is that band. DRIVING is louder than ARMED and uses
/// the signal colour (never green); strings are verbatim ac-scene (F5).
/// Channel/port come from config in M4c (#182) — placeholder channel for
/// now, but the STRING is ac-scene's, which is what F5 checks.
fn draw_banner(painter: &Painter, rect: Rect, state: &TransferViewState) -> f32 {
    let level = state.stimulus.level_dbfs();
    let (label, size, color) = match state.stimulus.state() {
        StimState::Idle => return 0.0,
        StimState::Armed => (
            ac_scene::readout::format_armed_banner(0, None, level),
            18.0,
            COLOR_VALUE,
        ),
        StimState::Driving => (
            ac_scene::readout::format_driving_banner(0, None, level),
            24.0,
            COLOR_SIGNAL,
        ),
    };
    painter.text(
        egui::pos2(rect.center().x, rect.min.y + 4.0),
        Align2::CENTER_TOP,
        label,
        FontId::proportional(size),
        color,
    );
    size + 8.0
}

/// Axis context (#194): gridlines + labels behind the traces, drawn from
/// ac-scene ticks verbatim — positions are ac-scene's normalized
/// coordinates, this crate only maps them (no tick math here). dB grid on
/// the magnitude pane, ±180° grid on the phase pane (with the 0°
/// reference), shared freq labels along the bottom.
///
/// Ticks are derived purely from the caller's freq/db range (identical
/// across every scene built this pass, live or stored — `app.rs` builds
/// them all against the same range), never from which trace supplied
/// them. So with no live frame yet, the first stored run's axis is the
/// same grid the live one would have drawn — this is what lets the
/// comparison render before any live frame arrives (correctness issue 1,
/// QA #336).
fn draw_axes(
    painter: &Painter,
    layout: &TransferLayout,
    scene: Option<&ac_scene::TransferScene>,
    stored: &[StoredTrace<'_>],
) {
    let Some(axis_scene) = scene.or_else(|| stored.first().map(|run| run.scene)) else {
        return;
    };
    draw_pane_grid(painter, &axis_scene.mag_axis, layout.mag);
    draw_pane_grid(painter, &axis_scene.phase_axis, layout.phase);
    draw_freq_labels(painter, &axis_scene.freq_axis, layout.phase);
}

/// Trace weight/colour is focus, not identity (#321 UX ruling: the
/// palette has one signal hue on purpose, so N traces cannot each get
/// their own colour). The focused trace — live or one stored run — draws
/// full weight in the signal colour; every other trace recedes to
/// structural grey, dashed for a stored run's existing live-vs-stored
/// provenance convention.
///
/// Stored runs are drawn after the live trace so a focused stored run's
/// full-weight curve is not occluded by it, and are drawn even when there
/// is no live trace to be occluded by (correctness issue 1) — the loop
/// does not depend on `scene`.
fn draw_traces(
    painter: &Painter,
    layout: &TransferLayout,
    scene: Option<&ac_scene::TransferScene>,
    stored: &[StoredTrace<'_>],
    live_focused: bool,
) {
    if let Some(scene) = scene {
        let stroke = focus_stroke(live_focused);
        draw_trace(painter, &scene.magnitude, layout.mag, stroke, false);
        draw_trace(painter, &scene.phase, layout.phase, stroke, false);
    }
    for run in stored.iter().filter(|r| r.visible) {
        let stroke = Stroke::new(
            if run.focused { 2.0 } else { 1.0 },
            super::palette::compare_color(run.color_slot),
        );
        draw_trace(painter, &run.scene.magnitude, layout.mag, stroke, true);
        draw_trace(painter, &run.scene.phase, layout.phase, stroke, true);
    }
}

fn focus_stroke(focused: bool) -> Stroke {
    if focused {
        Stroke::new(1.5, COLOR_SIGNAL)
    } else {
        Stroke::new(1.0, COLOR_STRUCTURAL)
    }
}

/// The top of the magnitude pane carries three rows, in this order and
/// for this reason (#224 + #229, ruled on when the two were reviewed as a
/// pair):
///
///   row 0   band labels        what the ANALYSER resolved, per rung
///   row 1   smoothing caption  what is actually ON SCREEN
///   row 2   delay readout      a measured value ([`draw_delay_readout`])
///
/// Rows 0 and 1 are both statements about resolution and are adjacent
/// with nothing between them, so the pane reads as one statement rather
/// than two competing ones. The caption is authoritative for the drawn
/// trace: at 1/1 octave the curve is smoothed far wider than any band's
/// Δf, and a screenshot showing "0.98 Hz" over it would overclaim.
///
/// The delay readout is on row 2 rather than sharing row 0, which is not
/// cosmetic: at a 3-digit delay its laid-out width runs past the deepest
/// band label's left edge, and the two overlap. Moving it down removes
/// the collision outright, with no width arbitration to re-verify when
/// the ladder or the font changes. Anchoring the caption *on* row 0 was
/// rejected for the same class of reason — the only gaps wide enough sit
/// between band labels, and those move with sample rate and zoom.
///
/// Positions and strings are verbatim ac-scene; this crate maps the
/// normalized x and draws, as with every other label. Structural grey
/// with no rule or box: findable when sought, invisible when not.
/// Live-only: band labels state what the analyser resolved on *this*
/// (live) frame, so there is nothing to draw without one.
fn draw_mag_annotations(
    painter: &Painter,
    layout: &TransferLayout,
    scene: Option<&ac_scene::TransferScene>,
) {
    let Some(scene) = scene else { return };
    for band in &scene.band_labels {
        let (x, _) = scene_to_screen((band.position, 0.0), layout.mag);
        text(
            painter,
            egui::pos2(x, layout.mag.y),
            Align2::CENTER_TOP,
            &band.text,
            COLOR_STRUCTURAL,
        );
    }

    // Absent when smoothing is off — an unaltered trace is the resting
    // state and needs no caption. The string is ac-scene's.
    let row1 = layout.content.left_top() + egui::vec2(0.0, ROW_H);
    let mut calibration_pos = row1;
    if let Some(label) = scene.smoothing_readout {
        let drawn = painter.text(
            row1,
            Align2::LEFT_TOP,
            label,
            FontId::default(),
            COLOR_LABEL,
        );
        calibration_pos = drawn.right_top() + egui::vec2(ROW_H, 0.0);
    }

    // #466: the session check's verdict on the applied voltage scale, on
    // the same row. The weight comes from `state`, never from the text:
    // verified is context grey like the smoothing caption, unverified is
    // readout weight, refused is the fault colour.
    if let Some(readout) = &scene.calibration_readout {
        let color = match readout.state {
            ac_scene::CalibrationState::Verified => COLOR_LABEL,
            ac_scene::CalibrationState::Unverified => COLOR_VALUE,
            ac_scene::CalibrationState::Refused => COLOR_SIGNAL,
        };
        text(
            painter,
            calibration_pos,
            Align2::LEFT_TOP,
            &readout.text,
            color,
        );
    }
}

/// Delay readout (row 2). With nothing loaded this is
/// `scene.delay_readout` verbatim (ms only — #391 removed the metres
/// conversion this used to also carry, and the calibration/warning rows
/// that came with it). Once a stored run exists, the value switches to
/// whichever trace is focused and gains an owner tag (acceptance
/// criterion 5: no readout naming a single measurement may leave its
/// owner ambiguous) — `delay (<owner>)` is this crate's own chrome text,
/// not a reformatted measurement, so it does not cross the
/// `computes_nothing` boundary; the number after it is still ac-scene's
/// string, untouched.
///
/// `Focus::Live` with no live scene (nothing loaded yet either, or a
/// stored-only session that hasn't cycled focus) draws no readout at
/// all — there is no measurement to attribute.
fn draw_delay_readout(
    painter: &Painter,
    layout: &TransferLayout,
    state: &TransferViewState,
    scene: Option<&ac_scene::TransferScene>,
    stored: &[StoredTrace<'_>],
) {
    // The live trace also carries the operator's delay control (#669):
    // where the delay came from and what Find reads against it. A stored
    // run has no live IR, so nothing to find.
    let focused: Option<(&str, &str, Option<&str>)> = match state.focus {
        Focus::Live => scene.map(|s| {
            (
                "live",
                s.delay_readout.as_str(),
                s.delay_control_readout.as_deref(),
            )
        }),
        Focus::Stored(idx) => stored
            .get(idx)
            .map(|run| (run.label, run.scene.delay_readout.as_str(), None)),
    };
    let Some((owner, delay, control)) = focused else {
        return;
    };
    let delay_text = if stored.is_empty() {
        delay.to_string()
    } else {
        format!("delay ({owner})  {delay}")
    };
    let delay_text = match control {
        Some(control) => format!("{delay_text}   {control}"),
        None => delay_text,
    };
    text(
        painter,
        layout.content.left_top() + egui::vec2(0.0, 2.0 * ROW_H),
        Align2::LEFT_TOP,
        delay_text,
        COLOR_VALUE,
    );
}

/// The comparison legend (#256): one strip — a `live` box, then a box per
/// slot 1…9 — so what is stored and what is drawn reads at a glance.
///
/// - stored and shown: filled with the slot's colour, number in black;
/// - stored and hidden: outlined in the slot's colour;
/// - empty: a dim outline;
/// - selected (`Tab`): a bright border.
///
/// The selected run's captions (estimator, smoothing) follow the strip.
/// Runs opened from a file have no slot and keep a row each below it.
fn draw_legend(
    painter: &Painter,
    layout: &TransferLayout,
    state: &TransferViewState,
    stored: &[StoredTrace<'_>],
    live_focused: bool,
) {
    let Some(legend_top) = layout.legend_top else {
        return;
    };
    let h = ROW_H;
    let gap = 4.0;
    let mut x = layout.content.min.x;
    let mut slot_box = |width: f32| {
        let rect = Rect::from_min_size(egui::pos2(x, legend_top + 2.0), egui::vec2(width, h));
        x += width + gap;
        rect
    };
    let draw_box = |rect: Rect, label: &str, color: egui::Color32, filled: bool, focused: bool| {
        if filled {
            painter.rect_filled(rect, 2.0, color);
        }
        let border = if focused {
            Stroke::new(2.0, COLOR_VALUE)
        } else {
            Stroke::new(1.0, color)
        };
        painter.rect_stroke(rect, 2.0, border, egui::StrokeKind::Inside);
        let ink = if filled { egui::Color32::BLACK } else { color };
        text(painter, rect.center(), Align2::CENTER_CENTER, label, ink);
    };

    draw_box(
        slot_box(3.0 * h),
        "live",
        COLOR_LABEL,
        state.live_trace_shown(),
        live_focused,
    );
    for n in 1..=crate::keys::SLOT_KEYS.len() as u8 {
        let rect = slot_box(1.4 * h);
        let label = n.to_string();
        match stored.iter().find(|r| r.slot == Some(n)) {
            Some(run) => draw_box(
                rect,
                &label,
                super::palette::compare_color(run.color_slot),
                run.visible,
                run.focused,
            ),
            None => draw_box(rect, &label, COLOR_STRUCTURAL, false, false),
        }
    }

    // The selected run's captions after the strip.
    if let Some(run) = stored.iter().find(|r| r.focused) {
        let mut next = egui::pos2(x + gap, legend_top + 2.0);
        for caption in [
            run.scene.estimator_readout.as_deref(),
            run.scene.smoothing_readout,
        ]
        .into_iter()
        .flatten()
        {
            next = painter
                .text(
                    next,
                    Align2::LEFT_TOP,
                    caption,
                    FontId::default(),
                    COLOR_LABEL,
                )
                .right_top()
                + egui::vec2(ROW_H, 0.0);
        }
    }

    // Runs opened from a file: a row each, in their colour.
    for (i, run) in stored.iter().filter(|r| r.slot.is_none()).enumerate() {
        let marker = if run.focused { "▸ " } else { "  " };
        let color = if run.visible {
            super::palette::compare_color(run.color_slot)
        } else {
            COLOR_STRUCTURAL
        };
        text(
            painter,
            egui::pos2(
                layout.content.min.x,
                legend_top + 6.0 + h * (i as f32 + 1.0),
            ),
            Align2::LEFT_TOP,
            format!("{marker}{}  {}", run.label, run.captured_at_utc),
            color,
        );
    }
}

/// Fault indicator (#228), centred on the magnitude pane: the delay
/// readout owns the content band's top-left corner, the meters own the
/// right edge, and the stimulus banner owns its own reserved band above
/// all of this, so nothing collides. Both strings are verbatim ac-scene.
/// Live-only: a fault is a live-session condition.
fn draw_fault(painter: &Painter, layout: &TransferLayout, scene: Option<&ac_scene::TransferScene>) {
    let Some(fault) = scene.and_then(|s| s.fault) else {
        return;
    };
    let color = match fault.severity() {
        // Never green, and never the trace colour: a fault must not read
        // as measurement.
        ac_scene::Severity::Fault => COLOR_SIGNAL,
        ac_scene::Severity::Confirmation => COLOR_VALUE,
    };
    let centre = egui::pos2(
        layout.mag.x + layout.mag.width / 2.0,
        layout.mag.y + layout.mag.height / 2.0,
    );
    painter.text(
        centre,
        Align2::CENTER_CENTER,
        fault.label(),
        FontId::proportional(28.0),
        color,
    );
    if let Some(detail) = fault.detail() {
        painter.text(
            egui::pos2(centre.x, centre.y + 22.0),
            Align2::CENTER_CENTER,
            detail,
            FontId::proportional(14.0),
            COLOR_LABEL,
        );
    }
}
