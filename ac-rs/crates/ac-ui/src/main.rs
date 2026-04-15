mod app;
mod data;
mod render;
mod theme;
mod ui;

use std::path::PathBuf;

use app::{App, AppInit, SourceKind};
use data::store::ChannelStore;
use data::types::ViewMode;

fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Args::parse(std::env::args().skip(1))?;
    if args.help {
        print_help();
        return Ok(());
    }

    let n_channels = args.channels.max(1);
    let (inputs, store) = ChannelStore::new(n_channels);

    let source_kind = if args.synthetic {
        SourceKind::Synthetic
    } else {
        SourceKind::Daemon
    };

    let init = AppInit {
        store,
        inputs,
        source_kind,
        output_dir: args.output_dir.clone(),
        endpoint: args.connect.clone(),
        synthetic_params: Some((args.channels.max(1), args.bins.max(16), args.rate.max(0.5))),
        benchmark_secs: args.benchmark,
        initial_view: args.view,
    };

    let event_loop = winit::event_loop::EventLoop::new()?;
    event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);
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
    synthetic: bool,
    channels: usize,
    bins: usize,
    rate: f32,
    output_dir: PathBuf,
    benchmark: Option<f64>,
    view: ViewMode,
}

impl Args {
    fn parse(args: impl Iterator<Item = String>) -> anyhow::Result<Self> {
        let mut out = Args {
            help: false,
            connect: "tcp://127.0.0.1:5557".to_string(),
            synthetic: false,
            channels: 1,
            bins: 1000,
            rate: 10.0,
            output_dir: default_output_dir(),
            benchmark: None,
            view: ViewMode::Spectrum,
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
                "--channels" => {
                    out.channels = it
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("--channels requires value"))?
                        .parse()?;
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
                other => anyhow::bail!("unknown argument: {other}"),
            }
        }
        Ok(out)
    }
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
  --synthetic          Fake data instead of daemon\n  \
  --channels <n>       Channel slot count; daemon must emit matching `channel` field [default: 1]\n  \
  --bins <n>           Synthetic bins per channel [default: 1000]\n  \
  --rate <hz>          Synthetic update rate [default: 10]\n  \
  --output-dir <path>  Screenshot/CSV dir [default: ~/ac-screenshots]\n  \
  --benchmark <secs>   Run for N seconds, print timing summary, exit\n  \
  --view <mode>        Initial view: spectrum|waterfall [default: spectrum]\n  \
  -h, --help           Show this help\n\n\
Keys:\n  \
  Esc/q            quit\n  \
  Enter            toggle freeze\n  \
  p                toggle peak hold\n  \
  Space            select channel (for compare layout)\n  \
  s                save screenshot + CSV\n  \
  d                toggle GPU/CPU timing overlay\n  \
  w                cycle view (spectrum/waterfall)\n  \
  l                cycle layout (grid/overlay/single/compare)\n  \
  f                toggle fullscreen\n  \
  h                toggle help overlay\n  \
  +/-              adjust dB range\n  \
  [/]              shift waterfall colormap floor\n  \
  Ctrl+R           reset all views and grid sizing\n  \
  Tab              next page (grid) / next channel\n  \
  Shift+Tab        prev page (grid) / prev channel\n\n\
Mouse:\n  \
  Scroll (cell)    zoom freq (waterfall) / both axes (spectrum)\n  \
  Scroll (bg)      resize grid cells (grid layout only)\n  \
  Shift+Scroll     zoom dB / gain\n  \
  Ctrl+Scroll      zoom freq (spectrum) / zoom time (waterfall)\n  \
  Left-drag        pan view\n  \
  Right-click      reset hovered cell\n"
    );
}
