use std::fmt;

// ---------------------------------------------------------------------------
// Token types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum LevelSpec {
    Dbfs(f64),
    Dbu(f64),
    Vrms(f64),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum TokenKind {
    Ppd,
    Steps,
    Time,
    Level,
    Freq,
    Bands,
}

type Token = (TokenKind, TokenValue);

#[derive(Debug, Clone)]
enum TokenValue {
    Float(f64),
    Int(u32),
    Level(LevelSpec),
}

impl TokenValue {
    fn as_f64(&self) -> f64 {
        match self {
            TokenValue::Float(v) => *v,
            TokenValue::Int(v) => *v as f64,
            TokenValue::Level(_) => panic!("expected float, got level"),
        }
    }
    fn as_u32(&self) -> u32 {
        match self {
            TokenValue::Int(v) => *v,
            TokenValue::Float(v) => *v as u32,
            TokenValue::Level(_) => panic!("expected int, got level"),
        }
    }
    fn as_level(&self) -> LevelSpec {
        match self {
            TokenValue::Level(l) => l.clone(),
            _ => panic!("expected level"),
        }
    }
}

// ---------------------------------------------------------------------------
// Token classifier
// ---------------------------------------------------------------------------

fn parse_level(s: &str) -> Result<LevelSpec, ()> {
    let s = s.to_lowercase();
    if let Some(rest) = s.strip_suffix("dbu") {
        return rest.parse::<f64>().map(LevelSpec::Dbu).map_err(|_| ());
    }
    if let Some(rest) = s.strip_suffix("dbfs") {
        return rest.parse::<f64>().map(LevelSpec::Dbfs).map_err(|_| ());
    }
    if let Some(rest) = s.strip_suffix("mvrms").or_else(|| s.strip_suffix("mv")) {
        return rest.parse::<f64>().map(|v| LevelSpec::Vrms(v / 1000.0)).map_err(|_| ());
    }
    if let Some(rest) = s.strip_suffix("vrms").or_else(|| s.strip_suffix('v')) {
        return rest.parse::<f64>().map(LevelSpec::Vrms).map_err(|_| ());
    }
    if let Some(rest) = s.strip_suffix("mvpp") {
        return rest
            .parse::<f64>()
            .map(|v| LevelSpec::Vrms(v / 1000.0 / (2.0 * std::f64::consts::SQRT_2)))
            .map_err(|_| ());
    }
    if let Some(rest) = s.strip_suffix("vpp") {
        return rest
            .parse::<f64>()
            .map(|v| LevelSpec::Vrms(v / (2.0 * std::f64::consts::SQRT_2)))
            .map_err(|_| ());
    }
    s.parse::<f64>().map(LevelSpec::Dbfs).map_err(|_| ())
}

fn parse_freq(s: &str) -> Result<f64, ()> {
    let s = s.to_lowercase();
    if let Some(rest) = s.strip_suffix("khz") {
        return rest.parse::<f64>().map(|v| v * 1000.0).map_err(|_| ());
    }
    if let Some(rest) = s.strip_suffix("hz") {
        return rest.parse::<f64>().map_err(|_| ());
    }
    Err(())
}

fn parse_time(s: &str) -> Result<f64, ()> {
    let s = s.to_lowercase();
    s.strip_suffix('s')
        .and_then(|rest| rest.parse::<f64>().ok())
        .ok_or(())
}

fn parse_ppd(s: &str) -> Result<u32, ()> {
    let s = s.to_lowercase();
    s.strip_suffix("ppd")
        .and_then(|rest| rest.parse::<f64>().ok())
        .map(|v| v as u32)
        .ok_or(())
}

fn parse_steps(s: &str) -> Result<u32, ()> {
    let s = s.to_lowercase();
    s.strip_suffix("steps")
        .or_else(|| s.strip_suffix("step"))
        .and_then(|rest| rest.parse::<f64>().ok())
        .map(|v| v as u32)
        .ok_or(())
}

/// Accepts `"bands <N>"` (parsed as the next token) or a single
/// composite `"<N>bands"` / `"<N>bpo"`. Only the composite form flows
/// through the token classifier; the `bands <N>` multi-token form is
/// handled inline by `parse_plot`.
fn parse_bands(s: &str) -> Result<u32, ()> {
    let s = s.to_lowercase();
    s.strip_suffix("bands")
        .or_else(|| s.strip_suffix("bpo"))
        .and_then(|rest| rest.parse::<f64>().ok())
        .map(|v| v as u32)
        .ok_or(())
}

