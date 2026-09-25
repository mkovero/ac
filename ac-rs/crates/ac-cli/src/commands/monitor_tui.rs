//! Terminal display for `ac monitor`.
//!
//! Renders a `htop`-style refreshing display per monitored channel:
//! peak dBFS, peak frequency, broadband floor, weighting / averaging
//! state. ANSI cursor-home redraw at the daemon's monitor interval;
//! Ctrl+C sends `stop` over CTRL and exits cleanly.
//!
//! Pure read-only display — no keybindings, no zoom.

use std::io::{self, Write};
use std::time::Duration;

use ac_core::wire::{check_wire_version, SpectrumFrame, WireVersionError};
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

/// Wire `spectrum` bins are linear amplitude, not dB — see
/// `ac_core::visualize::spectrum::spectrum_only`, which normalizes to a
/// linear magnitude and never takes a log. The linear→dB conversion used
/// to happen in `ac-ui/src/data/receiver.rs` before that crate was
/// detached; it hasn't been re-homed anywhere since, so
/// every direct consumer of `spectrum` must do it locally. Clamped at
/// 1e-12 so a silent/zero bin converts to a finite floor instead of
/// `-inf`, matching the floor convention in
/// `ac_core::visualize::aggregate`'s `to_dbfs` test helper.
fn linear_to_dbfs(amp: f64) -> f32 {
    (20.0 * amp.max(1e-12).log10()) as f32
}

/// Parse one daemon `data` frame into per-channel stats.
///
/// `Err` when the frame's `wire_version` is one this build does not read
/// (#112) — checked on the raw value first, for every frame type, so a
/// refused daemon's frames contribute nothing (not even `xruns`) and a frame
/// too far off the schema to parse still reads as a version mismatch.
/// `Ok(None)` for non-spectrum frames and malformed payloads. Pure — easy to
/// unit-test without spinning up ZMQ or a daemon.
pub fn stats_from_frame(value: &Value) -> Result<Option<ChannelStats>, WireVersionError> {
    check_wire_version(value)?;
    if value.get("type").and_then(|v| v.as_str()) != Some("visualize/spectrum") {
        return Ok(None);
    }
    // The shared type the daemon serialises (`ac_core::wire`), so a field
    // renamed on either side is a build error rather than a blank readout.
    Ok(serde_json::from_value::<SpectrumFrame>(value.clone())
        .ok()
        .and_then(|frame| stats_from_spectrum(&frame)))
}

fn stats_from_spectrum(frame: &SpectrumFrame) -> Option<ChannelStats> {
    let spec = &frame.spectrum;
    let freqs = &frame.freqs;
    if spec.is_empty() || freqs.len() != spec.len() {
        return None;
    }

    // Floor: minimum finite dB across the spectrum, after converting each
    // linear-amplitude bin. Filters NaN bins first so an empty/mute
    // channel doesn't peg the readout via a spuriously "quiet" NaN.
    let floor_db = spec
        .iter()
        .copied()
        .filter(|x| x.is_finite())
        .map(linear_to_dbfs)
        .fold(f32::INFINITY, f32::min);
    let floor_db = if floor_db.is_finite() {
        floor_db
    } else {
        -200.0
    };

    // Peak: prefer the daemon's THD-aware fundamental when present
    // (already dB, sent as-is); otherwise scan the spectrum, converting
    // each linear-amplitude bin before comparing. The fundamental is
    // meaningful only when `freq_hz` is also reported (the analysis
    // succeeded).
    let (peak_dbfs, peak_freq_hz) =
        if let (Some(d), Some(f)) = (frame.fundamental_dbfs, frame.freq_hz) {
            (d as f32, f as f32)
        } else {
            let mut best = (f32::NEG_INFINITY, 0.0_f32);
            for (&amp, &freq) in spec.iter().zip(freqs.iter()) {
                if !amp.is_finite() || !freq.is_finite() {
                    continue;
                }
                let db = linear_to_dbfs(amp);
                if db > best.0 {
                    best = (db, freq as f32);
                }
            }
            best
        };

    Some(ChannelStats {
        channel: frame.channel,
        peak_dbfs,
        peak_freq_hz,
        floor_db,
    })
}

/// The daemon's frames are being refused for their wire version (#112).
#[derive(Debug, Clone)]
pub struct Refusal {
    /// `host:port` of the daemon's CTRL endpoint.
    pub endpoint: String,
    /// The most recent refused frame's version.
    pub error: WireVersionError,
    /// DATA frames refused since the state began. A rising count says the
    /// daemon is alive and only the version is wrong.
    pub refused: u64,
}

