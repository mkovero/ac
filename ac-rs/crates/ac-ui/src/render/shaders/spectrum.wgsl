struct ChannelMeta {
    color: vec4<f32>,
    viewport: vec4<f32>,
    db_min: f32,
    db_max: f32,
    freq_log_min: f32,
    freq_log_max: f32,
    n_bins: u32,
    offset: u32,
    fill_alpha: f32,
    line_width: f32,
}

@group(0) @binding(0) var<storage, read> spectrum_data: array<f32>;
@group(0) @binding(1) var<storage, read> channels: array<ChannelMeta>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) edge: f32,
    @location(2) top_frac: f32,
}

const DEFAULT_LINE_HALF_W: f32 = 0.0018;
const DEFAULT_FILL_ALPHA: f32 = 0.15;

fn safe_idx(offset: u32, bin: u32, n_bins: u32) -> u32 {
    let b = min(bin, n_bins - 1u);
    return offset + b;
}

// Bit-pattern NaN test rather than `x != x`: the latter is the standard IEEE
// idiom but some backends compile it away under aggressive fast-math flags,
// silently turning a real gap sentinel (coherence-gated / out-of-range
// transfer columns, see ac-core::visualize::aggregate) into garbage
// geometry. Exponent all-1s + nonzero mantissa is NaN regardless of
// fast-math, because it's an integer comparison after the bitcast.
fn is_nan_bits(x: f32) -> bool {
    let bits = bitcast<u32>(x);
    let exponent = (bits >> 23u) & 0xFFu;
    let mantissa = bits & 0x7FFFFFu;
    return exponent == 0xFFu && mantissa != 0u;
}

fn bin_is_gap(m: ChannelMeta, bin: u32) -> bool {
    let clamped_bin = min(bin, m.n_bins - 1u);
    let idx = m.offset + clamped_bin;
    return is_nan_bits(spectrum_data[idx]);
}

fn bin_point(m: ChannelMeta, bin: u32) -> vec2<f32> {
    let clamped_bin = min(bin, m.n_bins - 1u);
    let idx = m.offset + clamped_bin;
    let mag = spectrum_data[idx];
    let denom = f32(max(m.n_bins - 1u, 1u));
    let x_n = f32(clamped_bin) / denom;
    let db_span = max(m.db_max - m.db_min, 0.0001);
    // NaN (gap sentinel) has no meaningful y — floor it here so the value
    // itself never reaches a float comparison; the vertex functions below
    // separately collapse the geometry to zero area at gap bins so nothing
    // renders at this x position at all.
    let safe_mag = select(mag, m.db_min, is_nan_bits(mag));
    let y_n = (safe_mag - m.db_min) / db_span;
    let x_c = clamp(x_n, 0.0, 1.0);
    let y_c = clamp(y_n, 0.0, 1.0);
    let x = m.viewport.x + x_c * m.viewport.z;
    let y = m.viewport.y + y_c * m.viewport.w;
    return vec2(x, y);
}

fn to_clip(p: vec2<f32>) -> vec4<f32> {
    return vec4(p.x * 2.0 - 1.0, p.y * 2.0 - 1.0, 0.0, 1.0);
}

@vertex
fn vs_line(@builtin(vertex_index) vid: u32, @builtin(instance_index) ch: u32) -> VsOut {
    let m = channels[ch];
    let bin = vid / 2u;
    let side_raw = f32(vid % 2u) * 2.0 - 1.0;
    let p = bin_point(m, bin);
    let lw = select(DEFAULT_LINE_HALF_W, m.line_width, m.line_width > 0.0);
    // Gap bin (NaN magnitude — coherence-gated or out-of-range transfer
    // column): collapse this bin's thickness to zero so the strip pinches
    // to a point here instead of drawing a floor-hugging line at db_min.
    // A contiguous run of gap bins becomes a run of zero-width points, i.e.
    // a true gap; only the single triangle bridging a valid neighbour into
    // the gap tapers rather than hard-cutting.
    let side = select(side_raw, 0.0, bin_is_gap(m, bin));
    let y = p.y + side * lw;
    var out: VsOut;
    out.pos = to_clip(vec2(p.x, y));
    out.color = m.color;
    out.edge = side;
    out.top_frac = 1.0;
    return out;
}

@fragment
fn fs_line(in: VsOut) -> @location(0) vec4<f32> {
    let a = 1.0 - smoothstep(0.55, 1.0, abs(in.edge));
    return vec4(in.color.rgb, in.color.a * a);
}

@vertex
fn vs_fill(@builtin(vertex_index) vid: u32, @builtin(instance_index) ch: u32) -> VsOut {
    let m = channels[ch];
    let bin = vid / 2u;
    let top = (vid % 2u) == 1u;
    let p = bin_point(m, bin);
    let gap = bin_is_gap(m, bin);
    var y: f32;
    var frac: f32;
    if (top && !gap) {
        y = p.y;
        frac = 1.0;
    } else {
        // Gap bin: pin the top vertex to the baseline too, collapsing the
        // fill quad's height to zero here (same rationale as `vs_line`).
        y = m.viewport.y;
        frac = 0.0;
    }
    let fa = select(DEFAULT_FILL_ALPHA, m.fill_alpha, m.fill_alpha > 0.0);
    var out: VsOut;
    out.pos = to_clip(vec2(p.x, y));
    out.color = vec4(m.color.rgb, fa);
    out.edge = 0.0;
    out.top_frac = frac;
    return out;
}

@fragment
fn fs_fill(in: VsOut) -> @location(0) vec4<f32> {
    return vec4(in.color.rgb, in.color.a * in.top_frac);
}
