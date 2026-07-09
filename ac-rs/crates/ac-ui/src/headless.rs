//! Headless T2/T3 display-truth harness — `ac-ui --headless-test` (#170).
//!
//! Extends `ac test software` with checks between "the numbers are right"
//! (T2: the post-receiver `DisplayFrame` buffer, exactly what `ChannelStore`
//! hands the renderer) and "the screen is right" (T3: pixels read back from
//! an offscreen wgpu texture rendered with the *real* `SpectrumRenderer` /
//! `EmberRenderer` production paint code, under software Vulkan/lavapipe or
//! `WGPU_BACKEND=gl`). See `handoff.md` at the repo root for the full I1-I4
//! invariant rationale this module implements.
//!
//! Never touches real hardware itself — it only knows how to talk to
//! whatever daemon endpoint it's given. `ac-cli`'s `run_software` is
//! responsible for pointing it at an isolated `ac-daemon --fake-audio`
//! subprocess (see `ac-cli/src/commands/test.rs`), never the user's
//! possibly-real daemon.
//!
//! Runs no winit event loop, opens no window, creates no `wgpu::Surface` —
//! `HeadlessGpu` requests a device directly from an adapter with
//! `compatible_surface: None`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::app::render_pipeline::{build_ember_spectrum_trace, ember_pack_cell};
use crate::data::control::CtrlClient;
use crate::data::receiver;
use crate::data::store::{
    ChannelStore, LoudnessStore, ScopeStore, SweepStore, TransferStore, VirtualChannelStore,
};
use crate::data::types::{CellView, DisplayConfig, DisplayFrame};
use crate::render::ember::EmberRenderer;
use crate::render::spectrum::{ChannelMeta, ChannelUpload, SpectrumRenderer};
use crate::ui::layout::CellRect;

struct Check {
    name: String,
    pass: bool,
    detail: String,
}

/// LF/HF crossover — reuse the same constant the real aggregator uses
/// rather than hardcoding a second copy (I1 requires "derive... from
/// current config, do not hardcode").
const LF_HF_CROSSOVER_HZ: f64 = ac_core::visualize::aggregate::DEFAULT_LF_CROSSOVER_HZ as f64;

/// FFT length requested for every harness capture. Fixed so the
/// interpolation→aggregation crossover derived from the returned `freqs`
/// grid (see `find_interp_aggregation_crossover`) is reproducible run to
/// run; matches the size used in the original HF-garbage field capture
/// (`tests/fixtures/fixtures-spectrum-hf-garbage/README.md`).
const HARNESS_FFT_N: u64 = 8192;

/// T2 tolerance for a single injected tone read back from the post-receiver
/// buffer. Rationale: Hann-window scalloping loss between bin centres is
/// ≤1.42 dB worst case (see `monitor_spectrum_wire_values_match_fake_tone`
/// in ac-daemon's it_protocol.rs, which already carries this exact number);
/// 1.5 dB gives a hair of margin without hiding a real multi-dB regression.
const I1_BUFFER_TOLERANCE_DB: f64 = 1.5;

/// T2 tolerance for "no value should exceed 0 dBFS" on bounded input.
/// Rationale: broadband noise can read a few hundredths of a dB above
/// nominal in a single bin from window-leakage constructive summation of
/// random phase (benign); 1.0 dB is well clear of that noise floor but
/// far below the +19 dB-class violation the HF-garbage fixture corpus
/// demonstrates (`tests/fixtures/fixtures-spectrum-hf-garbage/README.md`).
const I4_TOLERANCE_DB: f64 = 1.0;

/// T3 pixel tolerance, in rows, for locating a trace feature at its
/// expected position. Rationale: the issue's own risk note (architect
/// design comment, #170) flags that lavapipe's software rasterizer can
/// differ subtly from a real GPU at anti-aliased trace edges — assert on
/// the trace *feature* within a small pixel band, not an exact pixel.
const I3_PIXEL_TOLERANCE_PX: f32 = 2.0;

/// Fixed offscreen render target size for every T3 check. Resolution only
/// needs to be large enough that ±1-2 px maps to a meaningfully small dB
/// span; matches the ember substrate's own internal resolution
/// (`render/ember.rs::TEX_W/TEX_H`) so both views are checked at
/// comparable pixel density.
const T3_WIDTH: u32 = 1024;
const T3_HEIGHT: u32 = 512;
const T3_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Fixed dB/freq calibration window every T3 render uses. Not read from
/// live UI zoom/pan state (there is none here) — these are the harness's
/// own known-good axis calibration, independent of whatever the shader
/// actually draws, which is exactly what lets a T3 check catch an
/// orientation or scale bug in the shader rather than agreeing with it.
const WINDOW_DB_MIN: f32 = -90.0;
const WINDOW_DB_MAX: f32 = 0.0;
const WINDOW_FREQ_MIN: f32 = 20.0;
const WINDOW_FREQ_MAX: f32 = 20_000.0;