impl Refusal {
    /// `daemon sends wire v2, ac reads v1` — both versions side by side,
    /// which says which build is older without the text asserting a cause.
    fn versions(&self) -> String {
        format!(
            "daemon sends wire {}, ac reads {}",
            self.error.found_label(),
            WireVersionError::supported_label()
        )
    }
}

/// The two plain lines printed after the TUI exits in the refused state, so
/// the refusal survives in scrollback and in SSH session logs.
pub fn refusal_exit_lines(r: &Refusal) -> [String; 2] {
    [
        format!("ac monitor: version mismatch — {}", r.endpoint),
        format!(
            "ac monitor: {} — {} frames refused",
            r.versions(),
            r.refused
        ),
    ]
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
        format!("{:>7.1} dBFS", s.floor_db)
    } else {
        "     -- dBFS".to_string()
    };
    format!(
        "CH{:<2}   peak {} @ {}   floor {}",
        s.channel, peak, freq, floor,
    )
}

/// Build the full multi-line snapshot. Pure — no I/O. The caller wraps
/// each redraw with an ANSI cursor-home so the strings overwrite
/// in-place.
///
/// A `refusal` replaces the channel rows with the version-mismatch lines,
/// and the footer's `xruns` reads `--`: every value after the refusal would
/// come from a stream this client no longer reads. `fft N` and `interval`
/// stay — they are this client's own request parameters.
pub fn render_snapshot(
    title: &str,
    channels: &[ChannelStats],
    fft_n: u32,
    interval_ms: u32,
    xruns: u64,
    refusal: Option<&Refusal>,
) -> String {
    let mut out = String::new();
    out.push_str(title);
    out.push('\n');
    if let Some(r) = refusal {
        out.push_str(&format!("version mismatch — {}\n", r.endpoint));
        out.push_str(&format!(
            "{} — {} frames refused, not rendering\n",
            r.versions(),
            r.refused
        ));
        out.push('\n');
        out.push_str(&format!(
            "fft N={fft_n}   interval={interval_ms} ms   xruns=--\n",
        ));
        return out;
    }
    if channels.is_empty() {
        out.push_str("waiting for first frame…\n");
    } else {
        for s in channels {
            out.push_str(&format_channel_row(s));
            out.push('\n');
        }
    }
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

    // Default monitor params.
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
    let mut refusal: Option<Refusal> = None;

    loop {
        // Drain any pending key events without blocking. Ctrl+C / q /
        // Esc all exit; the rest are ignored.
        if event::poll(Duration::from_millis(0))? {
            if let Ok(event::Event::Key(k)) = event::read() {
                let exit_now =
                    matches!(
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
                match stats_from_frame(&value) {
                    Err(error) => {
                        // Rows held from before the refusal came from a
                        // stream this client no longer reads; drop them.
                        latest.iter_mut().for_each(|s| *s = None);
                        let refused = refusal.as_ref().map_or(0, |r| r.refused) + 1;
                        refusal = Some(Refusal {
                            endpoint: format!("{host}:5556"),
                            error,
                            refused,
                        });
                    }
                    Ok(stats) => {
                        // An accepted frame ends the state and resets the
                        // count: the daemon was restarted on a matching build.
                        refusal = None;
                        if let Some(stats) = stats {
                            if let Some(slot) = channels.iter().position(|c| *c == stats.channel) {
                                latest[slot] = Some(stats);
                            }
                        }
                        if let Some(x) = value.get("xruns").and_then(|v| v.as_u64()) {
                            xruns_max = xruns_max.max(x);
                        }
                    }
                }
            }
        }

        let snap: Vec<ChannelStats> = latest.iter().filter_map(|s| *s).collect();
        let body = render_snapshot(
            &title,
            &snap,
            fft_n,
            interval_ms,
            xruns_max,
            refusal.as_ref(),
        );
        execute!(stdout, cursor::MoveTo(0, 0))?;
        // Clear from cursor to end of screen so a shrinking row count
        // (channel drops out) doesn't leave stale lines on screen.
        execute!(stdout, terminal::Clear(terminal::ClearType::FromCursorDown))?;
        // Raw mode disables the terminal's automatic \n -> \r\n
        // translation, so a bare \n only moves the cursor down without
        // returning it to column 0 — each line staircases one column
        // further right than the last. Terminal is in raw mode here, so
        // supply the \r ourselves.
        stdout.write_all(body.replace('\n', "\r\n").as_bytes())?;
        stdout.flush()?;
    }

    terminal::disable_raw_mode().ok();
    execute!(stdout, cursor::Show)?;
    println!();
    if let Some(r) = &refusal {
        for line in refusal_exit_lines(r) {
            println!("{line}");
        }
    }

    let _ = client.send_cmd(&serde_json::json!({"cmd": "stop"}), Some(500));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Fixtures below are authored in dB for readability; `spec_db` is
    /// converted to the linear amplitude the wire actually carries (see
    /// `linear_to_dbfs`) before being embedded, so the round-trip through
    /// `stats_from_frame` recovers the same numbers the test asserts on.
    fn frame(
        channel: u32,
        fundamental: Option<(f64, f64)>,
        freqs: &[f64],
        spec_db: &[f64],
    ) -> Value {
        let spec_amp: Vec<f64> = spec_db.iter().map(|&db| 10f64.powf(db / 20.0)).collect();
        let mut v = json!({
            "type": "visualize/spectrum",
            "channel": channel,
            "freqs": freqs,
            "spectrum": spec_amp,
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
        let s = stats_from_frame(&f).unwrap().expect("parse");
        assert_eq!(s.channel, 2);
        assert!((s.peak_dbfs + 3.0).abs() < 1e-3);
        assert!((s.peak_freq_hz - 1000.0).abs() < 1e-3);
        assert!((s.floor_db + 90.0).abs() < 1e-3);
    }

    /// No fundamental in the frame (cal-only / pre-THD path) — the
    /// peak falls back to a max-scan over the spectrum array.
    #[test]
    fn stats_scan_spectrum_without_fundamental() {
        let f = frame(0, None, &[100.0, 500.0, 2000.0], &[-50.0, -10.0, -70.0]);
        let s = stats_from_frame(&f).unwrap().expect("parse");
        assert_eq!(s.channel, 0);
        assert!((s.peak_dbfs + 10.0).abs() < 1e-3);
        assert!((s.peak_freq_hz - 500.0).abs() < 1e-3);
    }

    /// Non-spectrum frames (sweep, transfer, error topics) must not be
    /// confused for monitor data.
    #[test]
    fn unrelated_frames_yield_none() {
        let f = json!({"type": "measurement/frequency_response/point", "channel": 0});
        assert!(stats_from_frame(&f).unwrap().is_none());
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
        let s = stats_from_frame(&f).unwrap().expect("parse");
        assert!(s.floor_db.is_finite());
        assert!((s.floor_db + 85.0).abs() < 1e-3);
    }

    /// Layout invariants shared by the populated and empty renders: no
    /// rule anywhere, the footer sits after exactly one blank line, and no
    /// two blank lines ever follow each other (#116 layout, #129).
    fn assert_rule_free_layout(out: &str) {
        assert!(!out.contains('─'), "rule in snapshot:\n{out}");
        let lines: Vec<&str> = out.lines().collect();
        let footer = lines
            .iter()
            .position(|l| l.starts_with("fft N="))
            .expect("footer");
        assert!(footer >= 2);
        assert!(lines[footer - 1].is_empty(), "no blank before footer");
        assert!(!lines[footer - 2].is_empty(), "two blanks before footer");
        assert!(lines
            .windows(2)
            .all(|w| !(w[0].is_empty() && w[1].is_empty())));
    }

    /// Render layout invariants: title, channel row and xrun footer must
    /// all be present; the channel row uses the formatted stats.
    #[test]
    fn render_snapshot_includes_required_lines() {
        let stats = vec![ChannelStats {
            channel: 0,
            peak_dbfs: -3.0,
            peak_freq_hz: 1000.0,
            floor_db: -85.0,
        }];
        let out = render_snapshot("ac monitor — 1 channel", &stats, 8192, 100, 0, None);
        assert!(out.contains("ac monitor — 1 channel"));
        assert!(out.contains("CH0"));
        assert!(out.contains("-3.0 dBFS"));
        assert!(out.contains("1000 Hz"));
        assert!(out.contains("-85.0 dBFS"));
        assert!(out.contains("fft N=8192"));
        assert!(out.contains("interval=100 ms"));
        assert!(out.contains("xruns=0"));
        assert_rule_free_layout(&out);
    }

    /// Empty channel list (no frame yet received) must render a
    /// placeholder between title and footer rather than nothing.
    #[test]
    fn render_snapshot_handles_empty_channels() {
        let out = render_snapshot("title", &[], 8192, 100, 0, None);
        assert!(out.contains("waiting for first frame"));
        assert_rule_free_layout(&out);
    }

    // ---- #112: the shared wire type and the version refusal ----

    /// The daemon's side of the contract: a `SpectrumFrame` as the daemon
    /// serialises it (THD branch, every field set) reaches `stats_from_frame`
    /// through its `Value` entry point with the fields it reads intact.
    #[test]
    fn a_daemon_serialised_spectrum_frame_reads_back_intact() {
        let frame = SpectrumFrame {
            frame_type: "visualize/spectrum".into(),
            cmd: "monitor_spectrum".into(),
            wire_version: Some(ac_core::wire::WIRE_VERSION),
            channel: 3,
            n_channels: 4,
            sr: 48000,
            freqs: vec![100.0, 1000.0, 10000.0],
            spectrum: vec![1e-4, 0.1, 1e-5],
            dbu_offset_db: None,
            voltage_check: None,
            spl_offset_db: None,
            mic_correction: "none".into(),
            xruns: 7,
            backend: "fake".into(),
            freq_hz: Some(997.0),
            peaks: Some(vec![[997.0, -20.5]]),
            fundamental_dbfs: Some(-20.5),
            thd_pct: Some(0.01),
            thdn_pct: Some(0.02),
            in_dbu: Some(None),
            clipping: Some(false),
        };
        let v = serde_json::to_value(&frame).unwrap();
        let s = stats_from_frame(&v).unwrap().expect("parse");
        assert_eq!(s.channel, 3);
        assert_eq!(s.peak_dbfs, -20.5);
        assert_eq!(s.peak_freq_hz, 997.0);
        assert!((s.floor_db + 100.0).abs() < 1e-3);
    }

    #[test]
    fn a_frame_from_an_unread_wire_version_is_refused_not_parsed() {
        let mut f = frame(0, None, &[100.0], &[-10.0]);
        f["wire_version"] = json!(2);
        let err = stats_from_frame(&f).unwrap_err();
        assert_eq!(err.found_label(), "v2");
        // Any frame type: a refused daemon contributes nothing, not even
        // the `xruns` a loudness frame carries.
        let other = json!({"type": "measurement/loudness", "wire_version": 2, "xruns": 9});
        assert!(stats_from_frame(&other).is_err());
    }

    fn refusal(found: u64, refused: u64) -> Refusal {
        let mut f = frame(0, None, &[100.0], &[-10.0]);
        f["wire_version"] = json!(found);
        Refusal {
            endpoint: "192.168.9.27:5556".into(),
            error: stats_from_frame(&f).unwrap_err(),
            refused,
        }
    }

    #[test]
    fn a_refusal_replaces_the_channel_rows() {
        let stats = vec![ChannelStats {
            channel: 0,
            peak_dbfs: -3.0,
            peak_freq_hz: 1000.0,
            floor_db: -85.0,
        }];
        let r = refusal(2, 214);
        let out = render_snapshot(
            "ac monitor — 2 channels — Ctrl+C to exit",
            &stats,
            8192,
            100,
            0,
            Some(&r),
        );
        assert_eq!(
            out,
            "ac monitor — 2 channels — Ctrl+C to exit\n\
             version mismatch — 192.168.9.27:5556\n\
             daemon sends wire v2, ac reads v1 — 214 frames refused, not rendering\n\
             \n\
             fft N=8192   interval=100 ms   xruns=--\n"
        );
        assert!(!out.contains("CH"));
        assert_rule_free_layout(&out);
    }

    /// The UX worst case (range aside, which is one version today) fits in
    /// 80 columns with a six-digit count.
    #[test]
    fn the_refusal_counts_line_fits_80_columns() {
        let r = refusal(0, 184_211);
        let out = render_snapshot("t", &[], 8192, 100, 0, Some(&r));
        for line in out.lines() {
            assert!(line.chars().count() <= 80, "{line:?}");
        }
    }

    #[test]
    fn exit_lines_keep_the_refusal_in_scrollback() {
        assert_eq!(
            refusal_exit_lines(&refusal(2, 214)),
            [
                "ac monitor: version mismatch — 192.168.9.27:5556".to_string(),
                "ac monitor: daemon sends wire v2, ac reads v1 — 214 frames refused".to_string(),
            ]
        );
    }
}
