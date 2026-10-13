//! The node canvas. egui-snarl draws the graph, and every edit made on it
//! goes through node_graph's command API (add_node, connect, disconnect,
//! set_number, remove_node). The `Graph` stays the single source of truth:
//! the canvas only mirrors it, so an edit the graph rejects (a type mismatch,
//! a cycle) never appears on screen.

use egui::{Color32, Pos2, Ui};
use egui_snarl::ui::{PinInfo, SnarlPin, SnarlViewer};
use egui_snarl::{InPin, InPinId, OutPin, OutPinId, Snarl};
use crate::document::Positions;
use node_graph::{Graph, Kind, NODE_NAMES, NodeId, create};
use std::collections::HashMap;

/// Wire and pin colour per data kind, so mismatches are visible at a glance.
pub fn kind_color(kind: Kind) -> Color32 {
    match kind {
        Kind::Number => Color32::from_rgb(120, 170, 255),
        Kind::Point => Color32::from_rgb(210, 210, 120),
        Kind::Profile => Color32::from_rgb(110, 210, 140),
        Kind::Solid => Color32::from_rgb(240, 160, 80),
        Kind::Text => Color32::from_rgb(190, 150, 230),
    }
}

/// Column of each node: 0 for nodes fed by nothing, otherwise one more than
/// its deepest source.
pub fn depths(graph: &Graph) -> HashMap<NodeId, usize> {
    let ids = graph.node_ids();
    let mut depth: HashMap<NodeId, usize> = ids.iter().map(|&id| (id, 0)).collect();
    // The graph is acyclic, so depths settle within one pass per node.
    for _ in 0..ids.len() {
        for w in graph.wires() {
            let d = depth[&w.from] + 1;
            if depth[&w.to] < d {
                depth.insert(w.to, d);
            }
        }
    }
    depth
}

/// A canvas mirroring `graph`. Nodes go where `positions` says; the rest are
/// laid out in columns by depth.
pub fn snarl_from_graph(graph: &Graph, positions: &Positions) -> Snarl<NodeId> {
    let depth = depths(graph);
    let mut next_y: HashMap<usize, f32> = HashMap::new();
    let mut snarl = Snarl::new();
    let mut on_canvas = HashMap::new();
    for id in graph.node_ids() {
        let pos = match positions.get(&id) {
            Some(&[x, y]) => Pos2::new(x, y),
            None => {
                let column = depth[&id];
                let y = next_y.entry(column).or_insert(0.0);
                *y += 130.0;
                Pos2::new(column as f32 * 230.0, *y - 130.0)
            }
        };
        on_canvas.insert(id, snarl.insert_node(pos, id));
    }
    for w in graph.wires() {
        snarl.connect(
            OutPinId { node: on_canvas[&w.from], output: w.out_port },
            InPinId { node: on_canvas[&w.to], input: w.in_port },
        );
    }
    snarl
}

/// Where each graph node sits on the canvas.
pub fn canvas_positions(snarl: &Snarl<NodeId>) -> Positions {
    snarl.nodes_pos_ids().map(|(_, pos, &id)| (id, [pos.x, pos.y])).collect()
}

/// Adapts a `Graph` to egui-snarl. Canvas nodes carry their graph `NodeId`.
pub struct GraphViewer<'a> {
    pub graph: &'a mut Graph,
    /// Why the last edit was rejected, for the status bar.
    pub message: &'a mut Option<String>,
}

