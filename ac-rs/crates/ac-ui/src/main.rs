mod app;
mod data;
mod render;
mod theme;
mod ui;

use std::path::PathBuf;
use std::time::Duration;

use app::{
    App, AppInit, SourceKind, CONTINUOUS_REPAINT_INTERVAL_DEFAULT, MAX_FPS_MAX, MAX_FPS_MIN,
};
use data::store::{
    ChannelStore, IrStore, LoudnessStore, ScopeStore, SweepStore, TransferStore, VirtualChannelStore,
};
use data::types::{SweepKind, ViewMode};

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse(std::env::args().skip(1))?;
    if args.help {
        print_help();
        return Ok(());
    }

    let (monitor_channels, n_channels) = if let Some(ref chs) = args.channels {
        (Some(chs.clone()), chs.len().max(1))
    } else if args.synthetic {
        (None, 2)
    } else {
        // Default to the configured input channel (mirrors what `ac monitor`
        // sends when invoked without an explicit channel spec). Without
        // this `ac-ui` standalone would probe the daemon and light up
        // every capture port — turning a casual launch into 8-channel ×
        // 4096-bin × 60 fps render work, which is gratuitous on stacks
        // where each present is expensive (#109).
        match ac_core::config::load(None) {
            Ok(cfg) => {
                let ch = cfg.input_channel;
                log::info!("defaulting to configured input_channel={ch} (override with --channels)");
                (Some(vec![ch]), 1)
            }
            Err(e) => {
                log::warn!("could not load config ({e}); defaulting to channel 0");
                (Some(vec![0]), 1)
            }
        }
    };
    let (inputs, store) = ChannelStore::new(n_channels);
    let transfer_store = TransferStore::new();
    let virtual_channels = VirtualChannelStore::new();
    let sweep_store = SweepStore::new();
    let loudness_store = LoudnessStore::new();
    let scope_store = ScopeStore::new();
    let ir_store = IrStore::new();

    let source_kind = if args.synthetic {
        SourceKind::Synthetic
    } else {
        SourceKind::Daemon
    };

    // Build with a unit user-event type so background producer threads can
    // wake the loop via `EventLoopProxy::send_event` when new frames arrive.
    let event_loop = winit::event_loop::EventLoop::<()>::with_user_event().build()?;
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
    let wake = event_loop.create_proxy();

    let init = AppInit {
        store,
        inputs,
        transfer_store,
        virtual_channels,
        sweep_store,
        loudness_store,
        scope_store,
        ir_store,
        source_kind,
        output_dir: args.output_dir.clone(),
        endpoint: args.connect.clone(),
        ctrl_endpoint: args.ctrl.clone(),
        synthetic_params: Some((n_channels, args.bins.max(16), args.rate.max(0.5))),
        benchmark_secs: args.benchmark,
        initial_view: args.view,
        initial_view_via_cli: args.view_set_via_cli,
        disable_persist: args.no_persist,
        initial_sweep_kind: args.mode,
        monitor_channels,
        present_mode: args.present_mode,
        continuous_interval: args.continuous_interval,
        wake: Some(wake),
    };

    let mut app = App::new(init);
    event_loop.run_app(&mut app)?;
    if let Some(report) = app.benchmark_report() {
        println!("{report}");
    }
    Ok(())
}

struct Args {
    help: bool,
    connect: String,
    ctrl: String,
    synthetic: bool,
    /// Explicit channel indices to monitor, e.g. `--channels 0,2,5` or
    /// `--channels 0-3`.  A bare number like `--channels 4` is treated as
    /// a count (channels 0..4) for backward compatibility.
    channels: Option<Vec<u32>>,
    bins: usize,
    rate: f32,
    output_dir: PathBuf,
    benchmark: Option<f64>,
    view: ViewMode,
    /// `true` when the user passed `--view`. Lets `App::new` decide
    /// whether to honour the persisted ui.json view_mode (no `--view`)
    /// or the CLI override (`--view ...`). `unified.md` Phase 6.
    view_set_via_cli: bool,
    /// `--no-persist` — skip both reading and writing
    /// `~/.config/ac/ui.json`. For benchmark / test runs that
    /// shouldn't pollute the on-disk state.
    no_persist: bool,
    mode: Option<SweepKind>,
    /// wgpu surface present mode. Default `auto-vsync` resolves to `Fifo`
    /// on desktop. `mailbox` is the workaround for NVIDIA + Vulkan
    /// `present()` busy-spin (#109/#110); `immediate` skips vsync entirely
    /// (tearing) for measuring the no-sync lower bound.
    present_mode: wgpu::PresentMode,
    /// Sleep budget between continuous-repaint frames. Default 30 fps
    /// matches the daemon auto-pick at typical fft_n; on stacks where
    /// `present()` is cheap (Wayland + radv, Apple Silicon) bump to 60
    /// for smoother motion. On stacks where `present()` is expensive
    /// (NVIDIA + Vulkan + X11) leave at 30 or drop further.
    continuous_interval: Duration,
}

