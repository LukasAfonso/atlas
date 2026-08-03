mod app;
mod board;
mod markdown;
mod vault;

use app::AtlasApp;
use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title("Atlas")
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Atlas",
        options,
        Box::new(|creation_context| Ok(Box::new(AtlasApp::new(creation_context)))),
    )
}
