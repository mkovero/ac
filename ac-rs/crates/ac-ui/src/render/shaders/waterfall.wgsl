struct WaterfallCell {
    viewport:     vec4<f32>,
    db_min:       f32,
    db_max:       f32,
    freq_log_min: f32,
    freq_log_max: f32,
    n_bins:       u32,
    n_rows:       u32,
    write_row:    u32,
    layer:        u32,
    freq_first:   f32,
    freq_last:    f32,
    log_spaced:   u32,
    rows_visible: f32,
}

const LN10: f32 = 2.302585;

@group(0) @binding(0) var<storage, read> cells: array<WaterfallCell>;
@group(0) @binding(1) var history: texture_2d_array<f32>;
@group(0) @binding(2) var lut:     texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) @interpolate(flat) cell_idx: u32,
}

@vertex
fn vs_main(@builtin(vertex_index) vid: u32, @builtin(instance_index) inst: u32) -> VsOut {
    // Triangle strip: (0,0) (1,0) (0,1) (1,1)
    let u = f32(vid & 1u);
    let v = f32((vid >> 1u) & 1u);
    let m = cells[inst];
    let x_px = m.viewport.x + u * m.viewport.z;
    let y_px = m.viewport.y + v * m.viewport.w;
    var out: VsOut;
    out.pos = vec4(x_px * 2.0 - 1.0, y_px * 2.0 - 1.0, 0.0, 1.0);
    // u: 0→left=low freq, 1→right=high freq
    // v: 0→bottom=oldest, 1→top=newest (matches the rest of the UI: y up)
    out.uv = vec2(u, v);
    out.cell_idx = inst;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let m = cells[in.cell_idx];
    if (m.n_bins == 0u || m.n_rows == 0u) {
        return vec4(0.0, 0.0, 0.0, 1.0);
    }

    // Remap screen u into a bin index using the current view window
    // [freq_log_min, freq_log_max] so mouse-scroll zoom actually narrows the
    // visible bandwidth. Two cases: log-spaced bins (synthetic) use direct
    // log interpolation; linear-spaced bins (real FFT output) need to go
    // back through 10^target to pick a bin by frequency.
    let u = clamp(in.uv.x, 0.0, 1.0);
    let target_log = m.freq_log_min + u * (m.freq_log_max - m.freq_log_min);
    var bin_f: f32;
    if (m.log_spaced != 0u) {
        let lo = log(max(m.freq_first, 1.0)) / LN10;
        let hi = log(max(m.freq_last, 10.0)) / LN10;
        let data_span = max(hi - lo, 0.0001);
        bin_f = (target_log - lo) / data_span * f32(m.n_bins - 1u);
    } else {
        let target_freq = exp(target_log * LN10);
        let data_span = max(m.freq_last - m.freq_first, 0.001);
        bin_f = (target_freq - m.freq_first) / data_span * f32(m.n_bins - 1u);
    }
    // Zoomed past the data edges → render background instead of clamped
    // edge-bin colors so the user can see the actual data boundary.
    if (bin_f < 0.0 || bin_f > f32(m.n_bins - 1u)) {
        return vec4(0.0, 0.0, 0.0, 1.0);
    }

    // Newest row sits at the top (uv.y = 1). `rows_visible` caps how deep into
    // the ring we look — the user Ctrl+scrolls to shrink this and "zoom time"
    // into a narrower recent window. Fractional so continuous scroll zoom
    // slides content by a fraction of a row per tick instead of snapping to
    // integer boundaries (which otherwise causes the labels — which track
    // the float — to drift away from the texture).
    let rows_shown = max(min(m.rows_visible, f32(m.n_rows)), 1.0);
    let rows_back_f = (1.0 - clamp(in.uv.y, 0.0, 1.0)) * (rows_shown - 1.0);
    let newest = (m.write_row + m.n_rows - 1u) % m.n_rows;

    // Bilinear interpolation across both axes so the colormap transitions
    // smoothly between adjacent bins and rows instead of snapping to the
    // nearest integer (which causes visible blocks at low bin counts).
    let bin_lo = u32(floor(bin_f));
    let bin_hi = min(bin_lo + 1u, m.n_bins - 1u);
    let bf = fract(bin_f);

    let rb_lo = u32(floor(rows_back_f));
    let rb_hi = min(rb_lo + 1u, u32(max(rows_shown - 1.0, 0.0)));
    let rf = fract(rows_back_f);

    let row_a = (newest + m.n_rows - rb_lo) % m.n_rows;
    let row_b = (newest + m.n_rows - rb_hi) % m.n_rows;

    let m00 = textureLoad(history, vec2<i32>(i32(bin_lo), i32(row_a)), i32(m.layer), 0).r;
    let m10 = textureLoad(history, vec2<i32>(i32(bin_hi), i32(row_a)), i32(m.layer), 0).r;
    let m01 = textureLoad(history, vec2<i32>(i32(bin_lo), i32(row_b)), i32(m.layer), 0).r;
    let m11 = textureLoad(history, vec2<i32>(i32(bin_hi), i32(row_b)), i32(m.layer), 0).r;

    let mag = mix(mix(m00, m10, bf), mix(m01, m11, bf), rf);
    let span = max(m.db_max - m.db_min, 0.0001);
    let t = clamp((mag - m.db_min) / span, 0.0, 1.0);
    let lut_x = i32(t * 255.0 + 0.5);
    let rgba = textureLoad(lut, vec2<i32>(lut_x, 0), 0);
    return vec4(rgba.rgb, 1.0);
}