fn parse_max_fps(s: &str) -> anyhow::Result<Duration> {
    let hz: u32 = s.parse().map_err(|e| anyhow::anyhow!(
        "--max-fps: expected integer hz in [{MAX_FPS_MIN}, {MAX_FPS_MAX}], got {s:?}: {e}",
    ))?;
    if !(MAX_FPS_MIN..=MAX_FPS_MAX).contains(&hz) {
        anyhow::bail!(
            "--max-fps: {hz} hz out of range [{MAX_FPS_MIN}, {MAX_FPS_MAX}]",
        );
    }
    // Round-down ms — `Duration::from_millis(1000 / hz)` for hz=30 → 33 ms,
    // matching the existing default. Avoids floating-point.
    Ok(Duration::from_millis((1000 / hz) as u64))
}

fn parse_view_mode(s: &str) -> anyhow::Result<ViewMode> {
    match s {
        "spectrum"                              => Ok(ViewMode::Spectrum),
        "waterfall"                             => Ok(ViewMode::Waterfall),
        "scope"                                 => Ok(ViewMode::Scope),
        "spectrum_ember"  | "spectrum-ember"    => Ok(ViewMode::SpectrumEmber),
        "goniometer"                            => Ok(ViewMode::Goniometer),
        "iotransfer" | "io_transfer" | "io-transfer" => Ok(ViewMode::IoTransfer),
        "bode_mag" | "bode-mag" | "bodemag" | "bode" => Ok(ViewMode::BodeMag),
        "coherence" | "coh" => Ok(ViewMode::Coherence),
        "bode_phase" | "bode-phase" | "bodephase" | "phase" => Ok(ViewMode::BodePhase),
        "group_delay" | "group-delay" | "groupdelay" | "gd" => Ok(ViewMode::GroupDelay),
        "nyquist" | "nyq" => Ok(ViewMode::Nyquist),
        "ir" | "impulse" | "impulse_response" => Ok(ViewMode::Ir),
        other => anyhow::bail!(
            "--view: expected spectrum|waterfall|scope|spectrum_ember|goniometer|iotransfer|bode_mag|coherence|bode_phase|group_delay|nyquist|ir, got {other}",
        ),
    }
}

fn parse_present_mode(s: &str) -> anyhow::Result<wgpu::PresentMode> {
    match s {
        "auto-vsync"    | "auto_vsync"    | "auto"      => Ok(wgpu::PresentMode::AutoVsync),
        "auto-no-vsync" | "auto_no_vsync" | "no-vsync"  => Ok(wgpu::PresentMode::AutoNoVsync),
        "fifo"                                          => Ok(wgpu::PresentMode::Fifo),
        "fifo-relaxed"  | "fifo_relaxed"                => Ok(wgpu::PresentMode::FifoRelaxed),
        "mailbox"                                       => Ok(wgpu::PresentMode::Mailbox),
        "immediate"                                     => Ok(wgpu::PresentMode::Immediate),
        other => anyhow::bail!(
            "--present-mode: expected auto-vsync|auto-no-vsync|fifo|fifo-relaxed|mailbox|immediate, got {other}",
        ),
    }
}

