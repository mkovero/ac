use std::env;
use std::fs;
use std::path::PathBuf;

// Bakes two 256-entry RGBA8 colormap LUTs (stacked vertically as a
// 256-wide × N_PALETTES-tall texture) for the waterfall renderer into
// $OUT_DIR/colormap.bin. Coefficients are Iñigo Quilez' polynomial fits of
// the Matplotlib perceptual palettes (https://www.shadertoy.com/view/WlfXRN).
// Row 0 = inferno, row 1 = magma. Viridis and plasma are kept in this file
// but excluded from the cycle — they start at coloured backgrounds, which
// visually fights a dark-bg UI. Re-add them by extending the slice below
// and keeping `PALETTE_NAMES` in `render/waterfall.rs` in sync.
// `PALETTE_NAMES` in `render/waterfall.rs` must agree with this ordering.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR set by cargo"));
    let lut_path = out_dir.join("colormap.bin");

    let palettes: &[fn(f32) -> (f32, f32, f32)] = &[inferno, magma];
    let mut bytes = Vec::with_capacity(256 * 4 * palettes.len());
    for palette in palettes {
        for i in 0..256u32 {
            let t = i as f32 / 255.0;
            let (r, g, b) = palette(t);
            bytes.push(to_u8(r));
            bytes.push(to_u8(g));
            bytes.push(to_u8(b));
            bytes.push(255);
        }
    }
    fs::write(&lut_path, &bytes).expect("write colormap lut");
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

fn poly(
    t: f32,
    c0: f32, c1: f32, c2: f32, c3: f32, c4: f32, c5: f32, c6: f32,
) -> f32 {
    c0 + t * (c1 + t * (c2 + t * (c3 + t * (c4 + t * (c5 + t * c6)))))
}

fn inferno(t: f32) -> (f32, f32, f32) {
    let r = poly(t,  0.000_218_940_37,  0.106_513_42,  11.602_733,  -41.703_995,   77.162_94,  -71.319_43,  25.131_126);
    let g = poly(t,  0.001_651_004_6,   0.563_956_44,  -3.972_877_7, 17.436_398,  -33.402_36,   32.626_064, -12.242_669);
    let b = poly(t, -0.019_480_378,     3.932_712_4,  -15.942_394,   44.354_145,  -81.807_31,   73.209_52,  -23.070_326);
    (r, g, b)
}

#[allow(dead_code)]
fn viridis(t: f32) -> (f32, f32, f32) {
    let r = poly(t,  0.277_727_38, -0.170_349_75,   2.422_137,  -12.157_051,   24.337_53,  -23.084_894,   8.589_299);
    let g = poly(t,  0.005_408_25,  1.404_613_7,    0.229_481_1, -1.014_227_7,  -0.404_263_3,  2.251_829_7, -1.229_558_6);
    let b = poly(t,  0.334_339_9,   1.384_177_9,    0.091_571_6, -9.069_5,       15.869_45,  -12.008_73,    4.106_604);
    (r, g, b)
}

fn magma(t: f32) -> (f32, f32, f32) {
    let r = poly(t, -0.002_136_485_3,  0.251_164_8,   8.353_717,  -27.669_6,    52.174_767, -51.888_48,  19.869_11);
    let g = poly(t, -0.000_749_655_4,  0.671_631_4,  -3.938_091_4, 15.840_65,  -27.938_18,   22.852_04,  -6.991_3);
    let b = poly(t, -0.005_386_131_8,  1.510_029_6,   0.314_058_08, 8.887_853, -27.340_91,   25.466_44,  -8.565_88);
    (r, g, b)
}

#[allow(dead_code)]
fn plasma(t: f32) -> (f32, f32, f32) {
    let r = poly(t,  0.054_342_46,    2.185_649_8,    0.233_402_98, -8.941_35,     14.197_80,  -9.829_91,    2.691_373);
    let g = poly(t,  0.023_638_08,    0.295_460_1,   -1.907_598,    2.592_914,     0.441_583_2, -1.415_167,   0.540_528);
    let b = poly(t,  0.538_132_8,     1.404_854_1,    0.333_049_25, -5.576_141,     6.690_805,  -3.115_221,    0.321_145_6);
    (r, g, b)
}