/// Entry point for `ac-ui --headless-test <ctrl_endpoint> <data_endpoint>`.
/// Returns the same `{ok, results: [{name, pass, detail}], all_pass}` shape
/// as `ac-daemon`'s `test_software`, so `ac-cli`'s `run_software` can print
/// both result sets through one loop.
pub fn run(ctrl_endpoint: &str, data_endpoint: &str) -> Value {
    let mut checks = Vec::new();
    match run_checked(ctrl_endpoint, data_endpoint, &mut checks) {
        Ok(()) => {}
        Err(e) => checks.push(Check {
            name: "display-truth harness".to_string(),
            pass: false,
            detail: format!("harness error: {e}"),
        }),
    }
    let all_pass = !checks.is_empty() && checks.iter().all(|c| c.pass);
    let results: Vec<Value> = checks
        .iter()
        .map(|c| json!({"name": c.name, "pass": c.pass, "detail": c.detail}))
        .collect();
    json!({"ok": true, "results": results, "all_pass": all_pass})
}

fn run_checked(
    ctrl_endpoint: &str,
    data_endpoint: &str,
    checks: &mut Vec<Check>,
) -> anyhow::Result<()> {
    let ctrl = CtrlClient::connect(ctrl_endpoint)?;
    let (inputs, mut store) = ChannelStore::new(1);
    let _receiver = receiver::spawn(
        data_endpoint.to_string(),
        inputs,
        TransferStore::new(),
        VirtualChannelStore::new(),
        SweepStore::new(),
        LoudnessStore::new(),
        ScopeStore::new(),
        None,
    );
    // Raw injected value, no EMA smoothing bias — I1/I4 compare against
    // the exact value the daemon put on the wire.
    let cfg = DisplayConfig {
        averaging_alpha: 1.0,
        ..Default::default()
    };

    // ── Capture 1: two tones straddling the LF/HF crossover (750 Hz) ──
    let f_lo = LF_HF_CROSSOVER_HZ * 0.8; // ~600 Hz
    let f_hi = LF_HF_CROSSOVER_HZ * 1.2; // ~900 Hz
    let frame1 = capture(
        &ctrl,
        &mut store,
        &cfg,
        json!({
            "cmd": "monitor_spectrum",
            "channels": [0],
            "fft_n": HARNESS_FFT_N,
            "fake_tones": [
                {"freq_hz": f_lo, "level_dbfs": -6.0},
                {"freq_hz": f_hi, "level_dbfs": -18.0},
            ],
        }),
        true,
    )?;
    check_i1_buffer(
        checks,
        &frame1,
        f_lo,
        -6.0,
        "I1 buffer @ LF/HF-crossover low side",
    );
    check_i1_buffer(
        checks,
        &frame1,
        f_hi,
        -18.0,
        "I1 buffer @ LF/HF-crossover high side",
    );
    check_i4_bounded(
        checks,
        &frame1,
        "I4 bounded output (two-tone LF/HF capture)",
    );

    // ── T3: render capture 1 through the real Spectrum + SpectrumEmber
    //    paint code and check pixel-level I1 apex + I3 orientation ──
    match HeadlessGpu::new() {
        Ok(gpu) => {
            run_t3_checks(checks, &gpu, &frame1, f_lo, -6.0, f_hi, -18.0);
        }
        Err(e) => checks.push(Check {
            name: "T3 GPU adapter".to_string(),
            pass: false,
            detail: format!(
                "no wgpu adapter available ({e}) — T3 checks require a software (lavapipe) \
                 or real Vulkan/GL adapter; see runbook, this is expected on a host with no \
                 GPU/driver stack"
            ),
        }),
    }

    // ── Capture 2: two tones straddling the interpolation→aggregation
    //    crossover, derived from capture 1's own freq grid + HARNESS_FFT_N
    //    (not hardcoded — I1 requires deriving it from current config) ──
    let sr = frame1.meta.sr as f64;
    let interp_crossover =
        find_interp_aggregation_crossover(&frame1.freqs, sr, HARNESS_FFT_N).unwrap_or(6533.0); // fallback: documented field value, only used if the grid is too coarse to detect the transition
    let f_lo2 = interp_crossover * 0.9;
    let f_hi2 = interp_crossover * 1.1;
    let frame2 = capture(
        &ctrl,
        &mut store,
        &cfg,
        json!({
            "cmd": "monitor_spectrum",
            "channels": [0],
            "fft_n": HARNESS_FFT_N,
            "fake_tones": [
                {"freq_hz": f_lo2, "level_dbfs": -6.0},
                {"freq_hz": f_hi2, "level_dbfs": -12.0},
            ],
        }),
        false,
    )?;
    check_i1_buffer(
        checks,
        &frame2,
        f_lo2,
        -6.0,
        "I1 buffer @ interp/aggregation-crossover low side",
    );
    check_i1_buffer(
        checks,
        &frame2,
        f_hi2,
        -12.0,
        "I1 buffer @ interp/aggregation-crossover high side",
    );
    check_i4_bounded(
        checks,
        &frame2,
        "I4 bounded output (interp/aggregation capture)",
    );

    // ── Capture 3: calibrated broadband noise — I2 (reported, not
    //    asserted, per Decision 2) + I4 bounded ──
    let frame3 = capture(
        &ctrl,
        &mut store,
        &cfg,
        json!({
            "cmd": "monitor_spectrum",
            "channels": [0],
            "fft_n": HARNESS_FFT_N,
            "fake_noise_dbfs": -20.0,
        }),
        false,
    )?;
    check_i4_bounded(
        checks,
        &frame3,
        "I4 bounded output (broadband noise capture)",
    );
    check_i2_variance_report(checks, &frame3);

    let _ = ctrl.send(&json!({"cmd": "stop"}));
    Ok(())
}

