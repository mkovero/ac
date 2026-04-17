use std::path::Path;

pub fn save_csv(results: &[serde_json::Value], path: &Path) {
    let fields = [
        "freq_hz",
        "drive_db",
        "out_vrms",
        "out_dbu",
        "fundamental_dbfs",
        "in_vrms",
        "in_dbu",
        "thd_pct",
        "thdn_pct",
        "noise_floor_dbfs",
    ];

    let mut wtr = match csv::Writer::from_path(path) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("  error: cannot write CSV: {e}");
            return;
        }
    };

    wtr.write_record(&fields).ok();
    for r in results {
        let mut row = Vec::with_capacity(fields.len());
        for &f in &fields {
            let key = if f == "freq_hz" {
                r.get("freq_hz")
                    .or_else(|| r.get("fundamental_hz"))
            } else {
                r.get(f)
            };
            match key {
                Some(serde_json::Value::Number(n)) => row.push(n.to_string()),
                Some(serde_json::Value::String(s)) => row.push(s.clone()),
                _ => row.push(String::new()),
            }
        }
        wtr.write_record(&row).ok();
    }
    wtr.flush().ok();
    println!("  CSV  -> {}", path.display());
}

pub fn save_transfer_csv(result: &serde_json::Value, path: &Path) {
    let mut wtr = match csv::Writer::from_path(path) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("  error: cannot write CSV: {e}");
            return;
        }
    };

    wtr.write_record(["freq_hz", "magnitude_db", "phase_deg", "coherence"])
        .ok();

    let freqs = result.get("freqs").and_then(|v| v.as_array());
    let mag = result.get("magnitude_db").and_then(|v| v.as_array());
    let phase = result.get("phase_deg").and_then(|v| v.as_array());
    let coh = result.get("coherence").and_then(|v| v.as_array());

    if let (Some(fs), Some(ms), Some(ps), Some(cs)) = (freqs, mag, phase, coh) {
        for i in 0..fs.len() {
            let f = fs.get(i).and_then(|v| v.as_f64()).unwrap_or(0.0);
            let m = ms.get(i).and_then(|v| v.as_f64()).unwrap_or(0.0);
            let p = ps.get(i).and_then(|v| v.as_f64()).unwrap_or(0.0);
            let c = cs.get(i).and_then(|v| v.as_f64()).unwrap_or(0.0);
            wtr.write_record(&[
                format!("{f}"),
                format!("{m}"),
                format!("{p}"),
                format!("{c}"),
            ])
            .ok();
        }
    }
    wtr.flush().ok();
    println!("  CSV  -> {}", path.display());
}

pub fn print_summary(results: &[serde_json::Value], device_name: &str, have_cal: bool) {
    if results.is_empty() {
        return;
    }

    let mut clean = Vec::new();
    let mut clipped_n = 0u32;
    let mut ac_n = 0u32;
    for r in results {
        let clip = r.get("clipping").and_then(|v| v.as_bool()).unwrap_or(false);
        let ac = r.get("ac_coupled").and_then(|v| v.as_bool()).unwrap_or(false);
        if clip {
            clipped_n += 1;
        }
        if ac {
            ac_n += 1;
        }
        if !clip && !ac {
            clean.push(r);
        }
    }
    let valid: Vec<&serde_json::Value> = if clean.is_empty() {
        results.iter().collect()
    } else {
        clean
    };

    let worst_thd = valid
        .iter()
        .filter_map(|r| r.get("thd_pct").and_then(|v| v.as_f64()))
        .fold(0.0_f64, f64::max);
    let worst_thdn = valid
        .iter()
        .filter_map(|r| r.get("thdn_pct").and_then(|v| v.as_f64()))
        .fold(0.0_f64, f64::max);
    let thds: Vec<f64> = valid
        .iter()
        .filter_map(|r| r.get("thd_pct").and_then(|v| v.as_f64()))
        .collect();
    let avg_thd = if thds.is_empty() {
        0.0
    } else {
        thds.iter().sum::<f64>() / thds.len() as f64
    };

    println!("\n{}", "=".repeat(62));
    println!("  SUMMARY -- {device_name}");
    println!("{}", "\u{2500}".repeat(62));
    println!("  Levels measured:  {}", results.len());
    if clipped_n > 0 {
        println!("  Clipped points:   {clipped_n}  (excluded)");
    }
    if ac_n > 0 {
        println!("  AC-coupled pts:   {ac_n}  (excluded -- coupling cap rolloff)");
    }
    println!("  Worst THD:        {worst_thd:.4}%");
    println!("  Worst THD+N:      {worst_thdn:.4}%");
    let note = if clipped_n > 0 || ac_n > 0 {
        "  (valid points only)"
    } else {
        ""
    };
    println!("  Average THD:      {avg_thd:.4}%{note}");

    if have_cal {
        let lo = results.first().and_then(|r| r.get("out_vrms")).and_then(|v| v.as_f64());
        let hi = results.last().and_then(|r| r.get("out_vrms")).and_then(|v| v.as_f64());
        if let (Some(lo_v), Some(hi_v)) = (lo, hi) {
            let lo_dbu = ac_core::conversions::vrms_to_dbu(lo_v);
            let hi_dbu = ac_core::conversions::vrms_to_dbu(hi_v);
            println!(
                "\n  Output range:  {} ({lo_dbu:+.1} dBu)  ->  {} ({hi_dbu:+.1} dBu)",
                ac_core::conversions::fmt_vrms(lo_v),
                ac_core::conversions::fmt_vrms(hi_v),
            );
        }
        let ivs: Vec<f64> = results
            .iter()
            .filter_map(|r| r.get("in_vrms").and_then(|v| v.as_f64()))
            .collect();
        if !ivs.is_empty() {
            let lo = ivs.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = ivs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            println!(
                "  DUT out range: {} ({:+.1} dBu)  ->  {} ({:+.1} dBu)",
                ac_core::conversions::fmt_vrms(lo),
                ac_core::conversions::vrms_to_dbu(lo),
                ac_core::conversions::fmt_vrms(hi),
                ac_core::conversions::vrms_to_dbu(hi),
            );
        }
    }
    println!("{}\n", "=".repeat(62));
}