fn classify(token: &str) -> Result<Token, String> {
    if let Ok(v) = parse_ppd(token) {
        return Ok((TokenKind::Ppd, TokenValue::Int(v)));
    }
    if let Ok(v) = parse_steps(token) {
        return Ok((TokenKind::Steps, TokenValue::Int(v)));
    }
    if let Ok(v) = parse_bands(token) {
        return Ok((TokenKind::Bands, TokenValue::Int(v)));
    }
    if let Ok(v) = parse_time(token) {
        return Ok((TokenKind::Time, TokenValue::Float(v)));
    }
    if let Ok(v) = parse_level(token) {
        return Ok((TokenKind::Level, TokenValue::Level(v)));
    }
    if let Ok(v) = parse_freq(token) {
        return Ok((TokenKind::Freq, TokenValue::Float(v)));
    }
    Err(format!("unrecognised token: {token:?}"))
}

fn classify_all(args: &[String]) -> Result<Vec<Token>, String> {
    args.iter().map(|a| classify(a)).collect()
}

// ---------------------------------------------------------------------------
// Grammar helpers
// ---------------------------------------------------------------------------

fn pull(tokens: &mut Vec<Token>, kind: TokenKind) -> Option<TokenValue> {
    let pos = tokens.iter().position(|(k, _)| *k == kind)?;
    Some(tokens.remove(pos).1)
}

fn check_empty(tokens: &[Token]) -> Result<(), String> {
    if tokens.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "unexpected token(s): {:?}",
            tokens.iter().map(|(k, _)| format!("{k:?}")).collect::<Vec<_>>()
        ))
    }
}

// ---------------------------------------------------------------------------
// Abbreviation expansion
// ---------------------------------------------------------------------------

fn expand(word: &str) -> &str {
    match word.to_lowercase().as_str() {
        "s" | "sw" => "sweep",
        "m" | "mon" => "monitor",
        "g" | "gen" => "generate",
        "c" | "cal" => "calibrate",
        "p" | "pl" => "plot",
        "pr" => "probe",
        "te" | "tst" => "test",
        "ser" => "server",
        "n" => "new",
        "ses" | "sess" => "sessions",
        "u" => "use",
        "df" => "diff",
        "l" | "lev" => "level",
        "f" | "freq" => "frequency",
        "si" => "sine",
        "pk" => "pink",
        "so" | "soft" => "software",
        "h" | "hw" => "hardware",
        "du" | "dut" => "dut",
        "comp" => "compare",
        "sh" => "show",
        "ls" => "sessions",
        "dmm" => "dmm",
        "stop" | "st" => "stop",
        "se" | "set" => "setup",
        "d" | "dev" | "devs" => "devices",
        "o" | "out" => "output",
        "i" | "in" => "input",
        "r" | "ra" => "range",
        "ref" => "reference",
        _ => {
            // Return the original word — but we need 'static lifetime.
            // We leak here; it's fine for CLI arg parsing (called once).
            return Box::leak(word.to_lowercase().into_boxed_str());
        }
    }
}

fn extract_show(args: &[String]) -> (Vec<String>, bool) {
    let mut show = false;
    let mut cleaned = Vec::new();
    for a in args {
        if a.eq_ignore_ascii_case("show") || a.eq_ignore_ascii_case("sh") {
            show = true;
        } else {
            cleaned.push(a.clone());
        }
    }
    (cleaned, show)
}

pub fn parse_channels(token: &str) -> Result<Vec<u32>, String> {
    let mut channels = std::collections::BTreeSet::new();
    for part in token.split(',') {
        let part = part.trim();
        if let Some((lo, hi)) = part.split_once('-') {
            let lo: u32 = lo.parse().map_err(|_| format!("bad channel: {part:?}"))?;
            let hi: u32 = hi.parse().map_err(|_| format!("bad channel: {part:?}"))?;
            for ch in lo..=hi {
                channels.insert(ch);
            }
        } else {
            let ch: u32 = part.parse().map_err(|_| format!("bad channel: {part:?}"))?;
            channels.insert(ch);
        }
    }
    Ok(channels.into_iter().collect())
}

