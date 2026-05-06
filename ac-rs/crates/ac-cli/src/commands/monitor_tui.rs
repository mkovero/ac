//! Headless TUI fallback for `ac monitor` — runs when ac-ui exits 71
//! (no GPU adapter, broken driver, headless container) or isn't on PATH.
//!
//! Renders a `htop`-style refreshing display per monitored channel:
//! peak dBFS, peak frequency, broadband floor, weighting / averaging
//! state. ANSI cursor-home redraw at the daemon's monitor interval;
//! Ctrl+C sends `stop` over CTRL and exits cleanly.
//!
//! Pure read-only display — no keybindings, no zoom. The intent is to
//! give a developer SSH'd into the bench host *something* useful when
//! the GPU UI can't paint.

use std::io::{self, Write};
use std::time::Duration;

use anyhow::Result;
use crossterm::{cursor, event, execute, terminal};
use serde_json::Value;

use crate::client::AcClient;

/// Snapshot of a single channel as derived from the most recent
/// `visualize/spectrum` frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelStats {
    pub channel: u32,
    pub peak_dbfs: f32,
    pub peak_freq_hz: f32,
    pub floor_db: f32,
}

/// Parse one daemon `data` frame into per-channel stats. Returns `None`
/// for non-spectrum frames or malformed payloads. Pure — easy to unit-
/// test without spinning up ZMQ or a daemon.
pub fn stats_from_frame(value: &Value) -> Option<ChannelStats> {
    if value.get("type").and_then(|v| v.as_str()) != Some("visualize/spectrum") {
        return None;
    }
    let channel = value.get("channel")?.as_u64()? as u32;
    let spec_arr = value.get("spectrum")?.as_array()?;
    let freqs_arr = value.get("freqs")?.as_array()?;
    if spec_arr.is_empty() || freqs_arr.len() != spec_arr.len() {
        return None;
    }

    // Floor: minimum finite dB across the spectrum. Filters NaN/-inf
    // bins so an empty/mute channel doesn't peg the readout to -inf.
    let floor_db = spec_arr
        .iter()
        .filter_map(|v| v.as_f64())
        .map(|x| x as f32)
        .filter(|x| x.is_finite())
        .fold(f32::INFINITY, f32::min);
    let floor_db = if floor_db.is_finite() { floor_db } else { -200.0 };

    // Peak: prefer the daemon's THD-aware fundamental when present;
    // otherwise scan the spectrum. The fundamental is meaningful only
    // when `freq_hz` is also reported (the analysis succeeded).
    let fundamental_dbfs = value.get("fundamental_dbfs").and_then(|v| v.as_f64());
    let fundamental_freq = value.get("freq_hz").and_then(|v| v.as_f64());

    let (peak_dbfs, peak_freq_hz) = if let (Some(d), Some(f)) = (fundamental_dbfs, fundamental_freq)
    {
        (d as f32, f as f32)
    } else {
        let mut best = (f32::NEG_INFINITY, 0.0_f32);
        for (db_v, freq_v) in spec_arr.iter().zip(freqs_arr.iter()) {
            let (Some(db), Some(freq)) = (db_v.as_f64(), freq_v.as_f64()) else {
                continue;
            };
            let db = db as f32;
            if db.is_finite() && db > best.0 {
                best = (db, freq as f32);
            }
        }
        best
    };

    Some(ChannelStats {
        channel,
        peak_dbfs,
        peak_freq_hz,
        floor_db,
    })
}

/// Pretty-print one channel row. 80-col friendly, fixed widths so
/// channels stack vertically without jitter.
fn format_channel_row(s: &ChannelStats) -> String {
    let peak = if s.peak_dbfs.is_finite() {
        format!("{:>6.1} dBFS", s.peak_dbfs)
    } else {
        "    -- dBFS".to_string()
    };
    let freq = if s.peak_freq_hz > 0.0 {
        format!("{:>7.0} Hz", s.peak_freq_hz)
    } else {
        "      -- Hz".to_string()
    };
    let floor = if s.floor_db.is_finite() {
        format!("{:>5.0} dB", s.floor_db)
    } else {
        "   -- dB".to_string()
    };
    format!(
        "CH{:<2}   peak {} @ {}   floor {}",
        s.channel, peak, freq, floor,
    )
}

