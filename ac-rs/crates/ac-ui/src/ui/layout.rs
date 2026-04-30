use crate::data::types::LayoutMode;

#[derive(Debug, Clone, Copy)]
pub struct CellRect {
    pub channel: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// User-controlled grid sizing. `cell_size` is the target cell width as a
/// fraction of the plot area width. `None` means "auto" (sqrt-based squarish
/// layout, everything fits on one page). `page` indexes into the stride of
/// cell_size × derived rows when more channels exist than fit on one page.
#[derive(Debug, Clone, Copy, Default)]
pub struct GridParams {
    pub cell_size: Option<f32>,
    pub page:      usize,
}

/// Plot inset padding. Kept in one place so `compute` and `grid_page_count`
/// both agree on cell sizing math.
const PAD_LEFT:  f32 = 0.055;
const PAD_RIGHT: f32 = 0.025;
const PAD_TOP:   f32 = 0.05;
const PAD_BOT:   f32 = 0.10;

/// Resolve (cols, rows, page_size, pages) for the current grid params. Used
/// by `compute(Grid)` and by the app when pinning `grid_page` after a resize.
pub fn grid_dims(n_channels: usize, params: GridParams) -> (usize, usize, usize, usize) {
    if n_channels == 0 {
        return (1, 1, 1, 1);
    }
    let plot_w = 1.0 - PAD_LEFT - PAD_RIGHT;
    let plot_h = 1.0 - PAD_TOP - PAD_BOT;
    let (cols, rows) = match params.cell_size {
        Some(cs) => {
            let cs = cs.clamp(0.08, 1.0);
            let cols = (1.0 / cs).round().max(1.0) as usize;
            let cell_w = plot_w / cols as f32;
            let rows = (plot_h / cell_w.max(1e-4)).round().max(1.0) as usize;
            (cols, rows)
        }
        None => {
            let cols = (n_channels as f32).sqrt().ceil().max(1.0) as usize;
            let rows = n_channels.div_ceil(cols.max(1)).max(1);
            (cols, rows)
        }
    };
    let page_size = (cols * rows).max(1);
    let pages = n_channels.div_ceil(page_size).max(1);
    (cols, rows, page_size, pages)
}

pub fn compute(
    mode: LayoutMode,
    n_channels: usize,
    active_channel: usize,
    selected: &[bool],
    grid: GridParams,
) -> Vec<CellRect> {
    if n_channels == 0 {
        return Vec::new();
    }
    let plot_x = PAD_LEFT;
    let plot_y = PAD_BOT;
    let plot_w = 1.0 - PAD_LEFT - PAD_RIGHT;
    let plot_h = 1.0 - PAD_TOP - PAD_BOT;

    match mode {
        LayoutMode::Compare => (0..n_channels)
            .filter(|i| selected.get(*i).copied().unwrap_or(false))
            .map(|i| CellRect {
                channel: i,
                x: plot_x,
                y: plot_y,
                w: plot_w,
                h: plot_h,
            })
            .collect(),
        LayoutMode::Sweep => {
            vec![CellRect {
                channel: 0,
                x: plot_x,
                y: plot_y,
                w: plot_w,
                h: plot_h,
            }]
        }
        LayoutMode::Single => {
            let target = active_channel.min(n_channels - 1);
            vec![CellRect {
                channel: target,
                x: plot_x,
                y: plot_y,
                w: plot_w,
                h: plot_h,
            }]
        }
        LayoutMode::Grid => {
            let (cols, rows, page_size, pages) = grid_dims(n_channels, grid);
            let page = grid.page % pages.max(1);
            let start = page * page_size;
            let end = (start + page_size).min(n_channels);
            let cell_w = plot_w / cols as f32;
            let cell_h = plot_h / rows as f32;
            let gap_x = cell_w * 0.08;
            let gap_y = cell_h * 0.10;
            (start..end)
                .map(|i| {
                    let local = i - start;
                    let col = local % cols;
                    let row = local / cols;
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

#[cfg(test)]
mod tests {
    //! Visibility set tests. The render pipeline gates per-channel
    //! preprocessing (smoothing, peak/min hold) on
    //! `cells.iter().map(|c| c.channel)` (#111). These tests pin which
    //! channels each layout exposes so that filter set is auditable
    //! without touching the wgpu/egui-bound render pipeline itself.
    use super::*;
    use std::collections::HashSet;

    fn visible(cells: &[CellRect]) -> HashSet<usize> {
        cells.iter().map(|c| c.channel).collect()
    }

    #[test]
    fn single_layout_exposes_only_active_channel() {
        let cells = compute(
            LayoutMode::Single,
            8,
            3,
            &[false; 8],
            GridParams::default(),
        );
        assert_eq!(visible(&cells), HashSet::from([3]));
    }

    #[test]
    fn compare_layout_exposes_only_selected() {
        let mut sel = vec![false; 8];
        sel[1] = true;
        sel[5] = true;
        let cells = compute(LayoutMode::Compare, 8, 0, &sel, GridParams::default());
        assert_eq!(visible(&cells), HashSet::from([1, 5]));
    }

    #[test]
    fn sweep_layout_exposes_only_channel_zero() {
        let cells = compute(
            LayoutMode::Sweep,
            8,
            5,
            &[false; 8],
            GridParams::default(),
        );
        assert_eq!(visible(&cells), HashSet::from([0]));
    }

    #[test]
    fn grid_layout_pages_change_visible_set() {
        // 8 channels, force a 2×2 = 4-cell page so paging splits 0..4
        // and 4..8 onto separate pages.
        let small_cell = GridParams { cell_size: Some(0.5), page: 0 };
        let page0 = compute(LayoutMode::Grid, 8, 0, &[false; 8], small_cell);
        let page1 = compute(
            LayoutMode::Grid,
            8,
            0,
            &[false; 8],
            GridParams { page: 1, ..small_cell },
        );
        let v0 = visible(&page0);
        let v1 = visible(&page1);
        assert!(!v0.is_empty(), "page 0 must show at least one cell");
        assert!(!v1.is_empty(), "page 1 must show at least one cell");
        assert!(v0.is_disjoint(&v1), "different pages must show different channels");
        assert_eq!(v0.union(&v1).count(), 8, "all 8 channels covered across pages");
    }

    #[test]
    fn grid_layout_handles_short_selected_slice() {
        // Compare layout reads `selected.get(i).copied().unwrap_or(false)`,
        // so a `&selected` shorter than n_channels must not panic — the
        // visibility-gated render pipeline computes `cells` before the
        // `selected.resize(n_total, false)` call (#111), so this is the
        // path it actually exercises.
        let sel: Vec<bool> = vec![true]; // only channel 0 selected
        let cells = compute(LayoutMode::Compare, 4, 0, &sel, GridParams::default());
        assert_eq!(visible(&cells), HashSet::from([0]));
    }
}
