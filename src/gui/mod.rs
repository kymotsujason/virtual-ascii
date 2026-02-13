mod app;
mod camera_check;
mod panels;
mod pipeline_bridge;
mod state;
mod v4l2_manager;

use eframe::egui;

fn load_icon() -> egui::IconData {
    let png_bytes = include_bytes!("../../assets/icon-256.png");
    let img = image::load_from_memory(png_bytes)
        .expect("Failed to load embedded icon")
        .into_rgba8();
    let (width, height) = img.dimensions();
    egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    }
}

pub fn run_gui() -> anyhow::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1200.0, 700.0])
            .with_title("Virtual ASCII")
            .with_icon(std::sync::Arc::new(load_icon())),
        ..Default::default()
    };
    eframe::run_native(
        "Virtual ASCII",
        options,
        Box::new(|_cc| Ok(Box::new(app::VirtualAsciiApp::new()))),
    )
    .map_err(|e| anyhow::anyhow!("{}", e))
}