fn is_channel_spec(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit() || b == b',' || b == b'-')
        && s.bytes().next().map_or(false, |b| b.is_ascii_digit())
}

// ---------------------------------------------------------------------------
// Parsed command types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ParsedCommand {
    pub cmd: CommandKind,
    pub show_plot: bool,
}

#[derive(Debug, Clone)]
pub enum CommandKind {
    Devices,
    Setup {
        output: Option<u32>,
        input: Option<u32>,
        reference: Option<u32>,
        device: Option<u32>,
        dbu_ref_vrms: Option<f64>,
        dmm_host: Option<String>,
        gpio_port: Option<Option<String>>,
        range_start: Option<f64>,
        range_stop: Option<f64>,
    },
    Stop,
    SweepLevel {
        start: LevelSpec,
        stop: LevelSpec,
        freq: f64,
        duration: f64,
    },
    SweepFrequency {
        start: Option<f64>,
        stop: Option<f64>,
        level: LevelSpec,
        duration: f64,
    },
    SweepIr {
        f1: f64,
        f2: f64,
        duration: f64,
        level: LevelSpec,
    },
    Plot {
        start: Option<f64>,
        stop: Option<f64>,
        level: LevelSpec,
        ppd: u32,
        /// If set, run the concatenated sweep capture through an
        /// IEC 61260-1 filterbank at this bands-per-octave resolution
        /// and emit an additional `SpectrumBands` report.
        bpo: Option<u32>,
    },
    PlotLevel {
        start: LevelSpec,
        stop: LevelSpec,
        freq: f64,
        steps: u32,
    },
    Monitor {
        start_freq: f64,
        end_freq: f64,
        interval: f64,
        channels: Option<Vec<u32>>,
    },
    MonitorCwt {
        start_freq: f64,
        end_freq: f64,
        interval: f64,
        channels: Option<Vec<u32>>,
    },
    MonitorNotImplemented {
        technique: &'static str,
    },
    GenerateSine {
        level: Option<LevelSpec>,
        freq: f64,
        channels: Option<String>,
    },
    GeneratePink {
        level: Option<LevelSpec>,
        channels: Option<String>,
    },
    Calibrate {
        level: LevelSpec,
        output_channel: Option<u32>,
        input_channel: Option<u32>,
    },
    CalibrateShow,
    ServerEnable,
    ServerDisable,
    ServerConnections,
    ServerSetHost {
        host: String,
    },
    SessionNew {
        name: String,
    },
    SessionList,
    SessionUse {
        name: String,
    },
    SessionRm {
        name: String,
    },
    SessionDiff {
        name_a: String,
        name_b: String,
    },
    TestSoftware,
    TestHardware {
        dmm: bool,
    },
    TestDut {
        compare: bool,
        level: LevelSpec,
    },
    Probe,
    DmmShow,
    Gpio {
        log: bool,
    },
    Report {
        path: String,
        format: ReportFormat,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    Html,
    Pdf,
}

// ---------------------------------------------------------------------------
// Main parser
// ---------------------------------------------------------------------------

pub fn parse(argv: &[String]) -> Result<ParsedCommand, String> {
    if argv.is_empty() {
        return Err("no command given".into());
    }

    let mut args: Vec<String> = argv.to_vec();
    let verb = expand(&args.remove(0)).to_string();

    // "ac calibrate show"
    if verb == "calibrate" && args.first().map(|a| expand(a)) == Some("show") {
        return Ok(ParsedCommand {
            cmd: CommandKind::CalibrateShow,
            show_plot: false,
        });
    }

    let (args, show_plot) = extract_show(&args);
    let mut args = args;

    match verb.as_str() {
        "sweep" => parse_sweep(&mut args, show_plot),
        "monitor" => parse_monitor(&args, show_plot),
        "plot" => parse_plot(&mut args, show_plot),
        "generate" => parse_generate(&mut args, show_plot),
        "calibrate" => parse_calibrate(&args, show_plot),
        "stop" => Ok(ParsedCommand {
            cmd: CommandKind::Stop,
            show_plot: false,
        }),
        "dmm" => Ok(ParsedCommand {
            cmd: CommandKind::DmmShow,
            show_plot: false,
        }),
        "devices" => Ok(ParsedCommand {
            cmd: CommandKind::Devices,
            show_plot: false,
        }),
        "setup" => parse_setup(&args),
        "server" => parse_server(&args),
        "new" => {
            if args.is_empty() {
                return Err("new: requires a session name".into());
            }
            if args.len() > 1 {
                return Err(format!("new: unexpected extra args: {args:?}"));
            }
            Ok(ParsedCommand {
                cmd: CommandKind::SessionNew {
                    name: args[0].clone(),
                },
                show_plot: false,
            })
        }
        "sessions" => Ok(ParsedCommand {
            cmd: CommandKind::SessionList,
            show_plot: false,
        }),
        "use" => {
            if args.is_empty() {
                return Err("use: requires a session name".into());
            }
            if args.len() > 1 {
                return Err(format!("use: unexpected extra args: {args:?}"));
            }
            Ok(ParsedCommand {
                cmd: CommandKind::SessionUse {
                    name: args[0].clone(),
                },
                show_plot: false,
            })
        }
        "rm" => {
            if args.is_empty() {
                return Err("rm: requires a session name".into());
            }
            if args.len() > 1 {
                return Err(format!("rm: unexpected extra args: {args:?}"));
            }
            Ok(ParsedCommand {
                cmd: CommandKind::SessionRm {
                    name: args[0].clone(),
                },
                show_plot: false,
            })
        }
        "diff" => {
            if args.len() < 2 {
                return Err("diff: requires two session names".into());
            }
            if args.len() > 2 {
                return Err(format!("diff: unexpected extra args: {:?}", &args[2..]));
            }
            Ok(ParsedCommand {
                cmd: CommandKind::SessionDiff {
                    name_a: args[0].clone(),
                    name_b: args[1].clone(),
                },
                show_plot: false,
            })
        }
        "test" => parse_test(&mut args),
        "probe" => {
            if !args.is_empty() {
                return Err(format!("probe: unexpected argument(s): {args:?}"));
            }
            Ok(ParsedCommand {
                cmd: CommandKind::Probe,
                show_plot: false,
            })
        }
        "gpio" => {
            let log = args.first().map_or(false, |a| a.eq_ignore_ascii_case("log"));
            Ok(ParsedCommand {
                cmd: CommandKind::Gpio { log },
                show_plot: false,
            })
        }
        "report" => {
            if args.is_empty() || args.len() > 2 {
                return Err("report: requires <path.json> [html|pdf]".into());
            }
            let format = match args.get(1).map(String::as_str) {
                None | Some("html") => ReportFormat::Html,
                Some("pdf")         => ReportFormat::Pdf,
                Some(other) => {
                    return Err(format!("report: unknown format {other:?} (expected html or pdf)"));
                }
            };
            Ok(ParsedCommand {
                cmd: CommandKind::Report {
                    path: args[0].clone(),
                    format,
                },
                show_plot: false,
            })
        }
        other => Err(format!(
            "unknown command: {other:?}  \
             (sweep | monitor | plot | transfer | generate | calibrate | \
             setup | devices | server | new | sessions | use | rm | diff | probe | gpio | report)"
        )),
    }
}

// ---------------------------------------------------------------------------
// Subcommand parsers — each in its own sibling file (sweep.rs, plot.rs, …).
// ---------------------------------------------------------------------------

mod calibrate;
mod generate;
mod monitor;
mod plot;
mod server;
mod setup;
mod sweep;
mod test;

use calibrate::parse_calibrate;
use generate::parse_generate;
use monitor::parse_monitor;
use plot::parse_plot;
use server::parse_server;
use setup::parse_setup;
use sweep::parse_sweep;
use test::parse_test;

// ---------------------------------------------------------------------------
// Usage string
// ---------------------------------------------------------------------------

pub const USAGE: &str = "\
ac — audio measurement CLI

Commands:
  devices                                                       list available audio ports
  calibrate       [output <N> input <N>] [show]                 level calibration
  generate        <sine|pink> [ch] [level] [freq]               output sine/pink
  sweep level     <start> <stop> [freq]                         sweep level with fixed frequency
  sweep frequency <freqStart freqStop> [level]                  sweep frequency with fixed level
  sweep ir        [freqStart freqStop] [duration] [level]       Farina log-sweep impulse response
  plot            [<freqStart freqStop>] [level] [ppd] [show]   per point THD vs frequency
  plot level      <start> <stop> [freq] [steps] [show]         per point THD vs level
  monitor         [channels] [<freqStart freqStop>] [interval] [show]  live spectrum
  stop                                                          stop active generator/measurement
  test software                                                  validate analysis pipeline (no hardware)
  test hardware   [dmm]                                          hardware validation (requires 2 loopbacks)
  test dut        [compare] [level]                              DUT characterization (requires 2 loopbacks)
  probe                                                         auto-detect analog ports and loopback pairs
  dmm                                                           read AC Vrms from configured DMM over SCPI
  report          <path.json> [html|pdf]                        render MeasurementReport JSON (default: html; sibling .html/.pdf)
  setup           [output <N>] [input <N>] [reference <N>]
                  [range <freqStart freqStop>]
                  [dmm <ipaddr>] [gpio <serialDevice>]

Units:  20hz 1khz  |  0dbu -12dbfs 775mvrms 1vrms  |  1s  |  10ppd
        append \"show\" to open GPU view window

Short forms:  s(weep) m(onitor) g(enerate) c(alibrate) p(lot) pr(obe) te(st)
              l(evel) f(requency) si(ne) pk(ink) sh(ow) so(ftware) h(ardware)
              se(tup) d(evices) st(op) ref(erence)

Sessions:
  new|use|ls|rm|diff                                            create, switch, list, remove, compare

Server:
  server [<enable|disable>] [connections]                       enable/disable server, show connections
  server <host>                                                 connect to remote host

Examples:
  ac setup output 11 input 0
  ac calibrate
  ac g si 0dbu 1khz
  ac plot 20hz 20khz 0dbu 20ppd show
  ac plot level -20dbu 6dbu 1khz 26steps show
  ac m sh
  ac s f 20hz 20khz 0dbu";

// ---------------------------------------------------------------------------
// Display impl for LevelSpec
// ---------------------------------------------------------------------------

impl fmt::Display for LevelSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LevelSpec::Dbfs(v) => write!(f, "{v:.1} dBFS"),
            LevelSpec::Dbu(v) => write!(f, "{v:.1} dBu"),
            LevelSpec::Vrms(v) => write!(f, "{v:.6} Vrms"),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn test_classify_freq() {
        assert!(matches!(classify("20hz"), Ok((TokenKind::Freq, TokenValue::Float(v))) if (v - 20.0).abs() < 1e-9));
        assert!(matches!(classify("1khz"), Ok((TokenKind::Freq, TokenValue::Float(v))) if (v - 1000.0).abs() < 1e-9));
        assert!(matches!(classify("20000hz"), Ok((TokenKind::Freq, TokenValue::Float(v))) if (v - 20000.0).abs() < 1e-9));
    }

