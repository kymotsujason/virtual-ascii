mod app;
mod camera_check;
mod panels;
mod pipeline_bridge;
mod state;
mod v4l2_manager;

use eframe::egui;

pub fn run_gui() -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 700.0])
            .with_title("Virtual ASCII"),
        ..Default::default()
    };
    eframe::run_native(
        "Virtual ASCII",
        options,
        Box::new(|_cc| Ok(Box::new(app::VirtualAsciiApp::new()))),
    )
    .map_err(|e| anyhow::anyhow!("{}", e))
}
