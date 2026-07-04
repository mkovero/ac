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
            dbu_offset_db: None,
            peaks: std::sync::Arc::new(Vec::new()),
            spl_offset_db: None,
            mic_correction: None,
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

// ── Cursor readout (footer) ───────────────────────────────────────
//
// Pre-2026-05-04 the bottom-left footer showed broadband peak/floor/
// span/dBu while the cursor was idle. That toggled to a cursor-tracked
// readout on hover and back when the cursor left, which the user found
// confusing. Now: the footer renders ONLY when hovering, with values
// that follow the cursor.
//
// These tests verify the cal-aware readout still paints correctly,
// just in the new hover-driven form.

fn hover_at(channel: usize, freq_hz: f32, db: f32) -> HoverInfo {
    HoverInfo {
        channel,
        rect: Rect::from_min_size(Pos2::ZERO, Vec2::new(1280.0, 720.0)),
        cursor: Pos2::new(640.0, 360.0),
        freq_hz,
        readout: HoverReadout::Db(db),
    }
}

#[test]
fn overlay_shows_cursor_readout_when_hovering() {
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
        hover: Some(hover_at(0, 1000.0, -12.0)),
        show_help: false,
        monitor_params: None,
        n_real: 1,
        virtual_pairs: &[],
        virtual_transfer: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
        gonio_state: crate::data::types::StereoStatus::NoAudio,
        keytips: &[],
        peak_hold: false,
        min_hold: false,
    };

    let texts = run_overlay(input);
    let has_cursor = texts
        .iter()
        .any(|t| t.contains("kHz") && t.contains("-12.0") && t.contains("dBFS"));
    assert!(has_cursor, "cursor readout not found in: {texts:?}");
    let has_thd = texts.iter().any(|t| t.contains("THD"));
    assert!(!has_thd, "THD must not appear in cursor readout: {texts:?}");
    // No literal "cursor" prefix — context makes it obvious.
    assert!(
        !texts.iter().any(|t| t.starts_with("cursor")),
        "cursor footer must not start with 'cursor': {texts:?}"
    );
}

#[test]
fn overlay_hides_footer_when_not_hovering() {
    // Toggling broadband ↔ cursor every time the mouse crossed the cell
    // edge was the bug we fixed — assert the footer stays empty so we
    // don't regress.
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
        virtual_transfer: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
        gonio_state: crate::data::types::StereoStatus::NoAudio,
        keytips: &[],
        peak_hold: false,
        min_hold: false,
    };

    let texts = run_overlay(input);
    assert!(
        !texts.iter().any(|t| t.contains("peak")),
        "broadband readout must not paint without hover: {texts:?}"
    );
    // The freq+dBFS line that the cursor would emit must be absent;
    // any "X.XX kHz  -YY.Y dBFS" residue would mean the footer leaked.
    assert!(
        !texts
            .iter()
            .any(|t| t.contains("kHz") && t.contains("dBFS")),
        "cursor readout must not paint without hover: {texts:?}"
    );
}

#[test]
fn overlay_shows_dbspl_when_spl_calibrated() {
    // Pistonphone-cal'd channel: cursor at -3 dBFS with a +97 offset
    // must read 94.0 dB SPL in the cursor footer. The dBFS suffix
    // must not appear when SPL-cal'd.
    let config = default_config();
    let mut frame = test_frame(1000.0, -3.0, 0.003, 0.005);
    frame.meta.spl_offset_db = Some(97.0);
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
        hover: Some(hover_at(0, 1000.0, -3.0)),
        show_help: false,
        monitor_params: None,
        n_real: 1,
        virtual_pairs: &[],
        virtual_transfer: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
        gonio_state: crate::data::types::StereoStatus::NoAudio,
        keytips: &[],
        peak_hold: false,
        min_hold: false,
    };

    let texts = run_overlay(input);
    let has_spl = texts.iter().any(|t| t.contains("94.0 dB SPL"));
    assert!(has_spl, "dB SPL readout not found in: {texts:?}");
    let has_dbfs = texts.iter().any(|t| t.contains("dBFS"));
    assert!(!has_dbfs, "dBFS must not appear when SPL-cal'd: {texts:?}");
}

