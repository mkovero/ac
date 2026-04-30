// Ember substrate — deposition pass.
//
// One vertex per audio sample. Vertex `xy` is in [0,1] cell-local space, where
// (0,0) is the bottom-left corner of the substrate texture. Drawn as point
// primitives with additive blending so overlapping deposits accumulate the
// per-sample intensity into the persistent luminance buffer.

struct Params {
    intensity: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

@group(0) @binding(0) var<uniform> params: Params;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
}

@vertex
fn vs_main(@location(0) xy: vec2<f32>) -> VsOut {
    var out: VsOut;
    out.pos = vec4(xy.x * 2.0 - 1.0, xy.y * 2.0 - 1.0, 0.0, 1.0);
    return out;
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4(params.intensity, 0.0, 0.0, 1.0);
}
