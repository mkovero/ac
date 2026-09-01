//! View dispatch (architect review, decision 4): a `ViewKind` enum with
//! one variant per view, drawn through one dispatch function, so a
//! future waterfall/H-view (M4+) is a new match arm — not a shell
//! restructure. Session management and keyboard routing stay
//! view-agnostic; they call [`draw_view`], never a view-specific drawing
//! function directly.
//!
//! The module is split by what each file answers:
//!
//! | module | holds |
//! |--------|-------|
//! | [`state`] | what the operator chose — no drawing, no egui |
//! | [`palette`] | the four colours, one hue |
//! | [`paint`] | shared primitives: polylines, grids, axis labels, meters |
//! | [`spectrum`], [`transfer`], [`ir`] | one view/panel each |
//!
//! Every file in it holds the same contract: points arrive normalized
//! from `ac-scene`, strings arrive formatted from `ac-scene`, and this
//! crate maps and paints (`computes_nothing`, AC1 — whose scan walks
//! this directory, see `computes_nothing::tests::source_files`).

mod ir;
mod paint;
mod palette;
mod spectrum;
mod state;
mod transfer;

use ac_scene::Scene;
use egui::Ui;

pub use ir::draw_sweep_ir_panel;
pub use state::{DerotChoice, Focus, LoadedRun, SpectrumViewState, StimState, TransferViewState};
pub use transfer::StoredTrace;

use palette::COLOR_LABEL;

pub enum ViewKind {
    Spectrum(SpectrumViewState),
    Transfer(TransferViewState),
}

impl ViewKind {
    pub fn id(&self) -> crate::keys::ViewId {
        match self {
            ViewKind::Spectrum(_) => crate::keys::ViewId::Spectrum,
            ViewKind::Transfer(_) => crate::keys::ViewId::Transfer,
        }
    }
}

/// One dispatch function every future view (M4+) extends by adding a
/// match arm — never by the shell inlining a new drawing call. The two
/// scene options are mutually exclusive in practice: the app builds only
/// the one matching the active view (the other stays `None`).
pub fn draw_view(
    kind: &ViewKind,
    ui: &mut Ui,
    scene: Option<&Scene>,
    transfer_scene: Option<&ac_scene::TransferScene>,
    stored: &[StoredTrace<'_>],
    ir_scene: Option<&ac_scene::IrScene>,
) {
    // Reserve the half line the top y-axis tick label hangs into (#245).
    // Every pane's tick labels are drawn vertically centred on their
    // gridline, so the topmost one — the tick sitting exactly on the pane's
    // top edge — puts half its glyph height above the rect the view was
    // given. egui's item spacing is narrower than that, so the shell's
    // connection banner on the row above ended up struck through by the
    // `20` of the +20 dB tick. Taking the space here, before either view
    // reads `available_rect_before_wrap`, keeps the top edge clear without
    // the panes needing to know why.
    //
    // It reserves the top only. The frequency labels are drawn at
    // `rect.max.y` with `Align2::CENTER_TOP`, so a full line still hangs
    // below the rect; nothing is drawn under a view today, so it overlaps
    // nothing. Anything stacked below one needs the same reserve at the
    // bottom.
    let tick_line_h = ui
        .painter()
        .layout_no_wrap("0".to_string(), egui::FontId::default(), COLOR_LABEL)
        .size()
        .y;
    ui.add_space(tick_line_h / 2.0);
    match kind {
        ViewKind::Spectrum(state) => spectrum::draw_spectrum(state, ui, scene),
        ViewKind::Transfer(state) => {
            transfer::draw_transfer(state, ui, transfer_scene, stored, ir_scene)
        }
    }
}