impl Args {
    fn parse(args: impl Iterator<Item = String>) -> anyhow::Result<Self> {
        // Env fallback for present mode — `AC_UI_PRESENT_MODE=mailbox`
        // lets users flip the workaround without retyping the flag every
        // launch. Explicit `--present-mode` always wins over the env.
        let env_present_mode = std::env::var("AC_UI_PRESENT_MODE")
            .ok()
            .map(|v| parse_present_mode(&v))
            .transpose()?
            .unwrap_or(wgpu::PresentMode::AutoVsync);
        let env_continuous_interval = std::env::var("AC_UI_MAX_FPS")
            .ok()
            .map(|v| parse_max_fps(&v))
            .transpose()?
            .unwrap_or(CONTINUOUS_REPAINT_INTERVAL_DEFAULT);
        let mut out = Args {
            help: false,
            connect: "tcp://127.0.0.1:5557".to_string(),
            ctrl: "tcp://127.0.0.1:5556".to_string(),
            synthetic: false,
            channels: None,
            bins: 1000,
            rate: 10.0,
            output_dir: default_output_dir(),
            benchmark: None,
            view: ViewMode::Spectrum,
            view_set_via_cli: false,
            no_persist: false,
            mode: None,
            present_mode: env_present_mode,
            continuous_interval: env_continuous_interval,
        };
        let mut it = args.peekable();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "-h" | "--help" => out.help = true,
                "--synthetic" => out.synthetic = true,
                "--connect" => {
                    out.connect = it
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--connect requires value"))?;
                }
                "--ctrl" => {
                    out.ctrl = it
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--ctrl requires value"))?;
                }
                "--channels" => {
                    let val = it.next()
                        .ok_or_else(|| anyhow::anyhow!("--channels requires value"))?;
                    out.channels = Some(parse_channel_spec(&val)?);
                }
                "--bins" => {
                    out.bins = it
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--bins requires value"))?
                        .parse()?;
                }
                "--rate" => {
                    out.rate = it
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--rate requires value"))?
                        .parse()?;
                }
                "--output-dir" => {
                    out.output_dir = PathBuf::from(
                        it.next()
                            .ok_or_else(|| anyhow::anyhow!("--output-dir requires value"))?,
                    );
                }
                "--benchmark" => {
                    out.benchmark = Some(
                        it.next()
                            .ok_or_else(|| anyhow::anyhow!("--benchmark requires value"))?
                            .parse()?,
                    );
                }
                "--no-persist" => {
                    out.no_persist = true;
                }
                "--view" => {
                    let v = it
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--view requires value"))?;
                    out.view = parse_view_mode(&v)?;
                    out.view_set_via_cli = true;
                }
                "--mode" => {
                    let v = it
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--mode requires value"))?;
                    out.mode = Some(match v.as_str() {
                        "sweep_frequency" => SweepKind::Frequency,
                        "sweep_level" => SweepKind::Level,
                        other => anyhow::bail!("--mode: expected sweep_frequency|sweep_level, got {other}"),
                    });
                }
                "--present-mode" => {
                    let v = it
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--present-mode requires value"))?;
                    out.present_mode = parse_present_mode(&v)?;
                }
                "--max-fps" => {
                    let v = it
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--max-fps requires value"))?;
                    out.continuous_interval = parse_max_fps(&v)?;
                }
                other => anyhow::bail!("unknown argument: {other}"),
            }
        }
        Ok(out)
    }
}

fn parse_channel_spec(s: &str) -> anyhow::Result<Vec<u32>> {
    if !s.contains(',') && !s.contains('-') {
        let n: usize = s.parse()?;
        return Ok((0..n as u32).collect());
    }
    let mut channels = std::collections::BTreeSet::new();
    for part in s.split(',') {
        let part = part.trim();
        if let Some((lo, hi)) = part.split_once('-') {
            let lo: u32 = lo.parse()?;
            let hi: u32 = hi.parse()?;
            for ch in lo..=hi {
                channels.insert(ch);
            }
        } else {
            channels.insert(part.parse()?);
        }
    }
    Ok(channels.into_iter().collect())
}

fn default_output_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join("ac-screenshots")
    } else {
        PathBuf::from("ac-screenshots")
    }
}

