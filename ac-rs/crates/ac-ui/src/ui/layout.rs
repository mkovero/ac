use crate::data::types::LayoutMode;

#[derive(Debug, Clone, Copy)]
pub struct CellRect {
    pub channel: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

pub fn compute(
    mode: LayoutMode,
    n_channels: usize,
    active_channel: usize,
) -> Vec<CellRect> {
    if n_channels == 0 {
        return Vec::new();
    }
    let pad_left = 0.055_f32;
    let pad_right = 0.025_f32;
    let pad_y_top = 0.05_f32;
    let pad_y_bot = 0.10_f32;
    let plot_x = pad_left;
    let plot_y = pad_y_bot;
    let plot_w = 1.0 - pad_left - pad_right;
    let plot_h = 1.0 - pad_y_top - pad_y_bot;

    match mode {
        LayoutMode::Overlay | LayoutMode::Single => {
            let target = if mode == LayoutMode::Single {
                active_channel.min(n_channels - 1)
            } else {
                usize::MAX
            };
            (0..n_channels)
                .filter(|i| mode == LayoutMode::Overlay || *i == target)
                .map(|i| CellRect {
                    channel: i,
                    x: plot_x,
                    y: plot_y,
                    w: plot_w,
                    h: plot_h,
                })
                .collect()
        }
        LayoutMode::Grid => {
            let cols = (n_channels as f32).sqrt().ceil() as usize;
            let rows = n_channels.div_ceil(cols);
            let cell_w = plot_w / cols as f32;
            let cell_h = plot_h / rows as f32;
            let gap_x = cell_w * 0.08;
            let gap_y = cell_h * 0.10;
            (0..n_channels)
                .map(|i| {
                    let col = i % cols;
                    let row = i / cols;
                    let x = plot_x + col as f32 * cell_w + gap_x * 0.5;
                    let y = plot_y + (rows - 1 - row) as f32 * cell_h + gap_y * 0.5;
                    CellRect {
                        channel: i,
                        x,
                        y,
                        w: cell_w - gap_x,
                        h: cell_h - gap_y,
                    }
                })
                .collect()
        }
    }
}

pub fn to_pixel_rect(cell: &CellRect, width: f32, height: f32) -> egui::Rect {
    let x0 = cell.x * width;
    let x1 = (cell.x + cell.w) * width;
    let y1 = height - cell.y * height;
    let y0 = height - (cell.y + cell.h) * height;
    egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1))
}
