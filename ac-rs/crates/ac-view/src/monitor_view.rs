//! The live input monitor window: what `ac monitor` opens. It runs the
//! daemon's `monitor_spectrum` on every requested input — no reference
//! leg, no output path — and draws one trace, one input meter and one
//! readout line per channel from `ac_scene::MonitorScene`.
//!
//! Before this window, `ac monitor` opened the transfer session's spectrum
//! view: a two-channel H1 session whose Welch spectrum refreshed about
//! twice a second, froze whenever the reference was silent, and showed
//! only the first requested channel.

use std::time::Duration;

use ac_core::wire::{check_wire_version, SpectrumFrame};
use ac_scene::{MonitorScene, MonitorState};
use anyhow::{bail, Result};
use egui::{Align2, Stroke};
use serde_json::json;

use crate::geometry::Viewport;
use crate::range::{DbRange, FreqRange};
use crate::view::paint::{draw_freq_labels, draw_pane_grid, draw_trace, text};
use crate::view::palette::{live_color, COLOR_LABEL, COLOR_SIGNAL, COLOR_STRUCTURAL, COLOR_VALUE};
use crate::zmq_client::{Client, Endpoint, Recv};

/// `monitor_spectrum` tick, seconds: 20 updates a second, the cadence
/// `transfer_stream` publishes at.
pub const MONITOR_INTERVAL_S: f64 = 0.05;

/// FFT length `monitor_spectrum` runs; the TUI's value.
pub const MONITOR_FFT_N: u32 = 8192;

/// Spectrum columns per frame: about one per pixel of a wide window. The
/// daemon's default 4096 made eight channels at 20 Hz ~22 MB/s of JSON.
pub const MONITOR_COLUMNS: u32 = 1024;

pub struct MonitorView {
    client: Client,
    channels: Vec<u32>,
    state: MonitorState,
    scene: Option<MonitorScene>,
    /// Channel index (position in `channels`) shown alone, or all.
    solo: Option<usize>,
    /// The daemon's own words for why the stream stopped or was refused.
    fault: Option<String>,
    malformed: u64,
    /// Spectrum frames accepted, all channels.
    frames: u64,
    stopped: bool,
}

impl MonitorView {
    /// Connect and start `monitor_spectrum` on `channels`. The analysis
    /// mode is set to `fft` first: it is daemon-global and sticky, and this
    /// window draws spectrum frames only.
    pub fn launch(endpoint: Endpoint, channels: &[u32]) -> Result<MonitorView> {
        if channels.is_empty() {
            bail!("no input channel to monitor");
        }
        let client = Client::connect(&endpoint)?;
        let mode = client.call(&json!({"cmd": "set_analysis_mode", "mode": "fft"}))?;
        if mode["ok"] != json!(true) {
            bail!(
                "set_analysis_mode fft refused: {}",
                mode["error"].as_str().unwrap_or("no reason given")
            );
        }
        client.drain_pending();
        let ack = client.call(&json!({
            "cmd": "monitor_spectrum",
            "interval": MONITOR_INTERVAL_S,
            "fft_n": MONITOR_FFT_N,
            "columns": MONITOR_COLUMNS,
            // This window draws no scope; its frames were another ~6 MB/s.
            "scope": false,
            "channels": channels,
        }))?;
        if ack["ok"] != json!(true) {
            bail!(
                "monitor_spectrum refused: {}",
                ack["error"].as_str().unwrap_or("no reason given")
            );
        }
        Ok(MonitorView {
            client,
            channels: channels.to_vec(),
            state: MonitorState::new(channels),
            scene: None,
            solo: None,
            fault: None,
            malformed: 0,
            frames: 0,
            stopped: false,
        })
    }

    pub fn title(&self) -> String {
        let list: Vec<String> = self.channels.iter().map(u32::to_string).collect();
        format!("ac monitor \u{2014} in {}", list.join(","))
    }

    /// Take every queued frame; the newest per channel wins.
    fn drain(&mut self, now_s: f64) {
        loop {
            match self.client.recv_frame(Duration::ZERO) {
                Recv::Empty => return,
                Recv::Malformed(why) => {
                    self.malformed += 1;
                    if self.malformed <= 5 {
                        eprintln!("ac-view: discarding malformed DATA frame ({why})");
                    }
                }
                Recv::Frame(topic, v) => {
                    if topic == "error" && v["cmd"] == "monitor_spectrum" {
                        self.fault = Some(
                            v["message"]
                                .as_str()
                                .unwrap_or("monitor stopped, no reason given")
                                .to_string(),
                        );
                        continue;
                    }
                    if topic != "data" || v["type"] != "visualize/spectrum" {
                        continue;
                    }
                    if let Err(e) = check_wire_version(&v) {
                        self.fault = Some(format!("daemon version mismatch: {e}"));
                        continue;
                    }
                    match serde_json::from_value::<SpectrumFrame>(v) {
                        Ok(f) => {
                            if self.state.push(f, now_s) {
                                self.frames += 1;
                            }
                        }
                        Err(e) => {
                            self.malformed += 1;
                            if self.malformed <= 5 {
                                eprintln!("ac-view: unreadable spectrum frame: {e}");
                            }
                        }
                    }
                }
            }
        }
    }