    #[test]
    fn test_classify_level() {
        match classify("0dbu") {
            Ok((TokenKind::Level, TokenValue::Level(LevelSpec::Dbu(v)))) => assert!((v - 0.0).abs() < 1e-9),
            other => panic!("expected Dbu(0.0), got {other:?}"),
        }
        match classify("-12dbfs") {
            Ok((TokenKind::Level, TokenValue::Level(LevelSpec::Dbfs(v)))) => assert!((v - (-12.0)).abs() < 1e-9),
            other => panic!("expected Dbfs(-12.0), got {other:?}"),
        }
        match classify("775mvrms") {
            Ok((TokenKind::Level, TokenValue::Level(LevelSpec::Vrms(v)))) => assert!((v - 0.775).abs() < 1e-9),
            other => panic!("expected Vrms(0.775), got {other:?}"),
        }
        match classify("1vrms") {
            Ok((TokenKind::Level, TokenValue::Level(LevelSpec::Vrms(v)))) => assert!((v - 1.0).abs() < 1e-9),
            other => panic!("expected Vrms(1.0), got {other:?}"),
        }
    }

    #[test]
    fn test_classify_time() {
        assert!(matches!(classify("0.2s"), Ok((TokenKind::Time, TokenValue::Float(v))) if (v - 0.2).abs() < 1e-9));
        assert!(matches!(classify("1s"), Ok((TokenKind::Time, TokenValue::Float(v))) if (v - 1.0).abs() < 1e-9));
    }