#[test]
fn overlay_shows_dbu_when_calibrated() {
    // Voltage-cal'd channel: cursor at -10 dBFS with a dbu_offset of
    // +10 dB → +0.0 dBu in the footer.
    let config = default_config();
    let mut frame = test_frame(1000.0, -3.0, 0.003, 0.005);
    frame.meta.dbu_offset_db = Some(10.0);
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
        hover: Some(hover_at(0, 1000.0, -10.0)),
        show_help: false,
        monitor_params: None,
        n_real: 1,
        virtual_pairs: &[],
        virtual_transfer: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
        gonio_state: crate::data::types::StereoStatus::NoAudio,
        keytips: &[],
        peak_hold: false,
        min_hold: false,
    };

    let texts = run_overlay(input);
    let has_dbu = texts.iter().any(|t| t.contains("+0.00 dBu"));
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
        virtual_transfer: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
        gonio_state: crate::data::types::StereoStatus::NoAudio,
        keytips: &[],
        peak_hold: false,
        min_hold: false,
    };

    let texts = run_overlay(input);
    assert!(
        texts.iter().any(|t| t == "CLIP"),
        "CLIP not found in: {texts:?}"
    );
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
        virtual_transfer: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
        gonio_state: crate::data::types::StereoStatus::NoAudio,
        keytips: &[],
        peak_hold: false,
        min_hold: false,
    };

    let texts = run_overlay(input);
    assert!(
        !texts.iter().any(|t| t == "CLIP"),
        "unexpected CLIP in: {texts:?}"
    );
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
        virtual_transfer: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
        gonio_state: crate::data::types::StereoStatus::NoAudio,
        keytips: &[],
        peak_hold: false,
        min_hold: false,
    };

    let texts = run_overlay(input);
    assert!(
        texts.iter().any(|t| t == "FROZEN"),
        "FROZEN not found in: {texts:?}"
    );
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
        virtual_transfer: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
        gonio_state: crate::data::types::StereoStatus::NoAudio,
        keytips: &[],
        peak_hold: false,
        min_hold: false,
    };

    let texts = run_overlay(input);
    assert!(
        texts.iter().any(|t| t.contains("connected")),
        "connection status not found in: {texts:?}"
    );
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
        virtual_transfer: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
        gonio_state: crate::data::types::StereoStatus::NoAudio,
        keytips: &[],
        peak_hold: false,
        min_hold: false,
    };

    let texts = run_overlay(input);
    assert!(
        texts.iter().any(|t| t.contains("disconnected")),
        "disconnected status not found in: {texts:?}"
    );
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
        virtual_transfer: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
        gonio_state: crate::data::types::StereoStatus::NoAudio,
        keytips: &[],
        peak_hold: false,
        min_hold: false,
    };

    let texts = run_overlay(input);
    // Cursor info lands in the bottom-left footer instead of next to
    // the cursor (no obstruction, no "cursor" prefix). Match on the
    // freq + dB pattern that cursor_readout emits.
    let has_hover = texts
        .iter()
        .any(|t| t.contains("kHz") && t.contains("-12.3"));
    assert!(has_hover, "cursor readout not found in: {texts:?}");
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
        virtual_transfer: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
        gonio_state: crate::data::types::StereoStatus::NoAudio,
        keytips: &[],
        peak_hold: false,
        min_hold: false,
    };

    let texts = run_overlay(input);
    assert!(
        texts.iter().any(|t| t == "saved"),
        "notification not found in: {texts:?}"
    );
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
        virtual_transfer: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
        gonio_state: crate::data::types::StereoStatus::NoAudio,
        keytips: &[],
        peak_hold: false,
        min_hold: false,
    };

    let texts = run_overlay(input);
    assert!(
        texts.iter().any(|t| t.contains("48000 Hz")),
        "sample rate not found in: {texts:?}"
    );
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
        virtual_transfer: &[],
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
        gonio_state: crate::data::types::StereoStatus::NoAudio,
        keytips: &[],
        peak_hold: false,
        min_hold: false,
    };

    let texts = run_overlay(input);
    let has_tag = texts
        .iter()
        .any(|t| t.contains("time fast") && t.contains("125 ms"));
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
        virtual_transfer: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: Some("A"),
        loudness: None,
        gonio_state: crate::data::types::StereoStatus::NoAudio,
        keytips: &[],
        peak_hold: false,
        min_hold: false,
    };

    let texts = run_overlay(input);
    let has_tag = texts.iter().any(|t| t.contains("wt A"));
    assert!(has_tag, "wt A tag not found in: {texts:?}");
}

