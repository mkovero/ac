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
    // Build a spectrum with a single bright peak at the requested frequency
    // so the bottom-left broadband readout has a real "peak @ freq" to show.
    // Other bins sit at -100 dBFS (the 10th-percentile floor the readout
    // reports).
    let freqs: Vec<f32> = (0..100).map(|i| 20.0 + i as f32 * 200.0).collect();
    let peak_idx = freqs
        .iter()
        .enumerate()
        .min_by(|a, b| (a.1 - freq).abs().partial_cmp(&(b.1 - freq).abs()).unwrap())
        .map(|(i, _)| i)
        .unwrap_or(0);
    let mut spec = vec![-100.0f32; 100];
    spec[peak_idx] = dbfs;
    DisplayFrame {
        spectrum: std::sync::Arc::new(spec),
        freqs: std::sync::Arc::new(freqs),
        meta: FrameMeta {
            freq_hz: freq,
            fundamental_dbfs: dbfs,
            thd_pct: thd,
            thdn_pct: thdn,
            in_dbu: None,
            sr: 48000,
            clipping: false,
            xruns: 0,
            leq_duration_s: None,
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
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
        n_real: 1,
        virtual_pairs: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
    };

    let texts = run_overlay(input);
    let has_readout = texts.iter().any(|t| {
        t.contains("peak") && t.contains("-3.0 dBFS") && t.contains("floor") && t.contains("span")
    });
    assert!(has_readout, "broadband readout not found in: {texts:?}");
    // THD must NOT appear in the monitor readout — it's meaningless on
    // broadband signals.
    let has_thd = texts.iter().any(|t| t.contains("THD"));
    assert!(!has_thd, "THD must not appear in monitor readout: {texts:?}");
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
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
        n_real: 1,
        virtual_pairs: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
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
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
        n_real: 1,
        virtual_pairs: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
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
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
        n_real: 1,
        virtual_pairs: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
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
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
        n_real: 1,
        virtual_pairs: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
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
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
        n_real: 0,
        virtual_pairs: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
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
        connected: false,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
        n_real: 0,
        virtual_pairs: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
    };

    let texts = run_overlay(input);
    assert!(texts.iter().any(|t| t.contains("disconnected")), "disconnected status not found in: {texts:?}");
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
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: Some(hover),
        show_help: false,
        monitor_params: None,
        n_real: 1,
        virtual_pairs: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
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
        connected: true,
        notification: Some("saved"),
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
        n_real: 0,
        virtual_pairs: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
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
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
        n_real: 1,
        virtual_pairs: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
    };

    let texts = run_overlay(input);
    assert!(texts.iter().any(|t| t.contains("48000 Hz")), "sample rate not found in: {texts:?}");
}

// ── Time-integration overlay tag ──────────────────────────────────

#[test]
fn overlay_shows_time_fast_tag() {
    let config = default_config();
    let frame = test_frame(1000.0, -3.0, 0.003, 0.005);
    let frames = [Some(frame)];
    let cell_views = [CellView::default()];

    let input = OverlayInput {
        config: &config,
        frames: &frames,
        cell_views: &cell_views,
        selected: &[false],
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
        n_real: 1,
        virtual_pairs: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: Some(crate::ui::overlay::TimeIntegrationOverlay {
            mode: "fast",
            tau_s: Some(0.125),
            duration_s: None,
        }),
        band_weighting: None,
        loudness: None,
    };

    let texts = run_overlay(input);
    let has_tag = texts.iter().any(|t| t.contains("time fast") && t.contains("125 ms"));
    assert!(has_tag, "time fast tag not found in: {texts:?}");
}

#[test]
fn overlay_shows_band_weighting_tag() {
    let config = default_config();
    let frame = test_frame(1000.0, -3.0, 0.003, 0.005);
    let frames = [Some(frame)];
    let cell_views = [CellView::default()];

    let input = OverlayInput {
        config: &config,
        frames: &frames,
        cell_views: &cell_views,
        selected: &[false],
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
        n_real: 1,
        virtual_pairs: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: Some("A"),
        loudness: None,
    };

    let texts = run_overlay(input);
    let has_tag = texts.iter().any(|t| t.contains("wt A"));
    assert!(has_tag, "wt A tag not found in: {texts:?}");
}

#[test]
fn overlay_shows_loudness_strip_with_r128_pass() {
    let config = default_config();
    let frame = test_frame(1000.0, -3.0, 0.003, 0.005);
    let frames = [Some(frame)];
    let cell_views = [CellView::default()];

    let input = OverlayInput {
        config: &config,
        frames: &frames,
        cell_views: &cell_views,
        selected: &[false],
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
        n_real: 1,
        virtual_pairs: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: Some(crate::data::types::LoudnessReadout {
            momentary_lkfs: Some(-22.9),
            short_term_lkfs: Some(-23.1),
            integrated_lkfs: Some(-23.0),
            lra_lu: 5.2,
            true_peak_dbtp: Some(-1.2),
            gated_duration_s: 17.3,
        }),
    };

    let texts = run_overlay(input);
    let joined = texts.join(" | ");
    assert!(joined.contains("M"), "loudness M marker missing: {joined}");
    assert!(joined.contains("LRA"), "LRA label missing: {joined}");
    assert!(joined.contains("dBTP"), "dBTP label missing: {joined}");
    assert!(
        joined.contains("R128 PASS"),
        "R128 PASS badge missing (integrated at -23.0): {joined}"
    );
}

#[test]
fn overlay_r128_fail_tag_when_far_off_target() {
    let config = default_config();
    let frame = test_frame(1000.0, -3.0, 0.003, 0.005);
    let frames = [Some(frame)];
    let cell_views = [CellView::default()];

    let input = OverlayInput {
        config: &config,
        frames: &frames,
        cell_views: &cell_views,
        selected: &[false],
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
        n_real: 1,
        virtual_pairs: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: Some(crate::data::types::LoudnessReadout {
            momentary_lkfs: Some(-10.0),
            short_term_lkfs: Some(-9.8),
            integrated_lkfs: Some(-10.0), // 13 LU too hot → FAIL
            lra_lu: 1.0,
            true_peak_dbtp: Some(-0.3),
            gated_duration_s: 42.0,
        }),
    };

    let texts = run_overlay(input);
    let joined = texts.join(" | ");
    assert!(
        joined.contains("R128 FAIL"),
        "R128 FAIL badge missing (integrated -10 vs -23 target): {joined}"
    );
}

#[test]
fn overlay_shows_leq_duration() {
    let config = default_config();
    let frame = test_frame(1000.0, -3.0, 0.003, 0.005);
    let frames = [Some(frame)];
    let cell_views = [CellView::default()];

    let input = OverlayInput {
        config: &config,
        frames: &frames,
        cell_views: &cell_views,
        selected: &[false],
        connected: true,
        notification: None,
        timing: None,
        gpu_supported: true,
        hover: None,
        show_help: false,
        monitor_params: None,
        n_real: 1,
        virtual_pairs: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: Some(crate::ui::overlay::TimeIntegrationOverlay {
            mode: "Leq",
            tau_s: None,
            duration_s: Some(12.5),
        }),
        band_weighting: None,
        loudness: None,
    };

    let texts = run_overlay(input);
    let has_tag = texts.iter().any(|t| t.contains("Leq") && t.contains("12.5 s"));
    assert!(has_tag, "Leq duration tag not found in: {texts:?}");
}
