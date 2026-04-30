// Ember substrate — decay pass.
//
// Reads the previous luminance buffer, multiplies by exp(-Δt/τ), and writes
// to the new luminance buffer. The wgpu API forbids reading and writing the
// same texture in one pass, so the renderer ping-pongs between two R16Float
// targets — `src` is the previous frame's accumulator, the colour attachment
// becomes the next one.

struct Params {
    decay:     f32,
    /// Horizontal scroll, in normalised texture coords. 0 = stationary
    /// (pure decay). >0 = strip-chart: the destination texel at uv.x reads
    /// the source at (uv.x + scroll_dx), so old content moves leftward and
    /// the rightmost band falls off the source domain (returns 0 = fresh).
    scroll_dx: f32,
    _pad0:     f32,
    _pad1:     f32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: Params;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    let u = f32(vid & 1u);
    let v = f32((vid >> 1u) & 1u);
    var out: VsOut;
    out.pos = vec4(u * 2.0 - 1.0, v * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2(u, v);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let src_uv_x = in.uv.x + params.scroll_dx;
    if (src_uv_x >= 1.0 || src_uv_x < 0.0) {
        return vec4(0.0, 0.0, 0.0, 1.0);
    }
    let dims = textureDimensions(src);
    let coord = vec2<i32>(vec2<f32>(src_uv_x, in.uv.y) * vec2<f32>(dims));
    let l = textureLoad(src, coord, 0).r;
    return vec4(l * params.decay, 0.0, 0.0, 1.0);
}