impl SnarlViewer<NodeId> for GraphViewer<'_> {
    fn title(&mut self, node: &NodeId) -> String {
        self.graph.node_type(*node).map_or("?", |t| t.name()).to_string()
    }

    fn inputs(&mut self, node: &NodeId) -> usize {
        self.graph.node_type(*node).map_or(0, |t| t.inputs().len())
    }

    fn outputs(&mut self, node: &NodeId) -> usize {
        self.graph.node_type(*node).map_or(0, |t| t.outputs().len())
    }

    fn show_input(&mut self, pin: &InPin, ui: &mut Ui, snarl: &mut Snarl<NodeId>) -> impl SnarlPin + 'static {
        let port = self.graph.node_type(snarl[pin.id.node]).and_then(|t| t.inputs().get(pin.id.input).cloned());
        let Some(port) = port else { return PinInfo::circle() };
        // An unwired input shows the default it falls back to.
        match (&port.default, pin.remotes.is_empty()) {
            (Some(d), true) => ui.label(format!("{} = {d}", port.name)),
            _ => ui.label(port.name),
        };
        PinInfo::circle().with_fill(kind_color(port.kind))
    }

    fn show_output(&mut self, pin: &OutPin, ui: &mut Ui, snarl: &mut Snarl<NodeId>) -> impl SnarlPin + 'static {
        let id = snarl[pin.id.node];
        let port = self.graph.node_type(id).and_then(|t| t.outputs().get(pin.id.output).cloned());
        let Some(port) = port else { return PinInfo::circle() };
        // One value: show it. A list: show how many items it holds.
        let data = self.graph.output(id, pin.id.output);
        match data {
            [single] => ui.label(format!("{} {single}", port.name)),
            _ => ui.label(format!("{} ({} items)", port.name, data.len())),
        };
        PinInfo::circle().with_fill(kind_color(port.kind))
    }

    fn has_body(&mut self, node: &NodeId) -> bool {
        self.graph.node_type(*node).is_some_and(|t| t.number_setting().is_some()) || self.graph.error(*node).is_some()
    }

    fn show_body(&mut self, node: egui_snarl::NodeId, _: &[InPin], _: &[OutPin], ui: &mut Ui, snarl: &mut Snarl<NodeId>) {
        let id = snarl[node];
        if let Some((value, range)) = self.graph.node_type(id).and_then(|t| t.number_setting()) {
            let mut v = value;
            if ui.add(egui::Slider::new(&mut v, range)).changed() {
                let _ = self.graph.set_number(id, v);
            }
        }
        if let Some(e) = self.graph.error(id) {
            ui.set_max_width(220.0);
            ui.colored_label(Color32::from_rgb(255, 120, 110), e);
        }
    }

    fn has_on_hover_popup(&mut self, _: &NodeId) -> bool {
        true
    }

    fn show_on_hover_popup(&mut self, node: egui_snarl::NodeId, _: &[InPin], _: &[OutPin], ui: &mut Ui, snarl: &mut Snarl<NodeId>) {
        if let Some(t) = self.graph.node_type(snarl[node]) {
            ui.label(t.description());
        }
    }

    fn connect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<NodeId>) {
        let (a, b) = (snarl[from.id.node], snarl[to.id.node]);
        match self.graph.connect(a, from.id.output, b, to.id.input) {
            // An input takes one wire; the graph replaced any old one.
            Ok(()) => {
                snarl.drop_inputs(to.id);
                snarl.connect(from.id, to.id);
                *self.message = None;
            }
            Err(e) => *self.message = Some(format!("Rejected: {e}")),
        }
    }

    fn disconnect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<NodeId>) {
        self.graph.disconnect(snarl[to.id.node], to.id.input);
        snarl.disconnect(from.id, to.id);
    }

    fn drop_inputs(&mut self, pin: &InPin, snarl: &mut Snarl<NodeId>) {
        self.graph.disconnect(snarl[pin.id.node], pin.id.input);
        snarl.drop_inputs(pin.id);
    }

    fn drop_outputs(&mut self, pin: &OutPin, snarl: &mut Snarl<NodeId>) {
        for remote in &pin.remotes {
            self.graph.disconnect(snarl[remote.node], remote.input);
        }
        snarl.drop_outputs(pin.id);
    }

    fn has_graph_menu(&mut self, _: Pos2, _: &mut Snarl<NodeId>) -> bool {
        true
    }

    fn show_graph_menu(&mut self, pos: Pos2, ui: &mut Ui, snarl: &mut Snarl<NodeId>) {
        ui.label("Add node");
        for name in NODE_NAMES {
            if ui.button(name).clicked() {
                if let Some(ty) = create(name) {
                    let id = self.graph.add_node(ty);
                    snarl.insert_node(pos, id);
                }
                ui.close();
            }
        }
    }

    fn has_node_menu(&mut self, _: &NodeId) -> bool {
        true
    }

    fn show_node_menu(&mut self, node: egui_snarl::NodeId, _: &[InPin], _: &[OutPin], ui: &mut Ui, snarl: &mut Snarl<NodeId>) {
        if ui.button("Remove").clicked() {
            let _ = self.graph.remove_node(snarl[node]);
            snarl.remove_node(node);
            ui.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::tests::example;

    fn starter_graph() -> Graph {
        example("Drilled")
    }

    /// The canvas node showing graph node `id`.
    fn canvas_node(snarl: &Snarl<NodeId>, id: NodeId) -> egui_snarl::NodeId {
        snarl.node_ids().find(|(_, n)| **n == id).unwrap().0
    }

    fn named(g: &Graph, name: &str) -> NodeId {
        g.node_ids().into_iter().find(|&id| g.node_type(id).unwrap().name() == name).unwrap()
    }

    #[test]
    fn layout_puts_sources_left_of_what_they_feed() {
        let g = starter_graph();
        let d = depths(&g);
        let (cut, solid) = (named(&g, "Difference"), named(&g, "Extrude"));
        assert!(d[&solid] > d[&cut]);
        assert_eq!(d[&named(&g, "Slider")], 0);
    }

    #[test]
    fn canvas_mirrors_every_node_and_wire() {
        let g = starter_graph();
        let snarl = snarl_from_graph(&g, &Default::default());
        assert_eq!(snarl.node_ids().count(), g.node_ids().len());
        assert_eq!(snarl.wires().count(), g.wires().len());
    }

    #[test]
    fn rejected_edits_never_reach_the_canvas() {
        let mut g = starter_graph();
        g.evaluate();
        let mut snarl = snarl_from_graph(&g, &Default::default());
        let mut message = None;
        let (wires_before, canvas_before) = (g.wires().len(), snarl.wires().count());
        let (volume, plate) = (named(&g, "Volume"), named(&g, "Rectangle"));
        // Rectangle's profile output into Volume's solid input: a type mismatch.
        let from = snarl.out_pin(OutPinId { node: canvas_node(&snarl, plate), output: 0 });
        let to = snarl.in_pin(InPinId { node: canvas_node(&snarl, volume), input: 0 });
        GraphViewer { graph: &mut g, message: &mut message }.connect(&from, &to, &mut snarl);
        assert!(message.as_deref().unwrap().contains("type mismatch"), "{message:?}");
        assert_eq!((g.wires().len(), snarl.wires().count()), (wires_before, canvas_before));
    }

    #[test]
    fn accepted_edits_update_graph_and_canvas_together() {
        let mut g = starter_graph();
        let mut snarl = snarl_from_graph(&g, &Default::default());
        let mut message = None;
        // Wire a new slider into the hole count, replacing the old one.
        let count = g.add_node(node_graph::slider(5.0, 0.0, 10.0));
        let counted = snarl.insert_node(Pos2::ZERO, count);
        let series = canvas_node(&snarl, named(&g, "Series"));
        let from = snarl.out_pin(OutPinId { node: counted, output: 0 });
        let to = snarl.in_pin(InPinId { node: series, input: 2 });
        let before = g.wires().len();
        GraphViewer { graph: &mut g, message: &mut message }.connect(&from, &to, &mut snarl);
        assert_eq!(message, None);
        assert_eq!(g.wires().len(), before, "the old wire was replaced, not added to");
        assert_eq!(snarl.wires().count(), g.wires().len());
        assert_eq!(snarl.in_pin(InPinId { node: series, input: 2 }).remotes, vec![OutPinId { node: counted, output: 0 }]);
        g.evaluate();
        assert_eq!(g.output(named(&g, "Series"), 0).len(), 5);
    }
}
