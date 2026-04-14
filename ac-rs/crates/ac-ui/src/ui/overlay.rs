use egui::{Align2, Color32, Context, FontId, Pos2};

use crate::data::types::{DisplayConfig, DisplayFrame, LayoutMode};
use crate::theme;

pub struct OverlayInput<'a> {
    pub config: &'a DisplayConfig,
    pub frames: &'a [Option<DisplayFrame>],
    pub connected: bool,
    pub notification: Option<&'a str>,
}

pub fn draw(ctx: &Context, input: OverlayInput<'_>) {
    let screen = ctx.screen_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("ac-ui-overlay"),
    ));

    let text_color = Color32::from_rgb(theme::TEXT[0], theme::TEXT[1], theme::TEXT[2]);
    let clip_color = Color32::from_rgb(
        theme::CLIP_LED[0],
        theme::CLIP_LED[1],
        theme::CLIP_LED[2],
    );

    let display_ch = match input.config.layout {
        LayoutMode::Single => input.config.active_channel,
        _ => 0,
    };

    let primary = input.frames.get(display_ch).and_then(|f| f.as_ref());
    let anyclip = input.frames.iter().flatten().any(|f| f.meta.clipping);

    if let Some(frame) = primary {
        let sr = frame.meta.sr;
        let top_right = format!("{} Hz │ CH{}", sr, display_ch);
        painter.text(
            Pos2::new(screen.right() - 8.0, screen.top() + 6.0),
            Align2::RIGHT_TOP,
            top_right,
            FontId::monospace(theme::STATUS_PX),
            text_color,
        );

        let dbu = frame
            .meta
            .in_dbu
            .map(|v| format!("   {:+.1} dBu", v))
            .unwrap_or_default();
        let bottom_left = format!(
            "{:>7.1} Hz   {:>6.1} dBFS   THD {:.3}%   THD+N {:.3}%{}",
            frame.meta.freq_hz,
            frame.meta.fundamental_dbfs,
            frame.meta.thd_pct,
            frame.meta.thdn_pct,
            dbu,
        );
        painter.text(
            Pos2::new(screen.left() + 8.0, screen.bottom() - 6.0),
            Align2::LEFT_BOTTOM,
            bottom_left,
            FontId::monospace(theme::READOUT_PX),
            text_color,
        );
    }

    let conn_label = if input.connected {
        ("● connected", text_color)
    } else {
        ("● disconnected", clip_color)
    };
    painter.text(
        Pos2::new(screen.right() - 8.0, screen.bottom() - 6.0),
        Align2::RIGHT_BOTTOM,
        conn_label.0,
        FontId::monospace(theme::STATUS_PX),
        conn_label.1,
    );

    if anyclip {
        painter.text(
            Pos2::new(screen.center().x, screen.top() + 6.0),
            Align2::CENTER_TOP,
            "CLIP",
            FontId::monospace(theme::READOUT_PX),
            clip_color,
        );
    }

    if input.config.frozen {
        painter.text(
            Pos2::new(screen.center().x, screen.top() + 22.0),
            Align2::CENTER_TOP,
            "FROZEN",
            FontId::monospace(theme::STATUS_PX),
            text_color,
        );
    }

    if let Some(note) = input.notification {
        painter.text(
            screen.center(),
            Align2::CENTER_CENTER,
            note,
            FontId::monospace(theme::READOUT_PX),
            text_color,
        );
    }
}