/// Build the full multi-line snapshot. Pure — no I/O. The caller wraps
/// each redraw with an ANSI cursor-home so the strings overwrite
/// in-place.
pub fn render_snapshot(
    title: &str,
    channels: &[ChannelStats],
    fft_n: u32,
    interval_ms: u32,
    xruns: u64,
) -> String {
    let bar = "─".repeat(69);
    let mut out = String::new();
    out.push_str(title);
    out.push('\n');
    out.push_str(&bar);
    out.push('\n');
    if channels.is_empty() {
        out.push_str("waiting for first frame…\n");
    } else {
        for s in channels {
            out.push_str(&format_channel_row(s));
            out.push('\n');
        }
    }
    out.push_str(&bar);
    out.push('\n');
    out.push_str(&format!(
        "fft N={fft_n}   interval={interval_ms} ms   xruns={xruns}\n",
    ));
    out
}

/// Top-level TUI loop. Returns on Ctrl+C, EOF on the daemon, or any I/O
/// error from the terminal. Always sends `stop` over CTRL on exit so a
/// monitor session doesn't leak past process death.
pub fn run(cfg: &ac_core::config::Config, channels: &[u32]) -> Result<()> {
    let host = cfg.server_host.as_deref().unwrap_or("127.0.0.1");
    let mut client = AcClient::new(host, 5556, 5557)?;

    // Default monitor params — match ac-ui's defaults so behaviour is
    // consistent regardless of which client started the session.
    let interval_ms: u32 = 100;
    let fft_n: u32 = 8192;

    let cmd = serde_json::json!({
        "cmd":      "monitor_spectrum",
        "interval": interval_ms as f64 / 1000.0,
        "fft_n":    fft_n,
        "channels": channels,
    });
    let ack = client.send_cmd(&cmd, None);
    super::check_ack(ack, "monitor_spectrum");

    let mut stdout = io::stdout();
    terminal::enable_raw_mode().ok();
    execute!(stdout, terminal::Clear(terminal::ClearType::All))?;

    let title = format!(
        "ac monitor — {} channel{} — Ctrl+C to exit",
        channels.len(),
        if channels.len() == 1 { "" } else { "s" },
    );

    let n = channels.len();
    let mut latest: Vec<Option<ChannelStats>> = vec![None; n];
    let mut xruns_max: u64 = 0;

    loop {
        // Drain any pending key events without blocking. Ctrl+C / q /
        // Esc all exit; the rest are ignored.
        if event::poll(Duration::from_millis(0))? {
            if let Ok(event::Event::Key(k)) = event::read() {
                let exit_now = matches!(
                    (k.code, k.modifiers),
                    (event::KeyCode::Char('c'), event::KeyModifiers::CONTROL)
                ) || matches!(k.code, event::KeyCode::Char('q') | event::KeyCode::Esc);
                if exit_now {
                    break;
                }
            }
        }

        if let Some((topic, value)) = client.recv_data(interval_ms as i64) {
            if topic == "data" {
                if let Some(stats) = stats_from_frame(&value) {
                    if let Some(slot) = channels.iter().position(|c| *c == stats.channel) {
                        latest[slot] = Some(stats);
                    }
                }
                if let Some(x) = value.get("xruns").and_then(|v| v.as_u64()) {
                    xruns_max = xruns_max.max(x);
                }
            }
        }

        let snap: Vec<ChannelStats> = latest.iter().filter_map(|s| *s).collect();
        let body = render_snapshot(&title, &snap, fft_n, interval_ms, xruns_max);
        execute!(stdout, cursor::MoveTo(0, 0))?;
        // Clear from cursor to end of screen so a shrinking row count
        // (channel drops out) doesn't leave stale lines on screen.
        execute!(stdout, terminal::Clear(terminal::ClearType::FromCursorDown))?;
        stdout.write_all(body.as_bytes())?;
        stdout.flush()?;
    }

    terminal::disable_raw_mode().ok();
    execute!(stdout, cursor::Show)?;
    println!();

    let _ = client.send_cmd(&serde_json::json!({"cmd": "stop"}), Some(500));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn frame(channel: u32, fundamental: Option<(f64, f64)>, freqs: &[f64], spec: &[f64]) -> Value {
        let mut v = json!({
            "type": "visualize/spectrum",
            "channel": channel,
            "freqs": freqs,
            "spectrum": spec,
        });
        if let Some((freq, db)) = fundamental {
            v["freq_hz"] = json!(freq);
            v["fundamental_dbfs"] = json!(db);
        }
        v
    }

    /// When the daemon reports a THD fundamental, peak should mirror it
    /// rather than re-scanning the spectrum (the fundamental is the
    /// authoritative answer once analysis succeeds).
    #[test]
    fn stats_use_fundamental_when_present() {
        let f = frame(
            2,
            Some((1000.0, -3.0)),
            &[100.0, 1000.0, 10000.0],
            &[-80.0, -3.0, -90.0],
        );
        let s = stats_from_frame(&f).expect("parse");
        assert_eq!(s.channel, 2);
        assert!((s.peak_dbfs + 3.0).abs() < 1e-3);
        assert!((s.peak_freq_hz - 1000.0).abs() < 1e-3);
        assert!((s.floor_db + 90.0).abs() < 1e-3);
    }

    /// No fundamental in the frame (cal-only / pre-THD path) — the
    /// peak falls back to a max-scan over the spectrum array.
    #[test]
    fn stats_scan_spectrum_without_fundamental() {
        let f = frame(
            0,
            None,
            &[100.0, 500.0, 2000.0],
            &[-50.0, -10.0, -70.0],
        );
        let s = stats_from_frame(&f).expect("parse");
        assert_eq!(s.channel, 0);
        assert!((s.peak_dbfs + 10.0).abs() < 1e-3);
        assert!((s.peak_freq_hz - 500.0).abs() < 1e-3);
    }

    /// Non-spectrum frames (sweep, transfer, error topics) must not be
    /// confused for monitor data.
    #[test]
    fn unrelated_frames_yield_none() {
        let f = json!({"type": "measurement/frequency_response/point", "channel": 0});
        assert!(stats_from_frame(&f).is_none());
    }

    /// NaN bins (e.g. clipped FFT bin, division-by-zero) must not bleed
    /// into the floor — the readout should still show a finite number.
    #[test]
    fn nan_bins_dont_corrupt_floor() {
        let nan = f64::NAN;
        let f = frame(
            0,
            Some((1000.0, -6.0)),
            &[100.0, 1000.0, 10000.0],
            &[nan, -6.0, -85.0],
        );
        let s = stats_from_frame(&f).expect("parse");
        assert!(s.floor_db.is_finite());
        assert!((s.floor_db + 85.0).abs() < 1e-3);
    }

    /// Render layout invariants: title + dashed bars + xrun footer must
    /// all be present; the channel row uses the formatted stats.
    #[test]
    fn render_snapshot_includes_required_lines() {
        let stats = vec![ChannelStats {
            channel: 0,
            peak_dbfs: -3.0,
            peak_freq_hz: 1000.0,
            floor_db: -85.0,
        }];
        let out = render_snapshot("ac monitor — 1 channel", &stats, 8192, 100, 0);
        assert!(out.contains("ac monitor — 1 channel"));
        assert!(out.contains("CH0"));
        assert!(out.contains("-3.0 dBFS"));
        assert!(out.contains("1000 Hz"));
        assert!(out.contains("-85 dB"));
        assert!(out.contains("fft N=8192"));
        assert!(out.contains("interval=100 ms"));
        assert!(out.contains("xruns=0"));
    }

    /// Empty channel list (no frame yet received) must render a
    /// placeholder rather than a bare bar-bar-footer.
    #[test]
    fn render_snapshot_handles_empty_channels() {
        let out = render_snapshot("title", &[], 8192, 100, 0);
        assert!(out.contains("waiting for first frame"));
    }
}
