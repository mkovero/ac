mod app;
mod data;
mod render;
mod theme;
mod ui;

use std::path::PathBuf;

use app::{App, AppInit, SourceKind};
use data::control::CtrlClient;
use data::store::{ChannelStore, SweepStore, TransferStore, TunerStore, VirtualChannelStore};
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
        match probe_daemon_channels(&args.ctrl) {
            Some(n) if n >= 1 => {
                log::info!("discovered {n} capture channels from daemon");
                (None, n)
            }
            _ => {
                log::warn!("daemon probe failed; defaulting to 2 channel slots");
                (None, 2)
            }
        }
    };
    let (inputs, store) = ChannelStore::new(n_channels);
    let transfer_store = TransferStore::new();
    let virtual_channels = VirtualChannelStore::new();
    let sweep_store = SweepStore::new();
    let tuner_store = TunerStore::new();

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
        tuner_store,
        source_kind,
        output_dir: args.output_dir.clone(),
        endpoint: args.connect.clone(),
        ctrl_endpoint: args.ctrl.clone(),
        synthetic_params: Some((n_channels, args.bins.max(16), args.rate.max(0.5))),
        benchmark_secs: args.benchmark,
        initial_view: args.view,
        initial_sweep_kind: args.mode,
        monitor_channels,
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
    mode: Option<SweepKind>,
}

impl Args {
    fn parse(args: impl Iterator<Item = String>) -> anyhow::Result<Self> {
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
            mode: None,
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
                "--view" => {
                    let v = it
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--view requires value"))?;
                    out.view = match v.as_str() {
                        "spectrum" => ViewMode::Spectrum,
                        "waterfall" => ViewMode::Waterfall,
                        other => anyhow::bail!("--view: expected spectrum|waterfall, got {other}"),
                    };
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
                other => anyhow::bail!("unknown argument: {other}"),
            }
        }
        Ok(out)
    }
}

/// Best-effort sync probe of the daemon's `devices` reply to discover how
/// many capture slots to preallocate. Short timeouts — if the daemon isn't
/// up yet we fall back to a safe default rather than blocking startup.
fn probe_daemon_channels(ctrl_endpoint: &str) -> Option<usize> {
    let ctrl = CtrlClient::connect(ctrl_endpoint).ok()?;
    let reply = ctrl.send(&serde_json::json!({ "cmd": "devices" })).ok()?;
    reply
        .get("capture")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
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
  --view <mode>        Initial view: spectrum|waterfall [default: spectrum]\n  \
  --mode <mode>        Start in sweep mode: sweep_frequency|sweep_level\n  \
  -h, --help           Show this help\n\n\
Keys:\n  \
  Esc/q            quit\n  \
  Enter            toggle freeze\n  \
  p                toggle peak hold\n  \
  Space            select channel (for compare / transfer layout)\n  \
  s                save screenshot + CSV\n  \
  d                toggle GPU/CPU timing overlay\n  \
  w                cycle view (spectrum/waterfall)\n  \
  l                cycle layout (grid/single/compare*/transfer*)\n  \
  f                toggle fullscreen\n  \
  h                toggle help overlay\n  \
  +/-              adjust dB range\n  \
  [/]              shift waterfall colormap floor\n  \
  Ctrl+R           reset all views and grid sizing\n  \
  Tab              next page (grid) / next channel\n  \
  Shift+Tab        prev page (grid) / prev channel / prev meas (transfer)\n\n\
* compare/transfer only cycle-visible when channels are selected; in\n  \
   transfer the last Space is REF, earlier picks are meas, Tab rotates meas\n\n\
Mouse:\n  \
  Scroll (cell)    zoom freq (waterfall) / both axes (spectrum)\n  \
  Scroll (bg)      resize grid cells (grid layout only)\n  \
  Shift+Scroll     zoom dB / gain\n  \
  Ctrl+Scroll      zoom freq (spectrum) / zoom time (waterfall)\n  \
  Left-drag        pan view\n  \
  Right-click      reset hovered cell\n"
    );
}
