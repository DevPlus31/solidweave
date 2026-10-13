//! The window: node canvas on the left, 3D preview in the middle, the
//! graph's text form (what an AI reads) and status at the bottom.

use crate::editor::{GraphViewer, snarl_from_graph};
use crate::viewport::{self, Camera, Scene};
use egui_snarl::Snarl;
use egui_snarl::ui::SnarlStyle;
use node_graph::{Graph, NodeId, create, slider};

pub struct CadApp {
    graph: Graph,
    canvas: Snarl<NodeId>,
    camera: Camera,
    scene: Scene,
    message: Option<String>,
}

/// A 40 x 20 x 6 plate with a row of round holes, all driven by sliders.
pub fn starter_graph() -> Graph {
    let mut g = Graph::new();
    let mut node = |name: &str| g.add_node(create(name).expect("known node"));
    let (plate, hole, xs, row, cut, solid, props, export) =
        (node("Rectangle"), node("Circle"), node("Series"), node("Move"), node("Difference"), node("Extrude"), node("Volume"), node("UsdExport"));
    let wire = |g: &mut Graph, value: f64, max: f64, to: NodeId, port: usize| {
        let s = g.add_node(slider(value, 0.0, max));
        g.connect(s, 0, to, port).expect("starter wiring");
    };
    wire(&mut g, 40.0, 100.0, plate, 0); // width
    wire(&mut g, 20.0, 100.0, plate, 1); // depth
    wire(&mut g, 3.0, 10.0, hole, 2); // hole radius
    wire(&mut g, 8.0, 100.0, xs, 0); // first hole x
    wire(&mut g, 12.0, 50.0, xs, 1); // spacing
    wire(&mut g, 3.0, 10.0, xs, 2); // hole count
    wire(&mut g, 10.0, 100.0, row, 2); // row y
    wire(&mut g, 6.0, 50.0, solid, 1); // thickness
    for (from, to, port) in [(hole, row, 0), (xs, row, 1), (plate, cut, 0), (row, cut, 1), (cut, solid, 0), (solid, props, 0), (solid, export, 0)] {
        g.connect(from, 0, to, port).expect("starter wiring");
    }
    g
}

impl CadApp {
    pub fn new() -> Self {
        let mut graph = starter_graph();
        graph.evaluate();
        let canvas = snarl_from_graph(&graph);
        let scene = Scene::from_graph(&graph);
        let mut camera = Camera::default();
        camera.fit(&scene);
        CadApp { graph, canvas, camera, scene, message: None }
    }

    /// Recomputes what changed and refreshes the preview if anything did.
    fn evaluate(&mut self) {
        self.graph.evaluate();
        if self.graph.last_solve_count > 0 {
            self.scene = Scene::from_graph(&self.graph);
        }
    }
}

impl eframe::App for CadApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.evaluate();

        egui::Panel::bottom("status").resizable(true).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("kernel: {}", self.graph.kernel().name()));
                ui.separator();
                ui.label(format!("{} nodes, {} wires", self.graph.node_ids().len(), self.graph.wires().len()));
                if let Some(m) = &self.message {
                    ui.separator();
                    ui.colored_label(egui::Color32::from_rgb(255, 120, 110), m);
                }
            });
            egui::CollapsingHeader::new("Text form (what the AI reads)").default_open(true).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                    ui.monospace(self.graph.to_text());
                });
            });
        });

        egui::Panel::left("canvas").resizable(true).default_size(700.0).show(ui, |ui| {
            ui.label("Right-click empty space to add a node, a node to remove it. Drag between pins to wire.");
            let mut viewer = GraphViewer { graph: &mut self.graph, message: &mut self.message };
            self.canvas.show(&mut viewer, &SnarlStyle::new(), "graph", ui);
        });

        // Apply this frame's edits before drawing the preview.
        self.evaluate();
        egui::CentralPanel::default().show(ui, |ui| viewport::show(ui, &mut self.camera, &self.scene));
    }
}
