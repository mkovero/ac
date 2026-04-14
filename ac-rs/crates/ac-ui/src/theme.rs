pub const BG: [f32; 4] = [0.039, 0.039, 0.059, 1.0];
pub const GRID_LABEL: [u8; 3] = [0x5A, 0x5A, 0x6A];
pub const TEXT: [u8; 3] = [0xB0, 0xB0, 0xBA];
pub const CLIP_LED: [u8; 3] = [0xDD, 0x33, 0x33];

pub const CHANNEL_COLORS: [[f32; 4]; 10] = [
    [0x4A as f32 / 255.0, 0x9E as f32 / 255.0, 0xAA as f32 / 255.0, 1.0],
    [0xAA as f32 / 255.0, 0x7A as f32 / 255.0, 0x4A as f32 / 255.0, 1.0],
    [0xAA as f32 / 255.0, 0x4A as f32 / 255.0, 0x6A as f32 / 255.0, 1.0],
    [0x6A as f32 / 255.0, 0xAA as f32 / 255.0, 0x4A as f32 / 255.0, 1.0],
    [0x7A as f32 / 255.0, 0x5A as f32 / 255.0, 0xAA as f32 / 255.0, 1.0],
    [0xAA as f32 / 255.0, 0x94 as f32 / 255.0, 0x4A as f32 / 255.0, 1.0],
    [0x4A as f32 / 255.0, 0xAA as f32 / 255.0, 0x7A as f32 / 255.0, 1.0],
    [0xAA as f32 / 255.0, 0x4A as f32 / 255.0, 0x4A as f32 / 255.0, 1.0],
    [0x4A as f32 / 255.0, 0x7A as f32 / 255.0, 0xAA as f32 / 255.0, 1.0],
    [0x8A as f32 / 255.0, 0xAA as f32 / 255.0, 0x4A as f32 / 255.0, 1.0],
];

pub fn channel_color(idx: usize) -> [f32; 4] {
    let base = CHANNEL_COLORS[idx % CHANNEL_COLORS.len()];
    let cycle = idx / CHANNEL_COLORS.len();
    if cycle == 0 {
        return base;
    }
    let shift = 1.0 + (cycle as f32) * 0.15;
    [
        (base[0] * shift).min(1.0),
        (base[1] * shift).min(1.0),
        (base[2] * shift).min(1.0),
        base[3],
    ]
}

pub const GRID_LABEL_PX: f32 = 10.0;
pub const STATUS_PX: f32 = 11.0;
pub const READOUT_PX: f32 = 12.0;

pub const DEFAULT_DB_MIN: f32 = -140.0;
pub const DEFAULT_DB_MAX: f32 = 0.0;
pub const DEFAULT_FREQ_MIN: f32 = 20.0;
pub const DEFAULT_FREQ_MAX: f32 = 24000.0;

pub const DECADE_FREQS: &[f32] = &[
    20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0, 20000.0,
];