#[test]
fn overlay_ember_shows_gain_window_and_hold_tags() {
    // #146: the ember status line surfaces the dB-window the gain trim
    // moves as `Y floor..ceiling dB`. #149: `peak`/`min` tags appear only
    // when the respective hold is armed.
    let mut config = default_config();
    config.view_mode = ViewMode::SpectrumEmber;
    let frame = test_frame(1000.0, -3.0, 0.003, 0.005);
    let frames = [Some(frame)];
    let cell_views = [CellView {
        db_min: -90.0,
        db_max: 0.0,
        ..CellView::default()
    }];

    let make = |peak_hold, min_hold| OverlayInput {
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
        virtual_transfer: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
        gonio_state: crate::data::types::StereoStatus::NoAudio,
        keytips: &[],
        peak_hold,
        min_hold,
    };

    // Holds off: gain window present, no peak/min tags.
    let texts = run_overlay(make(false, false));
    let ember = texts
        .iter()
        .find(|t| t.contains("spectrum (ember)"))
        .unwrap_or_else(|| panic!("ember status line not found in: {texts:?}"));
    assert!(
        ember.contains("Y -90..0 dB"),
        "gain window not shown: {ember:?}"
    );
    assert!(
        !ember.contains("peak"),
        "peak tag must be absent: {ember:?}"
    );
    assert!(!ember.contains("min"), "min tag must be absent: {ember:?}");

    // Holds armed: both tags appear.
    let texts = run_overlay(make(true, true));
    let ember = texts
        .iter()
        .find(|t| t.contains("spectrum (ember)"))
        .unwrap_or_else(|| panic!("ember status line not found in: {texts:?}"));
    assert!(ember.contains("peak"), "peak tag missing: {ember:?}");
    assert!(ember.contains("min"), "min tag missing: {ember:?}");
}

#[test]
fn overlay_ember_shows_cursor_readout_on_hover() {
    // #154: the SpectrumEmber footer is no longer suppressed. Hovering
    // surfaces freq + the trace bin magnitude sampled from the frame
    // spectrum — NOT the geometric cursor-Y carried by the old Db payload.
    let mut config = default_config();
    config.view_mode = ViewMode::SpectrumEmber;
    // Bright bin = -3 dBFS at ~1 kHz; the rest of the spectrum sits at -100.
    let frame = test_frame(1000.0, -3.0, 0.003, 0.005);
    let frames = [Some(frame)];
    let cell_views = [CellView {
        db_min: -90.0,
        db_max: 0.0,
        ..CellView::default()
    }];
    // Geometric payload deliberately -12 dBFS to prove it is ignored.
    let mut hover = hover_at(0, 1000.0, -12.0);
    hover.readout = HoverReadout::SpectrumBin;

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
        virtual_transfer: &[],
        active_palette: 0,
        smoothing_frac: None,
        ioct_bpo: None,
        tier_badge: None,
        time_integration: None,
        band_weighting: None,
        loudness: None,
        gonio_state: crate::data::types::StereoStatus::NoAudio,
        keytips: &[],
        peak_hold: false,
        min_hold: false,
    };

    let texts = run_overlay(input);
    let has_readout = texts
        .iter()
        .any(|t| t.contains("kHz") && t.contains("dBFS"));
    assert!(has_readout, "ember cursor readout missing: {texts:?}");
    assert!(
        texts.iter().any(|t| t.contains("-3.0")),
        "amplitude must be the sampled bin magnitude (-3 dBFS), not the \
         geometric -12 payload: {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains("-12.0")),
        "geometric cursor-Y dB must not appear: {texts:?}"
    );
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
        virtual_transfer: &[],
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
            spl_offset_db: None,
        }),
        gonio_state: crate::data::types::StereoStatus::NoAudio,
        keytips: &[],
        peak_hold: false,
        min_hold: false,
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
        virtual_transfer: &[],
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
            spl_offset_db: None,
        }),
        gonio_state: crate::data::types::StereoStatus::NoAudio,
        keytips: &[],
        peak_hold: false,
        min_hold: false,
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
        virtual_transfer: &[],
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
        gonio_state: crate::data::types::StereoStatus::NoAudio,
        keytips: &[],
        peak_hold: false,
        min_hold: false,
    };

    let texts = run_overlay(input);
    let has_tag = texts
        .iter()
        .any(|t| t.contains("Leq") && t.contains("12.5 s"));
    assert!(has_tag, "Leq duration tag not found in: {texts:?}");
}