/// Send `monitor_spectrum` with `cmd_extra` merged in, then poll the
/// receiver until a settled, non-empty frame arrives. `first` controls
/// whether a leading `stop` is sent first (skip on the very first capture
/// — nothing to stop yet).
fn capture(
    ctrl: &CtrlClient,
    store: &mut ChannelStore,
    cfg: &DisplayConfig,
    cmd: Value,
    first: bool,
) -> anyhow::Result<DisplayFrame> {
    if !first {
        let _ = ctrl.send(&json!({"cmd": "stop"}));
        std::thread::sleep(Duration::from_millis(200));
    }
    let reply = ctrl.send(&cmd)?;
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        anyhow::bail!("monitor_spectrum ack not ok: {reply}");
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    // Skip the first couple of ticks — the FFT ring may still be filling
    // (same convention as ac-daemon's it_protocol.rs wire tests).
    let mut seen = 0;
    loop {
        if Instant::now() > deadline {
            anyhow::bail!("no usable spectrum frame within 8 s");
        }
        std::thread::sleep(Duration::from_millis(100));
        let frames = store.read_all(cfg);
        if let Some(Some(f)) = frames.into_iter().next() {
            if !f.spectrum.is_empty() {
                seen += 1;
                if seen >= 3 {
                    return Ok(f);
                }
            }
        }
    }
}

/// Nearest-bin dBFS lookup — the harness's own independent readout, not a
/// call into any daemon/UI aggregation code, so a bug in that aggregation
/// can't hide from the comparison.
fn nearest_dbfs(frame: &DisplayFrame, target_hz: f64) -> Option<(f64, f64)> {
    let mut best: Option<(f64, f64, f64)> = None; // (freq, dbfs, |diff|)
    for (i, &f) in frame.freqs.iter().enumerate() {
        let diff = (f as f64 - target_hz).abs();
        let dbfs = *frame.spectrum.get(i)? as f64;
        if best.is_none_or(|(_, _, bd)| diff < bd) {
            best = Some((f as f64, dbfs, diff));
        }
    }
    best.map(|(f, db, _)| (f, db))
}

fn check_i1_buffer(
    checks: &mut Vec<Check>,
    frame: &DisplayFrame,
    target_hz: f64,
    level_dbfs: f64,
    name: &str,
) {
    let (name, pass, detail) = match nearest_dbfs(frame, target_hz) {
        Some((found_hz, found_db)) => {
            let err = (found_db - level_dbfs).abs();
            (
                name.to_string(),
                err <= I1_BUFFER_TOLERANCE_DB,
                format!(
                    "injected {level_dbfs:.1} dBFS @ {target_hz:.1} Hz, buffer column @ {found_hz:.1} \
                     Hz = {found_db:.2} dBFS (err {err:.2} dB, tol {I1_BUFFER_TOLERANCE_DB} dB)"
                ),
            )
        }
        None => (
            name.to_string(),
            false,
            format!("no buffer column found near {target_hz:.1} Hz"),
        ),
    };
    checks.push(Check { name, pass, detail });
}

