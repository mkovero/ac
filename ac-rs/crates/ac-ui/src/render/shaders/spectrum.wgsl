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
@group(0) @binding(1) var<storage, read> freq_data: array<f32>;
@group(0) @binding(2) var<storage, read> channels: array<ChannelMeta>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) edge: f32,
    @location(2) top_frac: f32,
}

const LN10: f32 = 2.302585;
const DEFAULT_LINE_HALF_W: f32 = 0.0018;
const DEFAULT_FILL_ALPHA: f32 = 0.15;

fn safe_idx(offset: u32, bin: u32, n_bins: u32) -> u32 {
    let b = min(bin, n_bins - 1u);
    return offset + b;
}

fn bin_point(m: ChannelMeta, bin: u32) -> vec2<f32> {
    let idx = safe_idx(m.offset, bin, m.n_bins);
    let freq = max(freq_data[idx], 1.0);
    let mag = spectrum_data[idx];
    let freq_span = max(m.freq_log_max - m.freq_log_min, 0.0001);
    let db_span = max(m.db_max - m.db_min, 0.0001);
    let x_n = (log(freq) / LN10 - m.freq_log_min) / freq_span;
    let y_n = (mag - m.db_min) / db_span;
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
    let side = f32(vid % 2u) * 2.0 - 1.0;
    let p = bin_point(m, bin);
    let lw = select(DEFAULT_LINE_HALF_W, m.line_width, m.line_width > 0.0);
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
    var y: f32;
    var frac: f32;
    if (top) {
        y = p.y;
        frac = 1.0;
    } else {
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
