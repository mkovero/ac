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
    let header: Vec<String> = std::iter::once("freq_hz".to_string())
        .chain(active.iter().enumerate().map(|(i, _)| format!("ch{i}_dbfs")))
        .collect();
    writeln!(f, "{}", header.join(","))?;
    for i in 0..n {
        let freq = active[0].freqs[i];
        let mut row = format!("{:.3}", freq);
        for frame in &active {
            let v = frame.spectrum.get(i).copied().unwrap_or(-140.0);
            row.push(',');
            row.push_str(&format!("{:.3}", v));
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