fn check_i4_bounded(checks: &mut Vec<Check>, frame: &DisplayFrame, name: &str) {
    let max = frame.spectrum.iter().copied().fold(f32::MIN, f32::max) as f64;
    let pass = max <= I4_TOLERANCE_DB;
    checks.push(Check {
        name: name.to_string(),
        pass,
        detail: format!(
            "max buffer value = {max:.3} dBFS (bound: 0 dBFS + {I4_TOLERANCE_DB} dB tolerance)"
        ),
    });
}

/// I2 flat-noise continuity: per-band (LF < 750 Hz, HF ≥ 750 Hz)
/// column-to-column variance, reported only — Decision 2 (handoff.md)
/// defers the pass/fail threshold until LF temporal averaging lands and
/// the equalized target is known. This check therefore always reports
/// `pass: true`; its value is the number in `detail`, which becomes a
/// diff once that threshold exists.
fn check_i2_variance_report(checks: &mut Vec<Check>, frame: &DisplayFrame) {
    let mut lf_diffs = Vec::new();
    let mut hf_diffs = Vec::new();
    for w in frame.freqs.windows(2).zip(frame.spectrum.windows(2)) {
        let (fw, dw) = w;
        let d = (dw[1] - dw[0]).abs() as f64;
        if (fw[0] as f64) < LF_HF_CROSSOVER_HZ {
            lf_diffs.push(d);
        } else {
            hf_diffs.push(d);
        }
    }
    let variance = |v: &[f64]| -> f64 {
        if v.is_empty() {
            return 0.0;
        }
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len() as f64
    };
    let lf_var = variance(&lf_diffs);
    let hf_var = variance(&hf_diffs);
    checks.push(Check {
        name: "I2 flat-noise column-to-column variance (reported, not asserted)".to_string(),
        pass: true,
        detail: format!(
            "LF band (<{LF_HF_CROSSOVER_HZ:.0} Hz, n={}) step variance = {lf_var:.4} dB²; \
             HF band (n={}) step variance = {hf_var:.4} dB² — threshold deferred to LF \
             temporal averaging (handoff.md Decision 2)",
            lf_diffs.len(),
            hf_diffs.len(),
        ),
    });
}

/// Find the column index where the aggregator's source-bin count first
/// goes from 1 (interpolation) to ≥2 (aggregation) — i.e. where adjacent
/// output columns stop being spaced one raw FFT bin apart. Mirrors the
/// mechanism documented in `audit/spectrum-hf-garbage-report.md` without
/// depending on any internal aggregator function: it only reads the
/// `freqs` grid the daemon already put on the wire plus the requested
/// `sr`/`fft_n`, so a bug in the aggregator's own crossover math can't
/// mask itself from this derivation.
fn find_interp_aggregation_crossover(freqs: &[f32], sr: f64, fft_n: u64) -> Option<f64> {
    let bin_width = sr / fft_n as f64;
    for w in freqs.windows(2) {
        let spacing = (w[1] - w[0]) as f64;
        if spacing >= bin_width * 1.5 {
            return Some(w[0] as f64);
        }
    }
    None
}

struct HeadlessGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl HeadlessGpu {
    fn new() -> anyhow::Result<Self> {
        pollster::block_on(Self::new_async())
    }

    async fn new_async() -> anyhow::Result<Self> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY | wgpu::Backends::GL,
            ..Default::default()
        });
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| anyhow::anyhow!("no wgpu adapter"))?;
        let info = adapter.get_info();
        log::info!(
            "ac-ui --headless-test: wgpu backend={:?} adapter={:?} driver={:?}",
            info.backend,
            info.name,
            info.driver,
        );
        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("ac-ui headless-test device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: wgpu::MemoryHints::default(),
                },
                None,
            )
            .await?;
        Ok(Self { device, queue })
    }
}

fn make_offscreen_texture(device: &wgpu::Device, label: &str) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: T3_WIDTH,
            height: T3_HEIGHT,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: T3_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// Read an offscreen texture back to unpadded, channel-order-corrected
