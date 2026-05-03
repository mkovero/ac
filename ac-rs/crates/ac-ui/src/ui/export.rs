use std::path::PathBuf;
use std::thread;

use crate::data::types::{DisplayFrame, TransferFrame};

pub struct ScreenshotRequest {
    pub output_dir: PathBuf,
    pub width: u32,
    pub height: u32,
    pub bytes_per_row: u32,
    pub pixels: Vec<u8>,
    pub format: wgpu::TextureFormat,
    pub frames: Vec<Option<DisplayFrame>>,
    pub transfer: Option<TransferFrame>,
}

pub fn spawn_save(req: ScreenshotRequest) {
    thread::spawn(move || {
        if let Err(e) = save(req) {
            log::error!("save failed: {e}");
        }
    });
}

fn save(req: ScreenshotRequest) -> anyhow::Result<()> {
    std::fs::create_dir_all(&req.output_dir)?;
    let stamp = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    let prefix = if req.transfer.is_some() { "transfer" } else { "spectrum" };
    let png_path = req.output_dir.join(format!("{prefix}_{stamp}.png"));
    let csv_path = req.output_dir.join(format!("{prefix}_{stamp}.csv"));

    let rgba = unpad(&req.pixels, req.width, req.height, req.bytes_per_row);
    let rgba = channel_swap_if_needed(rgba, req.format);

    let img = image::RgbaImage::from_raw(req.width, req.height, rgba)
        .ok_or_else(|| anyhow::anyhow!("image buffer size mismatch"))?;
    img.save(&png_path)?;

    if let Some(tf) = req.transfer.as_ref() {
        write_transfer_csv(&csv_path, tf)?;
    } else {
        write_csv(&csv_path, &req.frames)?;
    }
    log::info!("saved {} and {}", png_path.display(), csv_path.display());
    Ok(())
}

fn unpad(padded: &[u8], width: u32, height: u32, bytes_per_row: u32) -> Vec<u8> {
    let row_bytes = (width * 4) as usize;
    let mut out = Vec::with_capacity(row_bytes * height as usize);
    for y in 0..height as usize {
        let start = y * bytes_per_row as usize;
        out.extend_from_slice(&padded[start..start + row_bytes]);
    }
    out
}

fn channel_swap_if_needed(mut rgba: Vec<u8>, format: wgpu::TextureFormat) -> Vec<u8> {
    if matches!(
        format,
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
    ) {
        for px in rgba.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
    }
    rgba
}

fn write_csv(path: &std::path::Path, frames: &[Option<DisplayFrame>]) -> anyhow::Result<()> {
    use std::io::Write;

    let active: Vec<&DisplayFrame> = frames.iter().flatten().collect();
    if active.is_empty() {
        let mut f = std::fs::File::create(path)?;
        writeln!(f, "freq_hz")?;
        return Ok(());
    }

    let n = active.iter().map(|f| f.freqs.len()).min().unwrap_or(0);
    let mut f = std::fs::File::create(path)?;

    // Metadata header (`# `-prefixed lines, skipped by Pandas /
    // numpy.loadtxt with default settings). Records the cal layers and
    // processing-context that were active at capture time so a re-load
    // years later can interpret the dB values correctly. See #100.
    let stamp = chrono::Utc::now().to_rfc3339();
    writeln!(f, "# ac monitor export — {stamp}")?;
    if let Some(first) = active.first() {
        writeln!(f, "# sample_rate_hz: {}", first.meta.sr)?;
    }
    for (i, frame) in active.iter().enumerate() {
        let mc = frame.meta.mic_correction.as_deref().unwrap_or("none");
        let spl = frame.meta.spl_offset_db
            .map(|v| format!("{v:.2}"))
            .unwrap_or_else(|| "null".into());
        let dbu = frame.meta.in_dbu
            .map(|v| format!("{v:+.2}"))
            .unwrap_or_else(|| "null".into());
        writeln!(
            f,
            "# ch{i}: mic_correction={mc} spl_offset_db={spl} in_dbu={dbu}"
        )?;
    }

    // Data header. Per channel: dBFS magnitude + mic_corrected flag so a
    // downstream tool can reconstruct whether each column reflects a
    // mic-corrected reading.
    let mut header_cols: Vec<String> = vec!["freq_hz".to_string()];
    for i in 0..active.len() {
        header_cols.push(format!("ch{i}_dbfs"));
        header_cols.push(format!("ch{i}_mic_corrected"));
    }
    writeln!(f, "{}", header_cols.join(","))?;
    for i in 0..n {
        let freq = active[0].freqs[i];
        let mut row = format!("{:.3}", freq);
        for frame in &active {
            let v = frame.spectrum.get(i).copied().unwrap_or(-140.0);
            let mc = frame.meta.mic_correction.as_deref() == Some("on");
            row.push(',');
            row.push_str(&format!("{v:.3}"));
            row.push(',');
            row.push_str(if mc { "true" } else { "false" });
        }
        writeln!(f, "{}", row)?;
    }
    Ok(())
}

