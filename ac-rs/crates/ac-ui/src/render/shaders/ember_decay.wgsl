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
    /// (pure decay). >0 = strip-chart: the destination texel at x reads
    /// the source at (x + scroll_dx), so old content moves leftward and
    /// the rightmost band falls off the source domain (returns 0 = fresh).
    scroll_dx: f32,
    _pad0:     f32,
    _pad1:     f32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: Params;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    let u = f32(vid & 1u);
    let v = f32((vid >> 1u) & 1u);
    var out: VsOut;
    out.pos = vec4(u * 2.0 - 1.0, v * 2.0 - 1.0, 0.0, 1.0);
    return out;
}

// Samples `src` at this fragment's own render-target pixel coordinate
// (`@builtin(position)`), not at a separately-computed uv varying. Those two
// are not guaranteed to agree for an offscreen render target — the
// vertex-shader NDC we emit and the rasterizer's actual row assignment for a
// non-presentation target can differ from a naive uv fraction, and did: the
// old uv-based read silently sampled the mirror row (dims.y - 1 - row) on
// every decay pass, so persisted content built up a y-flipped ghost trace
// alongside the correct one (visible as "double ember" once persistence
// accumulated a few frames' worth). `frag_coord` is defined by WGSL to be the
// exact pixel this invocation is writing to, so read-row == write-row always,
// independent of any NDC convention.
@fragment
fn fs_main(@builtin(position) frag_coord: vec4<f32>) -> @location(0) vec4<f32> {
    let dims = textureDimensions(src);
    let scroll_px = params.scroll_dx * f32(dims.x);
    let src_x = i32(frag_coord.x + scroll_px);
    if (src_x < 0 || src_x >= i32(dims.x)) {
        return vec4(0.0, 0.0, 0.0, 1.0);
    }
    let coord = vec2<i32>(src_x, i32(frag_coord.y));
    let l = textureLoad(src, coord, 0).r;
    return vec4(l * params.decay, 0.0, 0.0, 1.0);
}