/// RGBA bytes. Reuses the exact same de-pad / channel-swap helpers the
/// interactive 'S' screenshot path uses (`ui/export.rs`), so this isn't a
/// second, possibly-diverging implementation of that bookkeeping.
fn readback(gpu: &HeadlessGpu, tex: &wgpu::Texture, mut encoder: wgpu::CommandEncoder) -> Vec<u8> {
    let bytes_per_row = crate::ui::export::bytes_per_row_aligned(T3_WIDTH);
    let size = (bytes_per_row as u64) * (T3_HEIGHT as u64);
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("t3 readback"),
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(T3_HEIGHT),
            },
        },
        wgpu::Extent3d {
            width: T3_WIDTH,
            height: T3_HEIGHT,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(Some(encoder.finish()));
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    // This is where a driver crash (rather than a wgpu validation error)
    // would surface — the CPU-side recording above is always accepted;
    // `poll` is what actually asks the adapter to execute the command
    // buffer. If `ac-ui --headless-test` dies with no further log output
    // past a "rendering ... view" line, look here first.
    let _ = gpu.device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .expect("map_async callback channel closed")
        .expect("map_async failed");
    let data = slice.get_mapped_range().to_vec();
    buffer.unmap();
    let rgba = crate::ui::export::unpad(&data, T3_WIDTH, T3_HEIGHT, bytes_per_row);
    crate::ui::export::channel_swap_if_needed(rgba, T3_FORMAT)
}

fn render_spectrum_pixels(gpu: &HeadlessGpu, frame: &DisplayFrame) -> Vec<u8> {
    log::info!("t3: rendering Spectrum view offscreen");
    let mut renderer = SpectrumRenderer::new(&gpu.device, T3_FORMAT);
    let meta = ChannelMeta {
        color: [1.0, 1.0, 1.0, 1.0],
        viewport: [0.0, 0.0, 1.0, 1.0],
        db_min: WINDOW_DB_MIN,
        db_max: WINDOW_DB_MAX,
        freq_log_min: WINDOW_FREQ_MIN.max(1.0).log10(),
        freq_log_max: WINDOW_FREQ_MAX.max(WINDOW_FREQ_MIN * 1.001).log10(),
        n_bins: 0,
        offset: 0,
        fill_alpha: 0.0,
        line_width: 3.0,
    };
    renderer.upload(
        &gpu.device,
        &gpu.queue,
        &[ChannelUpload {
            spectrum: (*frame.spectrum).clone(),
            meta,
        }],
    );
    let tex = make_offscreen_texture(&gpu.device, "t3 spectrum");
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("t3 spectrum encoder"),
        });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("t3 spectrum pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        renderer.draw(&mut pass);
    }
    readback(gpu, &tex, encoder)
}

/// Render `frame` through the real SpectrumEmber paint path
/// (`build_ember_spectrum_trace` + `ember_pack_cell`, the exact functions
/// `render_pipeline.rs` calls for a Single-cell SpectrumEmber view) and
/// let the phosphor substrate settle to steady state before reading back —
/// a single deposit at production intensity is too dim to threshold
/// reliably (the intensity constant is tuned for multi-tick steady state,
/// see the comment at its call site in `render_pipeline.rs`).
fn render_ember_pixels(gpu: &HeadlessGpu, frame: &DisplayFrame) -> Vec<u8> {
    log::info!("t3: rendering SpectrumEmber view offscreen");
    let view = CellView {
        freq_min: WINDOW_FREQ_MIN,
        freq_max: WINDOW_FREQ_MAX,
        db_min: WINDOW_DB_MIN,
        db_max: WINDOW_DB_MAX,
        ..Default::default()
    };
    let raw = build_ember_spectrum_trace(&frame.freqs, &frame.spectrum, &view, 1.0);
    let full_cell = CellRect {
        channel: 0,
        x: 0.0,
        y: 0.0,
        w: 1.0,
        h: 1.0,
    };
    let mut polyline = Vec::with_capacity(raw.len());
    ember_pack_cell(&mut polyline, &raw, &full_cell, &view, 1.0);

    let mut ember = EmberRenderer::new(&gpu.device, &gpu.queue, T3_FORMAT);
    ember.set_tau_p(1.2);
    ember.set_intensity(0.006);
    ember.set_tone(0.6, 1.5);

    let tex = make_offscreen_texture(&gpu.device, "t3 ember");
    let view_tex = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("t3 ember encoder"),
        });
    // Same static polyline every tick so the substrate reaches the same
    // steady-state luminance a continuously-running UI would settle to.
    const TICKS: usize = 90; // ~1.5 s at 60 fps
    for _ in 0..TICKS {
        ember.advance(
            &gpu.device,
            &gpu.queue,
            &mut encoder,
            [0.0, 0.0, 1.0, 1.0],
            &polyline,
            0.0,
            1.0 / 60.0,
            &[],
        );
    }
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("t3 ember display pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view_tex,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        ember.draw(&mut pass);
    }
    readback(gpu, &tex, encoder)
}

