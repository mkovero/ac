// Ember substrate — display pass.
//
// Samples the persistent luminance buffer, applies a CRT-phosphor tone curve
// (gamma + gain), looks up the result in a 256×N palette LUT, and writes RGB
// to the surface.

struct Params {
    viewport: vec4<f32>,  // (x, y, w, h) in surface-normalised [0,1] coords
    gamma:    f32,
    gain:     f32,
    palette_row: u32,
    _pad:     f32,
}

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var lut: texture_2d<f32>;
@group(0) @binding(2) var<uniform> params: Params;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) vid: u32) -> VsOut {
    let u = f32(vid & 1u);
    let v = f32((vid >> 1u) & 1u);
    let x_px = params.viewport.x + u * params.viewport.z;
    let y_px = params.viewport.y + v * params.viewport.w;
    var out: VsOut;
    out.pos = vec4(x_px * 2.0 - 1.0, y_px * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2(u, v);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let dims = textureDimensions(src);
    let coord = vec2<i32>(in.uv * vec2<f32>(dims));
    let l = textureLoad(src, coord, 0).r;
    let scaled = max(l * params.gain, 0.0);
    let t = clamp(pow(scaled, params.gamma), 0.0, 1.0);
    let lut_x = i32(t * 255.0 + 0.5);
    let rgba = textureLoad(lut, vec2<i32>(lut_x, i32(params.palette_row)), 0);
    return vec4(rgba.rgb, 1.0);
}
