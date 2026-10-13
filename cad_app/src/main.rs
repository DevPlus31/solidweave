//! Desktop editor: a node canvas driving node_graph, with a live 3D preview.
//! `cad_app [FILE.swv]` opens a graph file, and follows changes to it.

mod app;
mod editor;
mod viewport;

use cad_app::{ai, document};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("Solidweave").with_inner_size([1400.0, 850.0]),
        depth_buffer: viewport::DEPTH_BITS,
        multisampling: viewport::MSAA,
        ..Default::default()
    };
    eframe::run_native(
        "Solidweave",
        options,
        Box::new(|cc| {
            let mut app = app::CadApp::new(std::env::args_os().nth(1).map(Into::into));
            if let Some(render_state) = &cc.wgpu_render_state {
                viewport::init_gpu(render_state);
                app.use_gpu();
            }
            Ok(Box::new(app))
        }),
    )
}