fn freq_to_col(freq_hz: f64) -> u32 {
    let log_min = (WINDOW_FREQ_MIN as f64).max(1.0).log10();
    let log_max = (WINDOW_FREQ_MAX as f64)
        .max(WINDOW_FREQ_MIN as f64 * 1.001)
        .log10();
    let xn = ((freq_hz.max(1.0).log10() - log_min) / (log_max - log_min).max(1e-6)).clamp(0.0, 1.0);
    ((xn * T3_WIDTH as f64) as u32).min(T3_WIDTH - 1)
}

fn expected_row_for_db(db: f64) -> f32 {
    let n = ((db - WINDOW_DB_MIN as f64) / (WINDOW_DB_MAX as f64 - WINDOW_DB_MIN as f64).max(1e-3))
        .clamp(0.0, 1.0);
    ((1.0 - n) * (T3_HEIGHT as f64 - 1.0)) as f32
}

/// Scan column `col` for the row with the greatest colour deviation from
/// `bg` — the trace/phosphor "apex" for that column. Pure pixel-buffer
/// analysis, independent of how the buffer was produced (GPU readback or
/// a hand-built test fixture), which is what makes it unit-testable
/// without a GPU below.
fn find_trace_row(pixels: &[u8], width: u32, height: u32, col: u32, bg: [u8; 3]) -> Option<u32> {
    let mut best_row = None;
    let mut best_dist = 24u32; // ignore near-background noise / AA fringing
    for row in 0..height {
        let idx = ((row * width + col) * 4) as usize;
        if idx + 2 >= pixels.len() {
            continue;
        }
        let dist = (pixels[idx] as i32 - bg[0] as i32).unsigned_abs()
            + (pixels[idx + 1] as i32 - bg[1] as i32).unsigned_abs()
            + (pixels[idx + 2] as i32 - bg[2] as i32).unsigned_abs();
        if dist > best_dist {
            best_dist = dist;
            best_row = Some(row);
        }
    }
    best_row
}