// ── #163 virtual (transfer) cell dB-re-unity window ────────────────
//
// A6: a virtual cell's dB window is seeded to +20..-60 (re unity, not
// dBFS) on creation, and `grid::draw_grid` paints a distinguished 0 dB
// gridline for it (`is_virtual = true`) that a real cell's grid never
// draws even when 0 dB happens to fall in its window.

fn collect_line_segments(shape: &epaint::Shape, out: &mut Vec<(Pos2, Pos2, epaint::Stroke)>) {
    match shape {
        epaint::Shape::LineSegment { points, stroke } => {
            out.push((points[0], points[1], stroke.clone().into()));
        }
        epaint::Shape::Vec(shapes) => {
            for s in shapes {
                collect_line_segments(s, out);
            }
        }
        _ => {}
    }
}

fn run_grid_paint(view: &CellView, is_virtual: bool) -> Vec<(Pos2, Pos2, epaint::Stroke)> {
    let ctx = egui::Context::default();
    let rect = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(400.0, 300.0));
    let output = ctx.run(default_raw_input(), |ctx| {
        let painter = ctx.layer_painter(egui::LayerId::background());
        crate::render::grid::draw_grid(
            &painter,
            rect,
            view,
            ViewMode::Spectrum,
            true,
            true,
            None,
            None,
            is_virtual,
        );
    });
    let mut segs = Vec::new();
    for cs in &output.shapes {
        collect_line_segments(&cs.shape, &mut segs);
    }
    segs
}

/// Reproduces the resize-time seed in `App::redraw`'s virtual-snapshot
/// block: new virtual cell slots get `theme::VIRTUAL_DB_MIN/MAX`, not
/// `CellView::default()`'s dBFS window.
#[test]
fn virtual_cell_seeds_unity_db_window() {
    let mut cell_views: Vec<CellView> = vec![CellView::default()]; // 1 real slot
    let n_real = 1;
    let n_total = 2; // + 1 virtual slot appended
    let first_new = cell_views.len();
    cell_views.resize(n_total, CellView::default());
    for cv in cell_views.iter_mut().skip(first_new.max(n_real)) {
        cv.db_min = crate::theme::VIRTUAL_DB_MIN;
        cv.db_max = crate::theme::VIRTUAL_DB_MAX;
    }
    assert_eq!(cell_views[0].db_min, crate::theme::DEFAULT_DB_MIN);
    assert_eq!(cell_views[0].db_max, crate::theme::DEFAULT_DB_MAX);
    assert_eq!(cell_views[1].db_min, -60.0);
    assert_eq!(cell_views[1].db_max, 20.0);
}

#[test]
fn virtual_cell_grid_paints_distinguished_unity_gridline() {
    let view = CellView {
        db_min: crate::theme::VIRTUAL_DB_MIN,
        db_max: crate::theme::VIRTUAL_DB_MAX,
        ..CellView::default()
    };
    let rect = Rect::from_min_size(Pos2::new(0.0, 0.0), Vec2::new(400.0, 300.0));
    let expected_y = {
        let db_span = view.db_max - view.db_min;
        let t = -view.db_min / db_span;
        rect.bottom() - t * rect.height()
    };

    let virtual_segs = run_grid_paint(&view, true);
    let has_unity_line = virtual_segs.iter().any(|(a, b, stroke)| {
        (a.y - expected_y).abs() < 0.5 && (b.y - expected_y).abs() < 0.5 && stroke.width > 1.0
        // thicker than the regular 1.0px grid stroke
    });
    assert!(
        has_unity_line,
        "no distinguished 0 dB gridline found at y={expected_y} in {virtual_segs:?}"
    );

    // Same window on a *real* cell (is_virtual = false) must not draw the
    // distinguished line — only the regular grid stroke at that y.
    let real_segs = run_grid_paint(&view, false);
    let real_has_unity_line = real_segs.iter().any(|(a, b, stroke)| {
        (a.y - expected_y).abs() < 0.5 && (b.y - expected_y).abs() < 0.5 && stroke.width > 1.0
    });
    assert!(
        !real_has_unity_line,
        "real cell should not paint the virtual-only unity gridline"
    );
}