fn print_help() {
    println!(
        "ac-ui — GPU spectrum monitor\n\n\
Usage: ac-ui [OPTIONS]\n\n\
Options:\n  \
  --connect <addr>     ZMQ DATA endpoint [default: tcp://127.0.0.1:5557]\n  \
  --ctrl <addr>        ZMQ CTRL endpoint (REQ) [default: tcp://127.0.0.1:5556]\n  \
  --synthetic          Fake data instead of daemon\n  \
  --channels <spec>    Channel indices (0,2,5 or 0-3,7) or count (4 = 0..4) [default: auto from daemon.devices]\n  \
  --bins <n>           Synthetic bins per channel [default: 1000]\n  \
  --rate <hz>          Synthetic update rate [default: 10]\n  \
  --output-dir <path>  Screenshot/CSV dir [default: ~/ac-screenshots]\n  \
  --benchmark <secs>   Run for N seconds, print timing summary, exit\n  \
  --no-persist         Don't read/write ~/.config/ac/ui.json this session\n  \
  --view <mode>        Initial view: spectrum|waterfall|scope|spectrum_ember|goniometer|iotransfer|bode_mag|coherence|bode_phase|group_delay|nyquist|ir [default: spectrum]\n  \
  --mode <mode>        Start in sweep mode: sweep_frequency|sweep_level\n  \
  --present-mode <m>   wgpu present mode: auto-vsync|auto-no-vsync|fifo|fifo-relaxed|mailbox|immediate\n  \
                       (env: AC_UI_PRESENT_MODE) — try `mailbox` if NVIDIA + Vulkan pegs CPU at vsync\n  \
  --max-fps <hz>       Cap continuous-repaint rate. Default 30; bump to 60 on stacks with cheap present()\n  \
                       (env: AC_UI_MAX_FPS) — every doubling of fps roughly doubles GPU-driver CPU\n  \
  -h, --help           Show this help\n\n\
Keys (full list in-app: press h):\n  \
  Esc/q            quit\n  \
  Enter            toggle freeze\n  \
  s                save screenshot + CSV\n  \
  w                cycle view (matrix/single/waterfall-fft/waterfall-cwt)\n  \
  c                compare selected channels\n  \
  t                add virtual transfer (first selected = MEAS, last = REF)\n  \
  p / m            toggle peak / min hold (spectrum)\n  \
  o / Shift+O      1/N-oct smoothing / CWT 1/N-oct aggregation\n  \
  a / i            cycle weighting (A/C/Z) / time integration (fast/slow/Leq)\n  \
  Shift+I          reset Leq accumulators\n  \
  Shift+L          reset BS.1770 loudness\n  \
  Space            toggle channel selection at cursor\n  \
  d                toggle timing overlay\n  \
  f                toggle fullscreen\n  \
  h                toggle help overlay\n  \
  +/-              adjust dB span\n  \
  [/]              shift dB floor ±5\n  \
  ← / →            FFT monitor interval / Shift for CWT scales\n  \
  ↑ / ↓            FFT N ladder / Shift for CWT sigma\n  \
  Ctrl+R           reset all views and grid sizing\n  \
  Tab / Shift+Tab  next / prev grid page or channel\n\n\
Mouse:\n  \
  Scroll (cell)    zoom both axes\n  \
  Scroll (bg)      resize grid cells (grid layout only)\n  \
  Shift+Scroll     cycle waterfall palette (waterfall)\n  \
  Ctrl+Scroll      zoom freq (spectrum) / zoom time (waterfall)\n  \
  Left-click       zoom in: swap to Single on clicked cell (matrix)\n  \
  Left-drag        pan\n  \
  Right-click      reset hovered cell\n"
    );
}

#[cfg(test)]
mod view_mode_tests {
    use super::{parse_view_mode, ViewMode};