fn write_transfer_csv(path: &std::path::Path, tf: &TransferFrame) -> anyhow::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    writeln!(
        f,
        "# delay_ms={:.3} delay_samples={} meas_ch={} ref_ch={} sr={}",
        tf.delay_ms, tf.delay_samples, tf.meas_channel, tf.ref_channel, tf.sr,
    )?;
    writeln!(f, "freq_hz,magnitude_db,phase_deg,coherence")?;
    let n = tf
        .freqs
        .len()
        .min(tf.magnitude_db.len())
        .min(tf.phase_deg.len())
        .min(tf.coherence.len());
    for i in 0..n {
        writeln!(
            f,
            "{:.3},{:.3},{:.3},{:.4}",
            tf.freqs[i], tf.magnitude_db[i], tf.phase_deg[i], tf.coherence[i],
        )?;
    }
    Ok(())
}

pub fn bytes_per_row_aligned(width: u32) -> u32 {
    let unaligned = width * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    unaligned.div_ceil(align) * align
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::types::FrameMeta;
    use std::sync::Arc;

    fn frame(
        sr:             u32,
        spl:            Option<f32>,
        mic_correction: Option<&str>,
        in_dbu:         Option<f32>,
    ) -> DisplayFrame {
        DisplayFrame {
            spectrum: Arc::new(vec![-12.0, -18.0, -24.0]),
            freqs:    Arc::new(vec![100.0, 1000.0, 10000.0]),
            meta: FrameMeta {
                freq_hz:          1000.0,
                fundamental_dbfs: -12.0,
                thd_pct:          0.01,
                thdn_pct:         0.02,
                in_dbu,
                dbu_offset_db:    None,
                peaks:            Arc::new(Vec::new()),
                spl_offset_db:    spl,
                mic_correction:   mic_correction.map(str::to_string),
                sr,
                clipping:         false,
                xruns:            0,
                leq_duration_s:   None,
            },
            new_row: None,
        }
    }

    #[test]
    fn csv_header_records_cal_context_per_channel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("export.csv");
        let frames = vec![
            Some(frame(48000, Some(114.0), Some("on"),  Some(4.0))),
            Some(frame(48000, None,        Some("none"), None)),
        ];
        write_csv(&path, &frames).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        // Comment-prefixed metadata block.
        assert!(body.starts_with("# ac monitor export"));
        assert!(body.contains("# sample_rate_hz: 48000"));
        assert!(body.contains("# ch0: mic_correction=on spl_offset_db=114.00 in_dbu=+4.00"),
            "ch0 metadata line missing: {body}");
        assert!(body.contains("# ch1: mic_correction=none spl_offset_db=null in_dbu=null"),
            "ch1 metadata line missing: {body}");
    }

    #[test]
    fn csv_data_columns_include_mic_corrected_flag_per_channel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("export.csv");
        let frames = vec![
            Some(frame(48000, Some(100.0), Some("on"),   None)),
            Some(frame(48000, None,        Some("off"),  None)),
            Some(frame(48000, None,        None,         None)),
        ];
        write_csv(&path, &frames).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        // Data header: freq + (dbfs, mic_corrected) per channel.
        assert!(body.contains("freq_hz,ch0_dbfs,ch0_mic_corrected,ch1_dbfs,ch1_mic_corrected,ch2_dbfs,ch2_mic_corrected"),
            "data header malformed: {body}");
        // First data row: ch0 mic-corrected (on → true), ch1 off → false,
        // ch2 no curve → false.
        let data_lines: Vec<&str> = body.lines().filter(|l| !l.starts_with('#')).collect();
        assert!(data_lines.len() >= 2, "want header + ≥1 row, got: {data_lines:?}");
        let first_row = data_lines[1];
        assert!(first_row.contains(",true,"),  "ch0 should be mic-corrected: {first_row}");
        assert!(first_row.contains(",false,"), "ch1/ch2 should not: {first_row}");
    }

    #[test]
    fn csv_with_no_active_frames_is_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("export.csv");
        let frames: Vec<Option<DisplayFrame>> = vec![None, None];
        write_csv(&path, &frames).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.trim(), "freq_hz");
    }
}
