// Fire / dark-red high-contrast palette. Background is pure black; channel
// hues sweep ember → vermilion → amber → pale gold so overlapping traces
// stay distinguishable while the overall look stays in-family. Chrome
// (grid lines, axis labels, status text) is deliberately muted so the
// spectrum traces own the foreground and there is minimum competing light.
pub const BG: [f32; 4] = [0.000, 0.000, 0.000, 1.0];
pub const GRID_LABEL: [u8; 3] = [0x60, 0x3C, 0x30];
pub const TEXT: [u8; 3] = [0xC8, 0xA0, 0x70];
pub const CLIP_LED: [u8; 3] = [0xFF, 0x3A, 0x1C];
pub const SELECT_BORDER: [u8; 3] = [0xFF, 0xC8, 0x3A];

pub const CHANNEL_COLORS: [[f32; 4]; 10] = [
    rgb(0xFF, 0xC8, 0x3A), // bright gold
    rgb(0xFF, 0x6A, 0x00), // hot orange
    rgb(0xE6, 0x24, 0x2B), // vermilion red
    rgb(0xFF, 0xA6, 0x3D), // amber
    rgb(0xA8, 0x11, 0x20), // blood red
    rgb(0xFF, 0x8A, 0x5C), // coral
    rgb(0xFF, 0xE0, 0x7A), // pale gold
    rgb(0xD6, 0x4B, 0x1A), // burnt orange
    rgb(0x7A, 0x04, 0x0E), // deep ember
    rgb(0xFF, 0x4E, 0x33), // flame
];

const fn rgb(r: u8, g: u8, b: u8) -> [f32; 4] {
    [
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        1.0,
    ]
}

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

pub const GRID_LABEL_PX: f32 = 13.0;
pub const STATUS_PX: f32 = 14.0;
pub const READOUT_PX: f32 = 15.0;

pub const DEFAULT_DB_MIN: f32 = -180.0;
pub const DEFAULT_DB_MAX: f32 = 0.0;
pub const DEFAULT_FREQ_MIN: f32 = 20.0;
pub const DEFAULT_FREQ_MAX: f32 = 24000.0;