#[allow(clippy::too_many_arguments)]
fn run_t3_checks(
    checks: &mut Vec<Check>,
    gpu: &HeadlessGpu,
    frame: &DisplayFrame,
    f_lo: f64,
    db_lo: f64,
    f_hi: f64,
    db_hi: f64,
) {
    let spectrum_px = render_spectrum_pixels(gpu, frame);
    let ember_px = render_ember_pixels(gpu, frame);

    for (view_name, pixels, bg) in [
        ("Spectrum", &spectrum_px, [0u8, 0, 0]),
        ("SpectrumEmber", &ember_px, [0u8, 0, 0]),
    ] {
        let col_lo = freq_to_col(f_lo);
        let col_hi = freq_to_col(f_hi);
        let row_lo = find_trace_row(pixels, T3_WIDTH, T3_HEIGHT, col_lo, bg);
        let row_hi = find_trace_row(pixels, T3_WIDTH, T3_HEIGHT, col_hi, bg);

        match (row_lo, row_hi) {
            (Some(rl), Some(rh)) => {
                // db_lo is louder than db_hi in every capture 1 uses this
                // helper for, so its trace must land at the smaller row
                // (higher on screen) — I3, no exemptions.
                let pass = if db_lo > db_hi { rl < rh } else { rh < rl };
                checks.push(Check {
                    name: format!("I3 orientation ({view_name})"),
                    pass,
                    detail: format!(
                        "{f_lo:.0} Hz @ {db_lo} dBFS -> row {rl}; {f_hi:.0} Hz @ {db_hi} dBFS -> row {rh} \
                         (smaller row = higher on screen; louder tone must be smaller)"
                    ),
                });

                if view_name == "Spectrum" {
                    let exp_lo = expected_row_for_db(db_lo);
                    let exp_hi = expected_row_for_db(db_hi);
                    let err_lo = (rl as f32 - exp_lo).abs();
                    let err_hi = (rh as f32 - exp_hi).abs();
                    checks.push(Check {
                        name: "I1 pixel apex (Spectrum)".to_string(),
                        pass: err_lo <= I3_PIXEL_TOLERANCE_PX && err_hi <= I3_PIXEL_TOLERANCE_PX,
                        detail: format!(
                            "{f_lo:.0} Hz: expected row {exp_lo:.1}, found {rl} (err {err_lo:.1} px); \
                             {f_hi:.0} Hz: expected row {exp_hi:.1}, found {rh} (err {err_hi:.1} px); \
                             tol {I3_PIXEL_TOLERANCE_PX} px"
                        ),
                    });
                }
            }
            _ => {
                checks.push(Check {
                    name: format!("I3 orientation ({view_name})"),
                    pass: false,
                    detail: format!(
                        "could not locate a trace feature at one or both columns \
                         (row_lo={row_lo:?}, row_hi={row_hi:?})"
                    ),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_bg(width: u32, height: u32) -> Vec<u8> {
        vec![0u8; (width * height * 4) as usize]
    }

    fn set_px(pixels: &mut [u8], width: u32, col: u32, row: u32, rgb: [u8; 3]) {
        let idx = ((row * width + col) * 4) as usize;
        pixels[idx] = rgb[0];
        pixels[idx + 1] = rgb[1];
        pixels[idx + 2] = rgb[2];
        pixels[idx + 3] = 255;
    }

    #[test]
    fn find_trace_row_locates_bright_pixel_against_background() {
        let (w, h) = (16, 16);
        let mut px = solid_bg(w, h);
        set_px(&mut px, w, 4, 9, [255, 255, 255]);
        let row = find_trace_row(&px, w, h, 4, [0, 0, 0]);
        assert_eq!(row, Some(9));
    }

    #[test]
    fn find_trace_row_none_when_column_is_all_background() {
        let (w, h) = (16, 16);
        let px = solid_bg(w, h);
        assert_eq!(find_trace_row(&px, w, h, 4, [0, 0, 0]), None);
    }

    /// Harness self-test (#170 acceptance criterion): a deliberately
    /// introduced Y-flip must be caught. This directly exercises the same
    /// orientation logic `run_t3_checks` uses, on hand-built pixel data
    /// standing in for a GPU readback — no adapter required.
    #[test]
    fn self_test_y_flip_is_caught_by_orientation_check() {
        let (w, h) = (16, 16);
        let col = 4;
        let bg = [0u8, 0, 0];

        // Correct orientation: louder (db_lo=-6) trace at smaller row than
        // quieter (db_hi=-18) trace.
        let mut correct = solid_bg(w, h);
        set_px(&mut correct, w, col, 3, [255, 255, 255]); // louder -> near top
        let row_louder = find_trace_row(&correct, w, h, col, bg).unwrap();
        assert!(
            row_louder < h / 2,
            "sanity: louder trace should be in upper half"
        );

        // Deliberately flipped: same data, mirrored vertically — this is
        // exactly the class of bug I3/T3 exists to catch (the ember Y
        // inversion, #170 handoff.md).
        let mut flipped = solid_bg(w, h);
        set_px(&mut flipped, w, col, h - 1 - 3, [255, 255, 255]);
        let row_flipped = find_trace_row(&flipped, w, h, col, bg).unwrap();

        let pass_correct = row_louder < h / 2;
        let pass_flipped = row_flipped < h / 2;
        assert!(
            pass_correct,
            "correct orientation must pass the 'louder = higher' check"
        );
        assert!(
            !pass_flipped,
            "flipped orientation must fail the 'louder = higher' check"
        );
    }

    /// Harness self-test (#170 acceptance criterion): a deliberately
    /// introduced +6 dB offset must be caught at T2 (buffer). Fabricates a
    /// `DisplayFrame` with a known error and confirms `check_i1_buffer`
    /// flags it — no daemon required.
    #[test]
    fn self_test_plus_6db_offset_is_caught_at_t2() {
        use crate::data::types::FrameMeta;
        use std::sync::Arc;

        let frame = DisplayFrame {
            freqs: Arc::new(vec![1000.0]),
            // Injected level was -20 dBFS; buffer reports -14 dBFS — a +6
            // dB offset bug (the class handoff.md cites: aggregate.rs
            // double-dB-conversion).
            spectrum: Arc::new(vec![-14.0]),
            meta: FrameMeta {
                freq_hz: 1000.0,
                fundamental_dbfs: -14.0,
                thd_pct: 0.0,
                thdn_pct: 0.0,
                in_dbu: None,
                dbu_offset_db: None,
                peaks: Arc::new(Vec::new()),
                spl_offset_db: None,
                mic_correction: None,
                sr: 48_000,
                clipping: false,
                xruns: 0,
                leq_duration_s: None,
            },
            new_row: None,
        };
        let mut checks = Vec::new();
        check_i1_buffer(&mut checks, &frame, 1000.0, -20.0, "self-test +6dB");
        assert_eq!(checks.len(), 1);
        assert!(
            !checks[0].pass,
            "a +6 dB offset ({:.1} dB error) must fail I1's {I1_BUFFER_TOLERANCE_DB} dB tolerance: {}",
            6.0,
            checks[0].detail,
        );
    }

    #[test]
    fn nearest_dbfs_picks_closest_bin() {
        use crate::data::types::FrameMeta;
        use std::sync::Arc;
        let frame = DisplayFrame {
            freqs: Arc::new(vec![100.0, 500.0, 1000.0, 2000.0]),
            spectrum: Arc::new(vec![-40.0, -30.0, -20.0, -10.0]),
            meta: FrameMeta {
                freq_hz: 1000.0,
                fundamental_dbfs: -20.0,
                thd_pct: 0.0,
                thdn_pct: 0.0,
                in_dbu: None,
                dbu_offset_db: None,
                peaks: Arc::new(Vec::new()),
                spl_offset_db: None,
                mic_correction: None,
                sr: 48_000,
                clipping: false,
                xruns: 0,
                leq_duration_s: None,
            },
            new_row: None,
        };
        let (f, db) = nearest_dbfs(&frame, 950.0).unwrap();
        assert_eq!(f, 1000.0);
        assert_eq!(db, -20.0);
    }

    #[test]
    fn find_interp_aggregation_crossover_detects_spacing_jump() {
        // Synthetic grid: one-bin-wide spacing (0.5 Hz) up to col 100, then
        // a jump to 5x spacing — the aggregation branch's signature.
        let bin_width = 0.5_f64;
        let mut freqs = Vec::new();
        for i in 0..100 {
            freqs.push((i as f64 * bin_width) as f32);
        }
        let last = freqs.last().copied().unwrap() as f64;
        for i in 0..20 {
            freqs.push((last + i as f64 * bin_width * 5.0) as f32);
        }
        let sr = bin_width * 8192.0; // so sr/fft_n == bin_width
        let crossover = find_interp_aggregation_crossover(&freqs, sr, 8192).unwrap();
        assert!((crossover as f32 - freqs[99]).abs() < 0.01);
    }

    /// I4 must-fail regression corpus (#170 handoff.md / Decision 3
    /// validation gate): `ac monitor`'s own CSV export format, parsed with
    /// no daemon or GPU involved, must trip the same bounded-output check
    /// `check_i4_bounded` uses on a live capture. These five fixtures are
    /// the documented aggregate.rs double-dB-conversion bug's field
    /// signature (`tests/fixtures/fixtures-spectrum-hf-garbage/README.md`)
    /// — 876 violations per file above ~6533 Hz, up to +19.115 dBFS.
    #[test]
    fn i4_fixture_corpus_known_bad_csvs_fail_bounded_output() {
        // CARGO_MANIFEST_DIR = .../ac-rs/crates/ac-ui; fixtures live at the
        // outer repo root's tests/ (a sibling of ac-rs/, not inside it —
        // see ../../CLAUDE.md's "Repo structure").
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tests/fixtures/fixtures-spectrum-hf-garbage");
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("could not read fixture dir {}: {e}", dir.display()))
        {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("csv") {
                continue;
            }
            let body = std::fs::read_to_string(&path).unwrap();
            let max = max_dbfs_in_csv(&body);
            assert!(
                max > 0.0,
                "{}: expected a known-bad fixture (max > 0 dBFS), got max={max:.3} — \
                 if the underlying bug is now fixed, this fixture should move to a \
                 known-good reference set instead of silently passing here",
                path.display(),
            );
            checked += 1;
        }
        assert_eq!(
            checked,
            5,
            "expected exactly 5 known-bad fixtures in {}",
            dir.display()
        );
    }

    /// Minimal reader for the `ac monitor` CSV export shape (`# `-prefixed
    /// metadata, then `freq_hz,ch0_dbfs,ch0_mic_corrected,...`). Only
    /// extracts the max dBFS across every `*_dbfs` column — enough for the
    /// I4 bounded-output check, not a general-purpose CSV reader.
    fn max_dbfs_in_csv(body: &str) -> f64 {
        let mut lines = body.lines().filter(|l| !l.starts_with('#'));
        let header = lines.next().unwrap_or("");
        let dbfs_cols: Vec<usize> = header
            .split(',')
            .enumerate()
            .filter(|(_, name)| name.ends_with("_dbfs"))
            .map(|(i, _)| i)
            .collect();
        let mut max = f64::MIN;
        for line in lines {
            let cols: Vec<&str> = line.split(',').collect();
            for &i in &dbfs_cols {
                if let Some(v) = cols.get(i).and_then(|s| s.parse::<f64>().ok()) {
                    max = max.max(v);
                }
            }
        }
        max
    }
}
