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
/// One live pair's trace (#685), in launch order. `scene` for the selected
/// pair is the same scene `draw_transfer` gets as its `scene` argument —
/// the one the readouts, meters and fault indicator follow.
pub struct LiveTrace<'a> {
    /// `live`, or `live <meas channel>` when the session measures several.
    pub label: String,
    /// Launch order: picks the colour ([`super::palette::live_color`]).
    pub pair: usize,
    pub scene: &'a ac_scene::TransferScene,
    /// The selected live pair while the selection is on live.
    pub selected: bool,
}

/// The loaded target curve, as `ac-scene` built it: its trace on the
/// magnitude pane and its caption.
pub struct TargetTrace {
    pub trace: ac_scene::Trace,
    pub caption: String,
}

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
    fn new(content: Rect, file_runs: usize, strip: bool) -> Self {
        // The legend reserves one strip row (the live boxes + the nine slot
        // boxes, #256) plus one row per run opened from a file, and is
        // carved out before the panes are sized. Nothing stored and one
        // live pair, no legend.
        let legend_band = if strip {
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
    live: &[LiveTrace<'_>],
    stored: &[StoredTrace<'_>],
    ir: Option<&ac_scene::IrScene>,
    target: Option<&TargetTrace>,
) -> TransferPointer {
    let rect = ui.available_rect_before_wrap();
    let painter = ui.painter();

    // "No session" only when there is truly nothing to show — a viewer
    // that has loaded stored runs for comparison (#321, AC1) must still
    // see them with no live frame yet (fresh session, idle daemon, or a
    // purely offline before/after review). Gating the whole comparison
    // feature on an unrelated live-frame precondition made it unreachable
    // in exactly that state (QA #336 correctness issue 1).
    // A caller with one scene and no live list (a single-pair session, or
    // a test drawing one scene) gets that scene as the one live trace.
    let single;
    let live = if live.is_empty() {
        single = scene
            .map(|scene| LiveTrace {
                label: "live".to_string(),
                pair: 0,
                scene,
                selected: matches!(state.focus, Focus::Live),
            })
            .into_iter()
            .collect::<Vec<_>>();
        &single[..]
    } else {
        live
    };
    if scene.is_none() && live.is_empty() && stored.is_empty() {
        text(
            painter,
            rect.center(),
            Align2::CENTER_CENTER,
            "no session — transfer view",
            COLOR_STRUCTURAL,
        );
        return TransferPointer::default();
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
                // The panel follows focus (#702): say which trace has none.
                match state.focus {
                    super::Focus::Stored(_) => "this run has no IR",
                    super::Focus::Live if state.paused => {
                        "live is paused and no IR was held \u{2014} Enter resumes"
                    }
                    super::Focus::Live => "no IR frame yet",
                },
                COLOR_STRUCTURAL,
            ),
        }
        if let Some(scene) = scene {
            draw_input_meters(painter, content, scene);
        }
        // The time cursor (#720): hover and a pinned one, on the IR as
        // drawn; ac-scene snaps to the sample and formats.
        let vp = Viewport::from(content);
        // A view with nothing drawn (log/ETC from a daemon that sends none)
        // has no time to read or pin (Codex review).
        let drawn = ir.filter(|s| !s.trace.segments.is_empty());
        let pointer = if drawn.is_some() {
            pane_pointer(ui, &[vp], vp, true)
        } else {
            TransferPointer::default()
        };
        // Labels below the panel's header and view rows (Codex review).
        let label_vp = Viewport {
            y: vp.y + 2.0 * ROW_H,
            height: (vp.height - 2.0 * ROW_H).max(0.0),
            ..vp
        };
        if let Some(s) = drawn {
            let (t_lo, t_hi) = s.t_range;
            if let Some(r) = state.ir_cursor_pin.and_then(|t| s.cursor_readout(t)) {
                draw_cursor(painter, &[vp], label_vp, r.x, &r.text, 1, COLOR_SIGNAL);
            }
            if let Some(x) = pointer.hover_x {
                let t = ac_scene::ticks::x_to_time(x, t_lo, t_hi);
                if let Some(r) = s.cursor_readout(t) {
                    draw_cursor(painter, &[vp], label_vp, r.x, &r.text, 0, COLOR_VALUE);
                }
            }
        }
        return pointer;
    }

    let layout = TransferLayout::new(
        content,
        stored.iter().filter(|r| r.slot.is_none()).count(),
        !stored.is_empty() || state.live_count() > 1,
    );

    draw_axes(painter, &layout, scene, stored);
    let shown: &[LiveTrace<'_>] = if state.live_trace_shown() { live } else { &[] };
    // The target first, under the traces it is compared with: dashed, in
    // the value colour, magnitude pane only — it has no phase.
    if let Some(target) = target {
        draw_trace(
            painter,
            &target.trace,
            layout.mag,
            Stroke::new(TRACE_WIDTH, COLOR_VALUE),
            true,
        );
        text(
            painter,
            egui::pos2(layout.mag.x + 4.0, layout.mag.y + layout.mag.height - 4.0),
            Align2::LEFT_BOTTOM,
            &target.caption,
            COLOR_VALUE,
        );
    }
    draw_traces(painter, &layout, shown, stored);
    // The caption row follows focus (#707), as the delay readout does.
    let focused_run = match state.focus {
        Focus::Stored(idx) => stored.get(idx),
        Focus::Live => None,
    };
    draw_mag_annotations(painter, &layout, scene, focused_run);
    draw_delay_readout(painter, &layout, state, scene, stored);
    draw_legend(painter, &layout, state, live, stored);
    // Row 4 (#670): the coherence mask when not the default, and what data
    // protection is holding back. Verbatim ac-scene strings.
    if let Some(scene) = scene {
        let row = [
            scene.coherence_mask_readout.as_deref(),
            scene.protection_readout.as_deref(),
        ];
        let mut at = layout.content.left_top() + egui::vec2(0.0, 4.0 * ROW_H);
        for t in row.into_iter().flatten() {
            at = painter
                .text(at, Align2::LEFT_TOP, t, FontId::default(), COLOR_SIGNAL)
                .right_top()
                + egui::vec2(ROW_H, 0.0);
        }
    }
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

    // The phase pane's caption, for the focused trace: what the phase is
    // referenced to (`R`, `P` — nothing said it before), then the view when
    // it is not the wrapped one (#695). ac-scene's strings.
    let focused_scene = match state.focus {
        Focus::Stored(idx) => stored.get(idx).map(|r| r.scene),
        Focus::Live => scene,
    }
    .or(scene)
    .or_else(|| stored.first().map(|r| r.scene));
    if let Some(s) = focused_scene {
        let mut next = egui::pos2(layout.phase.x + 4.0, layout.phase.y + 2.0);
        next = painter
            .text(
                next,
                Align2::LEFT_TOP,
                &s.phase_ref_readout,
                FontId::default(),
                COLOR_LABEL,
            )
            .right_top()
            + egui::vec2(ROW_H, 0.0);
        if let Some(label) = s.phase_view_readout {
            text(painter, next, Align2::LEFT_TOP, label, COLOR_VALUE);
        }
    }

    // Last, so the fault indicator is over the traces rather than under
    // them.
    draw_fault(painter, &layout, scene, live);

    // The pointer cursor (#718): a line and the focused trace's values at
    // the column nearest the pointer, and a pinned one where the operator
    // clicked. ac-scene snaps and formats; this maps x and draws.
    // Only a trace that is drawn is read: live hidden (`V`) or paused
    // (Enter), or a hidden run, has nothing on screen to read (Codex review).
    // Then the cursor reads the first visible stored trace instead, and
    // names it (operator: the cursor "works, except when live is paused").
    let focused = match state.focus {
        Focus::Stored(idx) => stored
            .get(idx)
            .filter(|r| r.visible)
            .map(|r| (r.scene, None)),
        Focus::Live => scene
            .filter(|_| state.live_trace_shown())
            .map(|s| (s, None)),
    };
    let focused = focused.or_else(|| {
        // The first visible one with columns in the zoomed range: one that
        // ends below it has nothing on screen to read (Codex review).
        stored
            .iter()
            .find(|r| {
                r.visible
                    && r.scene
                        .cursor_columns
                        .iter()
                        .any(|c| (0.0..=1.0).contains(&c.x))
            })
            .map(|r| (r.scene, Some(short_owner(r.label))))
    });
    let (f_lo, f_hi) = (state.freq_range.min(), state.freq_range.max());
    let pointer = pane_pointer(ui, &[layout.mag, layout.phase], layout.mag, false);
    if let Some((s, owner)) = focused {
        let named = |t: &str| match &owner {
            Some(o) => format!("{o} \u{b7} {t}"),
            None => t.to_string(),
        };
        let panes = [layout.mag, layout.phase];
        // A pin outside the zoomed range is off screen, not moved to the
        // nearest column that is (Codex review).
        if let Some(pin) = state.cursor_pin.filter(|p| (f_lo..=f_hi).contains(p)) {
            if let Some(r) = s.cursor_readout(pin) {
                draw_cursor(
                    painter,
                    &panes,
                    layout.mag,
                    r.x,
                    &named(&r.text),
                    1,
                    COLOR_SIGNAL,
                );
            }
        }
        if let Some(x) = pointer.hover_x {
            let f = ac_scene::ticks::x_to_freq(x, f_lo, f_hi);
            if let Some(r) = s.cursor_readout(f) {
                draw_cursor(
                    painter,
                    &panes,
                    layout.mag,
                    r.x,
                    &named(&r.text),
                    0,
                    COLOR_VALUE,
                );
            }
        }
    }
    pointer
}

/// Where the pointer is over the panes this frame, as normalized x on the
/// shared axis, and whether it clicked there (#718, #720).
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct TransferPointer {
    pub hover_x: Option<f64>,
    /// Primary click: pin the cursor here.
    pub click_x: Option<f64>,
    /// Secondary click: clear the pin.
    pub clear: bool,
    /// Over the IR panel (a time axis) rather than the frequency panes.
    pub ir: bool,
}

/// The pointer over any of `panes`, as normalized x in `axis` (the pane
/// whose x the others share).
fn pane_pointer(ui: &Ui, panes: &[Viewport], axis: Viewport, ir: bool) -> TransferPointer {
    let (pos, primary, secondary) = ui.input(|i| {
        (
            i.pointer.hover_pos(),
            i.pointer.primary_clicked(),
            i.pointer.secondary_clicked(),
        )
    });
    let Some(p) = pos else {
        return TransferPointer::default();
    };
    // Only when nothing is over the panes: a window (target list,
    // settings, help) owns the pointer where it lies (Codex review).
    if ui.ctx().layer_id_at(p) != Some(ui.layer_id()) {
        return TransferPointer::default();
    }
    let inside = |vp: &Viewport| {
        p.x >= vp.x && p.x <= vp.x + vp.width && p.y >= vp.y && p.y <= vp.y + vp.height
    };
    if !panes.iter().any(inside) || axis.width <= 0.0 {
        return TransferPointer::default();
    }
    let x = f64::from((p.x - axis.x) / axis.width);
    TransferPointer {
        hover_x: Some(x),
        click_x: primary.then_some(x),
        clear: secondary,
        ir,
    }
}

/// One cursor: a vertical line through `panes` at normalized `x` (of
/// `label_pane`'s axis) and its text at the top of `label_pane`, on row
/// `row` (hover 0, pinned 1). The label is measured: right of the line if
/// it fits, else left, kept inside the pane, wrapped when wider than it.
fn draw_cursor(
    painter: &Painter,
    panes: &[Viewport],
    label_pane: Viewport,
    x: f64,
    label: &str,
    row: usize,
    color: egui::Color32,
) {
    let (x, _) = scene_to_screen((x, 0.0), label_pane);
    for vp in panes {
        painter.line_segment(
            [egui::pos2(x, vp.y), egui::pos2(x, vp.y + vp.height)],
            Stroke::new(1.0, color.gamma_multiply(0.6)),
        );
    }
    let (lo, hi) = (label_pane.x + 2.0, label_pane.x + label_pane.width - 2.0);
    let galley = painter.layout(
        label.to_string(),
        FontId::default(),
        color,
        (hi - lo).max(1.0),
    );
    let w = galley.size().x;
    let left = if x + 6.0 + w <= hi {
        x + 6.0
    } else {
        x - 6.0 - w
    };
    let left = left.clamp(lo, (hi - w).max(lo));
    painter.galley(
        egui::pos2(left, label_pane.y + 4.0 + row as f32 * ROW_H),
        galley,
        color,
    );
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
    live: &[LiveTrace<'_>],
    stored: &[StoredTrace<'_>],
) {
    // One width for every trace, one colour per trace, whatever is
    // selected (#256): changing a curve's look with the selection only
    // confused which curve was which. The selection is the strip box's
    // border.
    for trace in live {
        let stroke = live_stroke(trace.pair);
        draw_trace(painter, &trace.scene.magnitude, layout.mag, stroke, false);
        draw_trace(painter, &trace.scene.phase, layout.phase, stroke, false);
    }
    for run in stored.iter().filter(|r| r.visible) {
        let stroke = Stroke::new(TRACE_WIDTH, super::palette::compare_color(run.color_slot));
        draw_trace(painter, &run.scene.magnitude, layout.mag, stroke, true);
        draw_trace(painter, &run.scene.phase, layout.phase, stroke, true);
    }
}

/// Every trace's line width (#256).
const TRACE_WIDTH: f32 = 1.5;

/// A live pair's trace: the signal colour for the first pair, its own
/// colour for each further one (#685).
fn live_stroke(pair: usize) -> Stroke {
    Stroke::new(TRACE_WIDTH, super::palette::live_color(pair))
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
///
/// The caption row (smoothing, invert/offset) describes the **focused**
/// trace, as the delay readout on the row below does (#707): with a stored
/// run focused it is that run's, after its name, and the live trace's are
/// not drawn — `N` edits the focused trace, so the caption must say what
/// `N` just changed. The band labels stay live's; the calibration verdict,
/// a statement about the live session, is not drawn over a stored run's
/// captions.
fn draw_mag_annotations(
    painter: &Painter,
    layout: &TransferLayout,
    scene: Option<&ac_scene::TransferScene>,
    focused_run: Option<&StoredTrace<'_>>,
) {
    let row1 = layout.content.left_top() + egui::vec2(0.0, ROW_H);
    if let Some(run) = focused_run {
        let captions = [
            run.scene.speed_readout.as_deref(),
            run.scene.estimator_readout.as_deref(),
            run.scene.smoothing_readout,
            run.scene.invert_offset_readout.as_deref(),
        ];
        let mut next = painter
            .text(
                row1,
                Align2::LEFT_TOP,
                format!("{} \u{b7}", short_owner(run.label)),
                FontId::default(),
                super::palette::compare_color(run.color_slot),
            )
            .right_top()
            + egui::vec2(ROW_H / 2.0, 0.0);
        for caption in captions.into_iter().flatten() {
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

    if focused_run.is_some() {
        return;
    }
    // The speed preset and what it gives (#714), first: it is what the
    // band labels above it follow. ac-scene's string.
    let mut calibration_pos = row1;
    if let Some(label) = &scene.speed_readout {
        let drawn = painter.text(
            calibration_pos,
            Align2::LEFT_TOP,
            label,
            FontId::default(),
            COLOR_LABEL,
        );
        calibration_pos = drawn.right_top() + egui::vec2(ROW_H, 0.0);
    }
    // Absent when smoothing is off — an unaltered trace is the resting
    // state and needs no caption. The string is ac-scene's.
    if let Some(label) = scene.smoothing_readout {
        let drawn = painter.text(
            calibration_pos,
            Align2::LEFT_TOP,
            label,
            FontId::default(),
            COLOR_LABEL,
        );
        calibration_pos = drawn.right_top() + egui::vec2(ROW_H, 0.0);
    }
    // An inverted or offset trace is not what was measured: said in the
    // value colour, beside the smoothing caption. ac-scene's string.
    if let Some(label) = &scene.invert_offset_readout {
        let drawn = painter.text(
            calibration_pos,
            Align2::LEFT_TOP,
            label,
            FontId::default(),
            COLOR_VALUE,
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

/// A trace's name as the caption row's owner tag: at most 24 characters,
/// so a long file name cannot push the captions it owns off the row
/// (Codex review of #707). The full name is on the run's legend row.
fn short_owner(label: &str) -> String {
    const MAX: usize = 24;
    if label.chars().count() <= MAX {
        label.to_string()
    } else {
        let head: String = label.chars().take(MAX - 1).collect();
        format!("{head}\u{2026}")
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
    live: &[LiveTrace<'_>],
    stored: &[StoredTrace<'_>],
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

    // One live box per pair (#685), in its curve's colour, filled only
    // when that curve is actually on screen. Before the first frame the
    // lone box still stands, outlined.
    if live.is_empty() {
        draw_box(
            slot_box(3.0 * h),
            "live",
            live_stroke(0).color,
            false,
            matches!(state.focus, Focus::Live),
        );
    }
    for trace in live {
        let width = if trace.label.len() > 4 { 4.5 } else { 3.0 };
        draw_box(
            slot_box(width * h),
            &trace.label,
            live_stroke(trace.pair).color,
            state.live_trace_shown(),
            trace.selected,
        );
    }
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

    // A selected slot's captions are on the caption row (#707); a
    // file-opened run's stay on its own row below.

    // Runs opened from a file: a row each, in their colour — identity,
    // then how the trace was derived, then what was done to it (#221 UX),
    // each its own span so a narrow window clips the captions first.
    for (i, run) in stored.iter().filter(|r| r.slot.is_none()).enumerate() {
        let marker = if run.focused { "▸ " } else { "  " };
        let color = if run.visible {
            super::palette::compare_color(run.color_slot)
        } else {
            COLOR_STRUCTURAL
        };
        let mut next = painter
            .text(
                egui::pos2(
                    layout.content.min.x,
                    legend_top + 6.0 + h * (i as f32 + 1.0),
                ),
                Align2::LEFT_TOP,
                format!("{marker}{}  {}", run.label, run.captured_at_utc),
                FontId::default(),
                color,
            )
            .right_top()
            + egui::vec2(ROW_H, 0.0);
        for caption in [
            run.scene.estimator_readout.as_deref(),
            run.scene.smoothing_readout,
            run.scene.invert_offset_readout.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            next = painter
                .text(next, Align2::LEFT_TOP, caption, FontId::default(), color)
                .right_top()
                + egui::vec2(ROW_H, 0.0);
        }
    }
}

/// Fault indicator (#228), centred on the magnitude pane: the delay
/// readout owns the content band's top-left corner, the meters own the
/// right edge, and the stimulus banner owns its own reserved band above
/// all of this, so nothing collides. Both strings are verbatim ac-scene.
/// Live-only: a fault is a live-session condition.
///
/// With several pairs (#685) the indicator is the selected pair's, and a
/// fault on any other pair is listed under it by name, so a silent second
/// leg cannot hide behind the selected one.
fn draw_fault(
    painter: &Painter,
    layout: &TransferLayout,
    scene: Option<&ac_scene::TransferScene>,
    live: &[LiveTrace<'_>],
) {
    let centre = egui::pos2(
        layout.mag.x + layout.mag.width / 2.0,
        layout.mag.y + layout.mag.height / 2.0,
    );
    let mut below = centre.y + 44.0;
    // The selected pair's own fault is the big one below; skip its scene.
    let others = live
        .iter()
        .filter(|t| !scene.is_some_and(|s| std::ptr::eq(s, t.scene)));
    for trace in others {
        let Some(fault) = trace.scene.fault else {
            continue;
        };
        painter.text(
            egui::pos2(centre.x, below),
            Align2::CENTER_CENTER,
            format!("{}  {}", trace.label, fault.label()),
            FontId::proportional(16.0),
            COLOR_SIGNAL,
        );
        below += 20.0;
    }
    let Some(fault) = scene.and_then(|s| s.fault) else {
        return;
    };
    let color = match fault.severity() {
        // Never green, and never the trace colour: a fault must not read
        // as measurement.
        ac_scene::Severity::Fault => COLOR_SIGNAL,
        ac_scene::Severity::Confirmation => COLOR_VALUE,
    };
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

#[cfg(test)]
mod tests {
    use super::short_owner;

    #[test]
    fn a_long_owner_is_cut_to_24_characters() {
        assert_eq!(short_owner("slot 2"), "slot 2");
        let cut = short_owner("a-very-long-capture-name-2026-09-28T12-00-00Z.acsnap");
        assert_eq!(cut.chars().count(), 24);
        assert!(cut.ends_with('\u{2026}'));
    }
}
