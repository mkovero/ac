use egui::{epaint, Pos2, Rect, Vec2};

use crate::data::types::*;
use crate::ui::overlay::{draw, HoverInfo, HoverReadout, OverlayInput};

fn default_raw_input() -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 720.0))),
        ..Default::default()
    }
}

fn test_frame(freq: f32, dbfs: f32, thd: f32, thdn: f32) -> DisplayFrame {
    DisplayFrame {
        spectrum: std::sync::Arc::new(vec![-100.0; 100]),
        freqs: std::sync::Arc::new((0..100).map(|i| 20.0 + i as f32 * 200.0).collect()),
        meta: FrameMeta {
            freq_hz: freq,
            fundamental_dbfs: dbfs,
            thd_pct: thd,
            thdn_pct: thdn,
            in_dbu: None,
            sr: 48000,
            clipping: false,
            xruns: 0,
        },
        new_row: None,
    }
}

fn extract_texts(shapes: &[epaint::ClippedShape]) -> Vec<String> {
    let mut texts = Vec::new();
    for cs in shapes {
        collect_texts(&cs.shape, &mut texts);
    }
    texts
}

fn collect_texts(shape: &epaint::Shape, out: &mut Vec<String>) {
    match shape {
        epaint::Shape::Text(ts) => {
            out.push(ts.galley.job.text.clone());
        }
        epaint::Shape::Vec(shapes) => {
            for s in shapes {
                collect_texts(s, out);
            }
        }
        _ => {}
    }
}

fn run_overlay(input: OverlayInput<'_>) -> Vec<String> {
    let ctx = egui::Context::default();
    let mut input_opt = Some(input);
    let output = ctx.run(default_raw_input(), |ctx| {
        if let Some(input) = input_opt.take() {
            draw(ctx, input);
        }
    });
    extract_texts(&output.shapes)
}

fn default_config() -> DisplayConfig {
    DisplayConfig::default()
}

// ── Spectrum readout ──────────────────────────────────────────────

#[test]
fn overlay_shows_spectrum_readout() {
    let config = default_config();
    let frame = test_frame(1000.0, -3.0, 0.003, 0.005);
    let frames = [Some(frame)];
    let cell_views = [CellView::default()];

    let input = OverlayInput {
        config: &config,
        frames: &frames,
        cell_views: &cell_views,
        selected: &[false],
        selection_order: &[],
        transfer: None,
        active_meas: None,
        active_meas_idx: 0,
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
    };

    let texts = run_overlay(input);
    let has_readout = texts.iter().any(|t| t.contains("THD 0.003%") && t.contains("THD+N 0.005%"));
    assert!(has_readout, "spectrum readout not found in: {texts:?}");
}

#[test]
fn overlay_shows_dbu_when_calibrated() {
    let config = default_config();
    let mut frame = test_frame(1000.0, -3.0, 0.003, 0.005);
    frame.meta.in_dbu = Some(4.0);
    let frames = [Some(frame)];
    let cell_views = [CellView::default()];

    let input = OverlayInput {
        config: &config,
        frames: &frames,
        cell_views: &cell_views,
        selected: &[false],
        selection_order: &[],
        transfer: None,
        active_meas: None,
        active_meas_idx: 0,
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
    };

    let texts = run_overlay(input);
    let has_dbu = texts.iter().any(|t| t.contains("+4.0 dBu"));
    assert!(has_dbu, "dBu readout not found in: {texts:?}");
}

// ── CLIP indicator ────────────────────────────────────────────────

#[test]
fn overlay_shows_clip_when_clipping() {
    let config = default_config();
    let mut frame = test_frame(1000.0, -0.1, 10.0, 12.0);
    frame.meta.clipping = true;
    let frames = [Some(frame)];
    let cell_views = [CellView::default()];

    let input = OverlayInput {
        config: &config,
        frames: &frames,
        cell_views: &cell_views,
        selected: &[false],
        selection_order: &[],
        transfer: None,
        active_meas: None,
        active_meas_idx: 0,
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
    };

    let texts = run_overlay(input);
    assert!(texts.iter().any(|t| t == "CLIP"), "CLIP not found in: {texts:?}");
}

#[test]
fn overlay_no_clip_when_not_clipping() {
    let config = default_config();
    let frame = test_frame(1000.0, -3.0, 0.003, 0.005);
    let frames = [Some(frame)];
    let cell_views = [CellView::default()];

    let input = OverlayInput {
        config: &config,
        frames: &frames,
        cell_views: &cell_views,
        selected: &[false],
        selection_order: &[],
        transfer: None,
        active_meas: None,
        active_meas_idx: 0,
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
    };

    let texts = run_overlay(input);
    assert!(!texts.iter().any(|t| t == "CLIP"), "unexpected CLIP in: {texts:?}");
}

// ── FROZEN indicator ──────────────────────────────────────────────

#[test]
fn overlay_shows_frozen() {
    let mut config = default_config();
    config.frozen = true;
    let frame = test_frame(1000.0, -3.0, 0.003, 0.005);
    let frames = [Some(frame)];
    let cell_views = [CellView::default()];

    let input = OverlayInput {
        config: &config,
        frames: &frames,
        cell_views: &cell_views,
        selected: &[false],
        selection_order: &[],
        transfer: None,
        active_meas: None,
        active_meas_idx: 0,
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
    };

    let texts = run_overlay(input);
    assert!(texts.iter().any(|t| t == "FROZEN"), "FROZEN not found in: {texts:?}");
}