pub fn session_dir(name: &str) -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home)
        .join(".local/share/ac/sessions")
        .join(name)
}

pub fn output_dir(cfg: &ac_core::config::Config) -> std::path::PathBuf {
    if let Some(ref sess) = cfg.session {
        let d = session_dir(sess);
        std::fs::create_dir_all(&d).ok();
        d
    } else {
        std::path::PathBuf::from(".")
    }
}

pub fn timestamp() -> String {
    chrono::Local::now().format("%Y%m%d_%H%M%S").to_string()
}

pub fn print_freq_header(have_cal: bool) {
    println!("\n{}", "\u{2500}".repeat(78));
    if have_cal {
        println!(
            "  {:>8}  {:>12}  {:>8}  {:>12}  {:>8}  {:>8}  {:>9}  {:>9}",
            "Freq", "Out Vrms", "Out dBu", "In Vrms", "In dBu", "Gain", "THD%", "THD+N%"
        );
        println!(
            "  {}  {}  {}  {}  {}  {}  {}  {}",
            "\u{2500}".repeat(8),
            "\u{2500}".repeat(12),
            "\u{2500}".repeat(8),
            "\u{2500}".repeat(12),
            "\u{2500}".repeat(8),
            "\u{2500}".repeat(8),
            "\u{2500}".repeat(9),
            "\u{2500}".repeat(9),
        );
    } else {
        println!(
            "  {:>8}  {:>9}  {:>9}",
            "Freq", "THD%", "THD+N%"
        );
    }
}

pub fn print_freq_row(frame: &serde_json::Value) {
    let freq = frame
        .get("freq_hz")
        .or_else(|| frame.get("fundamental_hz"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let thd = frame.get("thd_pct").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let thdn = frame.get("thdn_pct").and_then(|v| v.as_f64()).unwrap_or(0.0);

    let clip = frame.get("clipping").and_then(|v| v.as_bool()).unwrap_or(false);
    let ac = frame.get("ac_coupled").and_then(|v| v.as_bool()).unwrap_or(false);
    let flag = if clip {
        "  [CLIP]"
    } else if ac {
        "  [AC]"
    } else {
        ""
    };

    if let Some(out_vrms) = frame.get("out_vrms").and_then(|v| v.as_f64()) {
        let out_s = ac_core::conversions::fmt_vrms(out_vrms);
        let in_s = frame
            .get("in_vrms")
            .and_then(|v| v.as_f64())
            .map(|v| ac_core::conversions::fmt_vrms(v))
            .unwrap_or_else(|| "  -".into());
        let odbu = frame
            .get("out_dbu")
            .and_then(|v| v.as_f64())
            .map(|v| format!("{v:+.2}"))
            .unwrap_or_else(|| "  -".into());
        let idbu = frame
            .get("in_dbu")
            .and_then(|v| v.as_f64())
            .map(|v| format!("{v:+.2}"))
            .unwrap_or_else(|| "  -".into());
        let gain_s = frame
            .get("gain_db")
            .and_then(|v| v.as_f64())
            .map(|v| format!("{v:+.2}dB"))
            .unwrap_or_else(|| "  -".into());
        println!(
            "  {freq:>7.0} Hz  {out_s:>12}  {odbu:>8}  {in_s:>12}  {idbu:>8}  {gain_s:>8}  {thd:>9.4}  {thdn:>9.4}{flag}"
        );
    } else {
        println!("  {freq:>7.0} Hz  {thd:>9.4}  {thdn:>9.4}{flag}");
    }
}