    /// The scene the last pass painted.
    pub fn scene(&self) -> Option<&MonitorScene> {
        self.scene.as_ref()
    }

    /// Spectrum frames accepted so far, all channels together.
    pub fn frames_received(&self) -> u64 {
        self.frames
    }

    /// The daemon's reason the stream stopped, if it sent one.
    pub fn fault(&self) -> Option<&str> {
        self.fault.as_deref()
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        const DIGITS: [egui::Key; 9] = [
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
            egui::Key::Num6,
            egui::Key::Num7,
            egui::Key::Num8,
            egui::Key::Num9,
        ];
        ctx.input(|i| {
            if i.key_pressed(egui::Key::Escape) || i.key_pressed(egui::Key::Q) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            if i.key_pressed(egui::Key::Num0) || i.key_pressed(egui::Key::A) {
                self.solo = None;
            }
            for (idx, key) in DIGITS.iter().enumerate() {
                if i.key_pressed(*key) && idx < self.channels.len() {
                    self.solo = Some(idx);
                }
            }
        });
    }

    fn stop(&mut self) {
        if !self.stopped {
            let _ = self.client.call(&json!({"cmd": "stop"}));
            self.stopped = true;
        }
    }
}

impl Drop for MonitorView {
    fn drop(&mut self) {
        self.stop();
    }
}

impl eframe::App for MonitorView {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_keys(&ctx);
        let now_s = ctx.input(|i| i.time);
        self.drain(now_s);
        let freq = FreqRange::default();
        let db = DbRange::default();
        self.scene = Some(
            self.state
                .scene(now_s, (freq.min(), freq.max()), (db.min(), db.max())),
        );

        ui.label("1\u{2013}9 solo a channel   0/A all   Esc/Q close");
        let rect = ui.available_rect_before_wrap();
        let painter = ui.painter();
        let meters_w = 10.0 * self.channels.len() as f32 + 8.0;
        let plot =
            egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x - meters_w, rect.max.y));
        let vp = Viewport::from(plot);
        let Some(scene) = &self.scene else {
            return;
        };
        draw_pane_grid(painter, &scene.db_axis, vp);
        draw_freq_labels(painter, &scene.freq_axis, vp);

        for (idx, ch) in scene.channels.iter().enumerate() {
            if self.solo.is_some_and(|s| s != idx) {
                continue;
            }
            if let Some(trace) = &ch.trace {
                draw_trace(painter, trace, vp, Stroke::new(1.5, live_color(idx)), false);
            }
        }

        // Readout lines, top-left of the plot, each in its trace's colour.
        let mut y = plot.min.y + 2.0;
        for (idx, ch) in scene.channels.iter().enumerate() {
            let color = if self.solo.is_some_and(|s| s != idx) {
                COLOR_STRUCTURAL
            } else {
                live_color(idx)
            };
            text(
                painter,
                egui::pos2(plot.min.x + 40.0, y),
                Align2::LEFT_TOP,
                &ch.readout,
                color,
            );
            y += 14.0;
        }

        // Meters, one per channel, left to right in request order.
        for (idx, ch) in scene.channels.iter().enumerate() {
            let bar_w = 6.0;
            let x = plot.max.x + 6.0 + idx as f32 * (bar_w + 4.0);
            let top = rect.min.y + 14.0;
            let h = (rect.max.y - top) * ch.meter.height as f32;
            let hold_y = rect.max.y - (rect.max.y - top) * ch.meter.hold as f32;
            let color = if ch.meter.clip_latch {
                COLOR_SIGNAL
            } else {
                live_color(idx)
            };
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(x, rect.max.y - h),
                    egui::pos2(x + bar_w, rect.max.y),
                ),
                0.0,
                color,
            );
            painter.line_segment(
                [egui::pos2(x, hold_y), egui::pos2(x + bar_w, hold_y)],
                Stroke::new(1.0, COLOR_VALUE),
            );
            text(
                painter,
                egui::pos2(x, rect.min.y),
                Align2::LEFT_TOP,
                &ch.label,
                COLOR_LABEL,
            );
        }

        if let Some(fault) = &self.fault {
            text(
                painter,
                plot.center(),
                Align2::CENTER_CENTER,
                fault,
                COLOR_SIGNAL,
            );
        }
        // Frames arrive at 20 Hz; a 16 ms repaint shows each within one
        // display frame without spinning at vsync.
        ctx.request_repaint_after(Duration::from_millis(16));
    }
}