// ── Connected/disconnected ────────────────────────────────────────

#[test]
fn overlay_shows_connected() {
    let config = default_config();
    let frames: [Option<DisplayFrame>; 0] = [];
    let cell_views: [CellView; 0] = [];

    let input = OverlayInput {
        config: &config,
        frames: &frames,
        cell_views: &cell_views,
        selected: &[],
        selection_order: &[],
        transfer: None,
        active_meas: None,
        active_meas_idx: 0,
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
    };

    let texts = run_overlay(input);
    assert!(texts.iter().any(|t| t.contains("connected")), "connection status not found in: {texts:?}");
}

#[test]
fn overlay_shows_disconnected() {
    let config = default_config();
    let frames: [Option<DisplayFrame>; 0] = [];
    let cell_views: [CellView; 0] = [];

    let input = OverlayInput {
        config: &config,
        frames: &frames,
        cell_views: &cell_views,
        selected: &[],
        selection_order: &[],
        transfer: None,
        active_meas: None,
        active_meas_idx: 0,
        connected: false,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
    };

    let texts = run_overlay(input);
    assert!(texts.iter().any(|t| t.contains("disconnected")), "disconnected status not found in: {texts:?}");
}

// ── Transfer delay ────────────────────────────────────────────────

#[test]
fn overlay_shows_transfer_delay() {
    let mut config = default_config();
    config.layout = LayoutMode::Transfer;
    let frame = test_frame(1000.0, -3.0, 0.003, 0.005);
    let frames = [Some(frame.clone()), Some(frame)];
    let cell_views = [CellView::default(), CellView::default()];
    let tf = TransferFrame {
        freqs: vec![100.0, 1000.0, 10000.0],
        magnitude_db: vec![0.0, 0.0, 0.0],
        phase_deg: vec![0.0, 0.0, 0.0],
        coherence: vec![1.0, 1.0, 1.0],
        delay_samples: 3,
        delay_ms: 0.0625,
        meas_channel: 0,
        ref_channel: 1,
        sr: 48000,
    };

    let input = OverlayInput {
        config: &config,
        frames: &frames,
        cell_views: &cell_views,
        selected: &[true, true],
        selection_order: &[0, 1],
        transfer: Some(&tf),
        active_meas: Some(0),
        active_meas_idx: 0,
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
    };

    let texts = run_overlay(input);
    let has_delay = texts.iter().any(|t| t.contains("+0.06 ms") && t.contains("+3 samp"));
    assert!(has_delay, "transfer delay not found in: {texts:?}");
}

// ── Hover readout ─────────────────────────────────────────────────

#[test]
fn overlay_shows_hover_db_readout() {
    let config = default_config();
    let frame = test_frame(1000.0, -3.0, 0.003, 0.005);
    let frames = [Some(frame)];
    let cell_views = [CellView::default()];

    let hover = HoverInfo {
        channel: 0,
        rect: Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 720.0)),
        cursor: Pos2::new(640.0, 360.0),
        freq_hz: 5000.0,
        readout: HoverReadout::Db(-12.3),
    };

    let input = OverlayInput {
        config: &config,
        frames: &frames,
        cell_views: &cell_views,
        selected: &[false],
        selection_order: &[],
        transfer: None,
        active_meas: None,
        active_meas_idx: 0,
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: Some(hover),
        show_help: false,
        monitor_params: None,
    };

    let texts = run_overlay(input);
    let has_hover = texts.iter().any(|t| t.contains("CH0") && t.contains("-12.3 dB") && t.contains("kHz"));
    assert!(has_hover, "hover readout not found in: {texts:?}");
}

// ── Notification ──────────────────────────────────────────────────

#[test]
fn overlay_shows_notification() {
    let config = default_config();
    let frames: [Option<DisplayFrame>; 0] = [];
    let cell_views: [CellView; 0] = [];

    let input = OverlayInput {
        config: &config,
        frames: &frames,
        cell_views: &cell_views,
        selected: &[],
        selection_order: &[],
        transfer: None,
        active_meas: None,
        active_meas_idx: 0,
        connected: true,
        notification: Some("saved"),
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
    };

    let texts = run_overlay(input);
    assert!(texts.iter().any(|t| t == "saved"), "notification not found in: {texts:?}");
}

// ── Sample rate and channel ───────────────────────────────────────

#[test]
fn overlay_shows_sample_rate() {
    let config = default_config();
    let frame = test_frame(1000.0, -3.0, 0.003, 0.005);
    let frames = [Some(frame)];
    let cell_views = [CellView::default()];

    let input = OverlayInput {
        config: &config,
        frames: &frames,
        cell_views: &cell_views,
        selected: &[false],
        selection_order: &[],
        transfer: None,
        active_meas: None,
        active_meas_idx: 0,
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
    };

    let texts = run_overlay(input);
    assert!(texts.iter().any(|t| t.contains("48000 Hz")), "sample rate not found in: {texts:?}");
}
