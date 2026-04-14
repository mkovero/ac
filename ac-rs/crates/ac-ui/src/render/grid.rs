use egui::{Color32, Painter, Pos2, Rect, Stroke};

use crate::data::types::DisplayConfig;
use crate::theme;

pub fn draw_grid(
    painter: &Painter,
    rect: Rect,
    config: &DisplayConfig,
    show_labels: bool,
) {
    let stroke = Stroke::new(
        1.0,
        Color32::from_rgba_unmultiplied(255, 255, 255, (0.08 * 255.0) as u8),
    );
    let label_color = Color32::from_rgb(
        theme::GRID_LABEL[0],
        theme::GRID_LABEL[1],
        theme::GRID_LABEL[2],
    );

    let log_min = config.freq_min.max(1.0).log10();
    let log_max = config.freq_max.max(log_min.exp().max(1.1)).log10();
    let span = (log_max - log_min).max(0.0001);

    for &f in theme::DECADE_FREQS {
        if f < config.freq_min || f > config.freq_max {
            continue;
        }
        let t = (f.log10() - log_min) / span;
        let x = rect.left() + t * rect.width();
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            stroke,
        );
        if show_labels {
            let label = if f >= 1000.0 {
                format!("{:.0}k", f / 1000.0)
            } else {
                format!("{:.0}", f)
            };
            painter.text(
                Pos2::new(x + 2.0, rect.bottom() - 2.0),
                egui::Align2::LEFT_BOTTOM,
                label,
                egui::FontId::monospace(theme::GRID_LABEL_PX),
                label_color,
            );
        }
    }

    let db_step = 20.0_f32;
    let db_span = (config.db_max - config.db_min).max(0.0001);
    let mut db = (config.db_min / db_step).ceil() * db_step;
    while db <= config.db_max + 0.001 {
        let t = (db - config.db_min) / db_span;
        let y = rect.bottom() - t * rect.height();
        painter.line_segment(
            [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
            stroke,
        );
        if show_labels {
            painter.text(
                Pos2::new(rect.left() + 2.0, y - 2.0),
                egui::Align2::LEFT_BOTTOM,
                format!("{:.0}", db),
                egui::FontId::monospace(theme::GRID_LABEL_PX),
                label_color,
            );
        }
        db += db_step;
    }
}
