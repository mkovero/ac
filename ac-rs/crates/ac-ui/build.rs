use std::env;
use std::fs;
use std::path::PathBuf;

// Bakes a 256-entry RGBA8 colormap LUT for the waterfall renderer into
// $OUT_DIR/colormap.bin. Using Matplotlib's inferno polynomial fit (Inigo
// Quilez, https://www.shadertoy.com/view/WlfXRN) to match the fire theme:
// sweeps black → purple → red → orange → pale yellow.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR set by cargo"));
    let lut_path = out_dir.join("colormap.bin");
    let mut bytes = Vec::with_capacity(256 * 4);
    for i in 0..256u32 {
        let t = i as f32 / 255.0;
        let (r, g, b) = inferno(t);
        bytes.push(to_u8(r));
        bytes.push(to_u8(g));
        bytes.push(to_u8(b));
        bytes.push(255);
    }
    fs::write(&lut_path, &bytes).expect("write colormap lut");
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

fn inferno(t: f32) -> (f32, f32, f32) {
    let c0 = ( 0.000_218_940_37,  0.001_651_004_6, -0.019_480_378);
    let c1 = ( 0.106_513_42,      0.563_956_44,     3.932_712_4);
    let c2 = (11.602_733,        -3.972_877_7,    -15.942_394);
    let c3 = (-41.703_995,       17.436_398,       44.354_145);
    let c4 = (77.162_94,        -33.402_36,       -81.807_31);
    let c5 = (-71.319_43,        32.626_064,       73.209_52);
    let c6 = (25.131_126,       -12.242_669,      -23.070_326);
    let poly = |c0_: f32, c1_: f32, c2_: f32, c3_: f32, c4_: f32, c5_: f32, c6_: f32| {
        c0_ + t * (c1_ + t * (c2_ + t * (c3_ + t * (c4_ + t * (c5_ + t * c6_)))))
    };
    (
        poly(c0.0, c1.0, c2.0, c3.0, c4.0, c5.0, c6.0),
        poly(c0.1, c1.1, c2.1, c3.1, c4.1, c5.1, c6.1),
        poly(c0.2, c1.2, c2.2, c3.2, c4.2, c5.2, c6.2),
    )
}
