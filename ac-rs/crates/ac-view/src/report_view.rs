//! `ac-view --report <file>` (#665): a window on one `ac plot ir`
//! report, opened by `ac plot ir` itself once the report is written. No
//! daemon, no session — the file is the whole input. The scene comes from
//! [`crate::report_flow::open_sweep_ir`]; this module only paints it.

use std::path::{Path, PathBuf};

use ac_scene::{SweepIrFault, SweepIrScene};

/// The report window: the sweep-derived IR panel over the whole window,
/// or the panel's fault when the file is not a report it can draw.
pub struct ReportView {
    path: PathBuf,
    scene: Result<SweepIrScene, SweepIrFault>,
}

impl ReportView {
    pub fn open(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            scene: crate::report_flow::open_sweep_ir(path),
        }
    }

    /// The window title: the report's own file name.
    pub fn title(&self) -> String {
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string());
        format!("ac-view \u{2014} {name}")
    }

    /// Whether the file gave a drawable scene.
    pub fn is_drawable(&self) -> bool {
        self.scene.is_ok()
    }
}

impl eframe::App for ReportView {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.key_pressed(egui::Key::Escape) || i.key_pressed(egui::Key::Q)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ui.label(format!("{}   (Esc or Q closes)", self.path.display()));
        let rect = ui.available_rect_before_wrap();
        crate::view::draw_sweep_ir_panel(
            ui.painter(),
            rect,
            self.scene.as_ref().map_err(Clone::clone),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A missing file still opens a window, showing the panel's fault
    /// rather than exiting: the operator sees which file it was.
    #[test]
    fn a_missing_report_opens_on_the_fault() {
        let v = ReportView::open(Path::new("/nonexistent/x-plot_ir.json"));
        assert!(!v.is_drawable());
        assert_eq!(v.title(), "ac-view \u{2014} x-plot_ir.json");
    }
}