    #[test]
    fn test_classify_ppd() {
        assert!(matches!(classify("10ppd"), Ok((TokenKind::Ppd, TokenValue::Int(10)))));
    }

    #[test]
    fn test_classify_steps() {
        assert!(matches!(classify("26steps"), Ok((TokenKind::Steps, TokenValue::Int(26)))));
        assert!(matches!(classify("1step"), Ok((TokenKind::Steps, TokenValue::Int(1)))));
    }

    #[test]
    fn test_devices() {
        let p = parse(&args("devices")).unwrap();
        assert!(matches!(p.cmd, CommandKind::Devices));
    }

    #[test]
    fn test_abbreviated_devices() {
        let p = parse(&args("d")).unwrap();
        assert!(matches!(p.cmd, CommandKind::Devices));
    }

    #[test]
    fn test_stop() {
        let p = parse(&args("stop")).unwrap();
        assert!(matches!(p.cmd, CommandKind::Stop));
        let p = parse(&args("st")).unwrap();
        assert!(matches!(p.cmd, CommandKind::Stop));
    }

    #[test]
    fn test_session_new() {
        let p = parse(&args("new test-session")).unwrap();
        match p.cmd {
            CommandKind::SessionNew { name } => assert_eq!(name, "test-session"),
            other => panic!("expected SessionNew, got {other:?}"),
        }
    }

