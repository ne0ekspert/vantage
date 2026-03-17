mod app;
mod commands;
mod domain;
mod import_export;
mod inspector;
mod interactions;
mod its_cctv;
mod map;
mod storage;
mod timeline;
mod traffic;
mod wigle;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1600.0, 960.0])
            .with_min_inner_size([1200.0, 760.0])
            .with_title("Vantage"),
        renderer: eframe::Renderer::Wgpu,
        depth_buffer: 24,
        ..Default::default()
    };

    eframe::run_native(
        "Vantage",
        native_options,
        Box::new(|cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(app::VantageApp::new(cc)))
        }),
    )
}