    #[test]
    fn parses_all_known_view_names() {
        let cases = [
            ("spectrum",         ViewMode::Spectrum),
            ("waterfall",        ViewMode::Waterfall),
            ("scope",            ViewMode::Scope),
            ("spectrum_ember",   ViewMode::SpectrumEmber),
            ("spectrum-ember",   ViewMode::SpectrumEmber),
            ("goniometer",       ViewMode::Goniometer),
            ("iotransfer",       ViewMode::IoTransfer),
            ("io_transfer",      ViewMode::IoTransfer),
            ("io-transfer",      ViewMode::IoTransfer),
            ("bode_mag",         ViewMode::BodeMag),
            ("bode-mag",         ViewMode::BodeMag),
            ("bodemag",          ViewMode::BodeMag),
            ("bode",             ViewMode::BodeMag),
            ("coherence",        ViewMode::Coherence),
            ("coh",              ViewMode::Coherence),
            ("bode_phase",       ViewMode::BodePhase),
            ("bode-phase",       ViewMode::BodePhase),
            ("bodephase",        ViewMode::BodePhase),
            ("phase",            ViewMode::BodePhase),
            ("group_delay",      ViewMode::GroupDelay),
            ("group-delay",      ViewMode::GroupDelay),
            ("groupdelay",       ViewMode::GroupDelay),
            ("gd",               ViewMode::GroupDelay),
            ("nyquist",          ViewMode::Nyquist),
            ("nyq",              ViewMode::Nyquist),
            ("ir",               ViewMode::Ir),
            ("impulse",          ViewMode::Ir),
            ("impulse_response", ViewMode::Ir),
        ];
        for (s, want) in cases {
            assert_eq!(parse_view_mode(s).unwrap(), want, "input {s:?}");
        }
    }

    #[test]
    fn unknown_view_errors_helpfully() {
        // Use a definitely-not-real view name (not just an
        // un-implemented Phase view name, which keeps becoming
        // valid as the plan rolls forward).
        let err = parse_view_mode("polezero").unwrap_err().to_string();
        assert!(err.contains("polezero"), "error mentions input: {err}");
        assert!(
            err.contains("goniometer") && err.contains("iotransfer")
                && err.contains("bode_mag") && err.contains("nyquist"),
            "error lists current view names: {err}"
        );
    }
}

#[cfg(test)]
mod present_mode_tests {
    use super::parse_present_mode;
    use wgpu::PresentMode;

    #[test]
    fn known_values_round_trip() {
        let cases = [
            ("auto-vsync",    PresentMode::AutoVsync),
            ("auto",          PresentMode::AutoVsync),
            ("auto-no-vsync", PresentMode::AutoNoVsync),
            ("no-vsync",      PresentMode::AutoNoVsync),
            ("fifo",          PresentMode::Fifo),
            ("fifo-relaxed",  PresentMode::FifoRelaxed),
            ("mailbox",       PresentMode::Mailbox),
            ("immediate",     PresentMode::Immediate),
        ];
        for (s, want) in cases {
            assert_eq!(parse_present_mode(s).unwrap(), want, "input {s:?}");
        }
    }

    #[test]
    fn underscored_aliases_match_dashed() {
        assert_eq!(
            parse_present_mode("auto_vsync").unwrap(),
            parse_present_mode("auto-vsync").unwrap(),
        );
        assert_eq!(
            parse_present_mode("auto_no_vsync").unwrap(),
            parse_present_mode("auto-no-vsync").unwrap(),
        );
        assert_eq!(
            parse_present_mode("fifo_relaxed").unwrap(),
            parse_present_mode("fifo-relaxed").unwrap(),
        );
    }

    #[test]
    fn unknown_values_error_clearly() {
        let err = parse_present_mode("vsync").unwrap_err().to_string();
        assert!(err.contains("vsync"), "error mentions input: {err}");
        assert!(err.contains("mailbox"), "error lists valid values: {err}");
    }
}

#[cfg(test)]
mod max_fps_tests {
    use super::parse_max_fps;
    use std::time::Duration;

    #[test]
    fn standard_rates() {
        assert_eq!(parse_max_fps("30").unwrap(), Duration::from_millis(33));
        assert_eq!(parse_max_fps("60").unwrap(), Duration::from_millis(16));
        // Floor case (5 Hz = 200 ms) and ceiling (240 Hz = 4 ms).
        assert_eq!(parse_max_fps("5").unwrap(),   Duration::from_millis(200));
        assert_eq!(parse_max_fps("240").unwrap(), Duration::from_millis(4));
    }

    #[test]
    fn out_of_range_rejected() {
        for s in ["0", "1", "4", "241", "1000"] {
            assert!(
                parse_max_fps(s).is_err(),
                "{s} hz should have been rejected as out of range",
            );
        }
    }

    #[test]
    fn non_integer_rejected() {
        for s in ["abc", "60hz", "60.5", ""] {
            assert!(
                parse_max_fps(s).is_err(),
                "{s:?} should have been rejected as non-integer",
            );
        }
    }
}