    #[test]
    fn test_session_list() {
        let p = parse(&args("sessions")).unwrap();
        assert!(matches!(p.cmd, CommandKind::SessionList));
        let p = parse(&args("ls")).unwrap();
        assert!(matches!(p.cmd, CommandKind::SessionList));
    }

    #[test]
    fn test_session_use() {
        let p = parse(&args("use my-session")).unwrap();
        match p.cmd {
            CommandKind::SessionUse { name } => assert_eq!(name, "my-session"),
            other => panic!("expected SessionUse, got {other:?}"),
        }
    }

    #[test]
    fn test_session_diff() {
        let p = parse(&args("diff session-a session-b")).unwrap();
        match p.cmd {
            CommandKind::SessionDiff { name_a, name_b } => {
                assert_eq!(name_a, "session-a");
                assert_eq!(name_b, "session-b");
            }
            other => panic!("expected SessionDiff, got {other:?}"),
        }
    }

    #[test]
    fn test_probe() {
        let p = parse(&args("probe")).unwrap();
        assert!(matches!(p.cmd, CommandKind::Probe));
    }

    #[test]
    fn test_gpio() {
        let p = parse(&args("gpio")).unwrap();
        assert!(matches!(p.cmd, CommandKind::Gpio { log: false }));
        let p = parse(&args("gpio log")).unwrap();
        assert!(matches!(p.cmd, CommandKind::Gpio { log: true }));
    }

    #[test]
    fn test_dmm() {
        let p = parse(&args("dmm")).unwrap();
        assert!(matches!(p.cmd, CommandKind::DmmShow));
    }

    #[test]
    fn test_channels_parsing() {
        assert_eq!(parse_channels("11").unwrap(), vec![11]);
        assert_eq!(parse_channels("0,2,5").unwrap(), vec![0, 2, 5]);
        assert_eq!(parse_channels("0-3").unwrap(), vec![0, 1, 2, 3]);
        assert_eq!(parse_channels("0-3,7").unwrap(), vec![0, 1, 2, 3, 7]);
    }

    #[test]
    fn test_level_vpp() {
        match classify("2vpp") {
            Ok((TokenKind::Level, TokenValue::Level(LevelSpec::Vrms(v)))) => {
                let expected = 2.0 / (2.0 * std::f64::consts::SQRT_2);
                assert!((v - expected).abs() < 1e-9);
            }
            other => panic!("expected Vrms from vpp, got {other:?}"),
        }
    }

    #[test]
    fn test_error_no_command() {
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn test_error_unknown_command() {
        assert!(parse(&args("banana")).is_err());
    }

    #[test]
    fn test_error_sweep_no_noun() {
        assert!(parse(&args("sweep")).is_err());
    }

    #[test]
    fn test_error_generate_no_noun() {
        assert!(parse(&args("generate")).is_err());
    }

    #[test]
    fn test_mv_suffix() {
        match classify("245mv") {
            Ok((TokenKind::Level, TokenValue::Level(LevelSpec::Vrms(v)))) => {
                assert!((v - 0.245).abs() < 1e-9);
            }
            other => panic!("expected Vrms(0.245), got {other:?}"),
        }
    }

}
