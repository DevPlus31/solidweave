//! Desktop editor: a node canvas driving node_graph, with a live 3D preview.

mod app;
mod document;
mod editor;
mod viewport;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("Solidweave").with_inner_size([1400.0, 850.0]),
        ..Default::default()
    };
    eframe::run_native("Solidweave", options, Box::new(|_cc| Ok(Box::new(app::CadApp::new()))))
}
