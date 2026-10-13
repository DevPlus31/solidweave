//! The editor's document logic, kept free of UI code so it can be tested:
//! examples, the file format, undo history, which solids to show, export.
//!
//! A saved file is the graph's text form (what an AI reads, see
//! `node_graph::text`) with each node's canvas position as a comment:
//! `n2 = Rectangle(x=n0.N, y=n1.N)  # @ 230,0`. The parser ignores comments,
//! so the file stays plain, AI-readable text.

use node_graph::{Graph, Kind, NodeId, Solid, Value};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::rc::Rc;

/// Canvas position of each node.
pub type Positions = HashMap<NodeId, [f32; 2]>;

/// File extension of saved graphs.
pub const EXTENSION: &str = "swv";

// ───────────────────────────── Examples ─────────────────────────────

/// (menu name, graph text). Each is checked by a test to load and evaluate
/// without errors.
pub const EXAMPLES: [(&str, &str); 3] = [
    ("Machined block (booleans)", MACHINED_BLOCK),
    ("Drilled plate (holes)", DRILLED_PLATE),
    ("Turned parts (revolve)", TURNED_PARTS),
];

/// A block with a slot cut through it and a rib joined on, all from sliders.
const MACHINED_BLOCK: &str = "\
n0 = Slider(value=60, range=10..100)
n1 = Slider(value=40, range=10..100)
n2 = Rectangle(x=n0.N, y=n1.N)
n3 = Slider(value=15, range=1..50)
n4 = Extrude(profile=n2.R, height=n3.N)
n5 = Slider(value=20, range=1..50)
n6 = Slider(value=12, range=1..50)
n7 = Rectangle(x=n5.N, y=n6.N)
n8 = Slider(value=8, range=0..60)
n9 = Slider(value=14, range=0..60)
n10 = Move(profile=n7.R, x=n8.N, y=n9.N)
n11 = Slider(value=30, range=1..60)
n12 = Extrude(profile=n10.P, height=n11.N)
n13 = SolidDifference(A=n4.S, B=n12.S)
n14 = Slider(value=6, range=1..30)
n15 = Slider(value=40, range=1..100)
n16 = Rectangle(x=n14.N, y=n15.N)
n17 = Slider(value=45, range=0..100)
n18 = Move(profile=n16.R, x=n17.N, y=0)
n19 = Slider(value=25, range=1..60)
n20 = Extrude(profile=n18.P, height=n19.N)
n21 = SolidUnion(A=n13.S, B=n20.S)
n22 = Volume(solid=n21.S)
n23 = UsdExport(solids=n21.S)
";

/// A 40 x 20 x 6 plate with a row of round holes.
const DRILLED_PLATE: &str = "\
n0 = Slider(value=40, range=0..100)
n1 = Slider(value=20, range=0..100)
n2 = Rectangle(x=n0.N, y=n1.N)
n3 = Slider(value=3, range=0..10)
n4 = Circle(x=0, y=0, radius=n3.N)
n5 = Slider(value=8, range=0..100)
n6 = Slider(value=12, range=0..50)
n7 = Slider(value=3, range=0..10)
n8 = Series(start=n5.N, step=n6.N, count=n7.N)
n9 = Slider(value=10, range=0..100)
n10 = Move(profile=n4.C, x=n8.S, y=n9.N)
n11 = Difference(outer=n2.R, holes=n10.P)
n12 = Slider(value=6, range=0..50)
n13 = Extrude(profile=n11.P, height=n12.N)
n14 = Volume(solid=n13.S)
n15 = UsdExport(solids=n13.S)
";

/// A tube revolved from a rectangle, a ring revolved from a circle, a ball.
const TURNED_PARTS: &str = "\
n0 = Slider(value=3, range=0.5..20)
n1 = Slider(value=12, range=1..40)
n2 = Rectangle(x=n0.N, y=n1.N)
n3 = Slider(value=5, range=0..20)
n4 = Move(profile=n2.R, x=n3.N, y=0)
n5 = Revolve(profile=n4.P)
n6 = Slider(value=12, range=0..40)
n7 = Slider(value=16, range=0..40)
n8 = Slider(value=2, range=0.5..5)
n9 = Circle(x=n6.N, y=n7.N, radius=n8.N)
n10 = Revolve(profile=n9.C)
n11 = Slider(value=26, range=0..60)
n12 = Slider(value=6, range=0.5..20)
n13 = Sphere(x=0, y=0, z=n11.N, radius=n12.N)
n14 = Volume(solid=n5.S)
";

// ───────────────────────────── File format ─────────────────────────────

/// The graph's text form without error notes: what gets saved, compared to
/// spot edits, and kept for undo.
pub fn graph_text(graph: &Graph) -> String {
    graph.to_text().lines().map(|l| l.split('#').next().unwrap_or("").trim_end()).fold(String::new(), |mut s, l| {
        s.push_str(l);
        s.push('\n');
        s
    })
}

/// The file contents: the graph's text with each node's position appended.
pub fn save_text(graph: &Graph, positions: &Positions) -> String {
    let mut out = String::from("# Solidweave graph\n");
    for line in graph_text(graph).lines() {
        out.push_str(line);
        let id = line.split('=').next().and_then(|n| n.trim().strip_prefix('n')?.parse().ok()).map(NodeId);
        if let Some([x, y]) = id.and_then(|id| positions.get(&id)) {
            let _ = write!(out, "  # @ {x:.0},{y:.0}");
        }
        out.push('\n');
    }
    out
}

/// Reads a file written by `save_text` (or any graph text). Positions are
/// optional.
pub fn load_text(text: &str) -> Result<(Graph, Positions), String> {
    let graph = Graph::from_text(text)?;
    let mut positions = Positions::new();
    for line in text.lines() {
        let Some((def, note)) = line.split_once('#') else { continue };
        let Some(at) = note.trim().strip_prefix('@') else { continue };
        let id = def.split('=').next().and_then(|n| n.trim().strip_prefix('n')?.parse().ok()).map(NodeId);
        let xy: Vec<f32> = at.split(',').filter_map(|v| v.trim().parse().ok()).collect();
        if let (Some(id), [x, y]) = (id, xy.as_slice()) {
            positions.insert(id, [*x, *y]);
        }
    }
    Ok((graph, positions))
}

// ───────────────────────────── Undo ─────────────────────────────

/// One state of the document: graph text and canvas layout.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub text: String,
    pub positions: Positions,
}

/// Undo and redo stacks of snapshots. Restoring one rebuilds the graph from
/// its text through `Graph::from_text`, so undo goes through the same
/// command API as every other edit.
#[derive(Default)]
pub struct History {
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

impl History {
    const LIMIT: usize = 200;

    /// Records the state before an edit. A new edit discards the redo stack.
    pub fn record(&mut self, before: Snapshot) {
        self.undo.push(before);
        if self.undo.len() > Self::LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// The state to go back to, given the current one.
    pub fn undo(&mut self, current: Snapshot) -> Option<Snapshot> {
        let s = self.undo.pop()?;
        self.redo.push(current);
        Some(s)
    }

    pub fn redo(&mut self, current: Snapshot) -> Option<Snapshot> {
        let s = self.redo.pop()?;
        self.undo.push(current);
        Some(s)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

// ───────────────────────────── Results and export ─────────────────────────────

/// The graph's results: solids that no other solid-making node consumes.
/// A boolean's inputs (the block and the cutter) are steps, not results.
pub fn result_solids(graph: &Graph) -> Vec<(NodeId, Rc<dyn Solid>)> {
    let makes_solids = |id: NodeId| graph.node_type(id).is_some_and(|t| t.outputs().iter().any(|p| p.kind == Kind::Solid));
    let mut out = Vec::new();
    for id in graph.node_ids() {
        let consumed = graph.wires().iter().any(|w| w.from == id && makes_solids(w.to));
        if !consumed {
            out.extend(solids_of(graph, id).into_iter().map(|s| (id, s)));
        }
    }
    out
}

/// Every solid on a node's outputs.
pub fn solids_of(graph: &Graph, id: NodeId) -> Vec<Rc<dyn Solid>> {
    let ports = graph.node_type(id).map_or(0, |t| t.outputs().len());
    (0..ports)
        .flat_map(|p| graph.output(id, p))
        .filter_map(|v| if let Value::Solid(s) = v { Some(Rc::clone(s)) } else { None })
        .collect()
}

/// The solids as one `.usda` layer, one BrepArray prim each, as `UsdExport` writes.
pub fn usda(solids: &[Rc<dyn Solid>]) -> String {
    let breps: Vec<_> = solids.iter().enumerate().map(|(i, s)| s.to_brep_array(&format!("Solid_{i}"))).collect();
    usd_brep::layer_to_usda(&breps)
}

/// The solids' tessellation as ASCII STL.
pub fn stl(solids: &[Rc<dyn Solid>]) -> String {
    let mut s = String::from("solid solidweave\n");
    for [a, b, c] in solids.iter().flat_map(|solid| solid.tessellate()) {
        let n = (b - a).cross(c - a).normalize();
        let _ = writeln!(s, "  facet normal {} {} {}\n    outer loop", n.x, n.y, n.z);
        for p in [a, b, c] {
            let _ = writeln!(s, "      vertex {} {} {}", p.x, p.y, p.z);
        }
        s.push_str("    endloop\n  endfacet\n");
    }
    s.push_str("endsolid solidweave\n");
    s
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn example(name_start: &str) -> Graph {
        let (_, text) = EXAMPLES.iter().find(|(n, _)| n.starts_with(name_start)).unwrap();
        Graph::from_text(text).unwrap()
    }

    fn volume_of_results(g: &Graph) -> f64 {
        result_solids(g).iter().map(|(_, s)| s.volume()).sum()
    }

    #[test]
    fn every_example_loads_and_evaluates_without_errors() {
        for (name, text) in EXAMPLES {
            let mut g = Graph::from_text(text).unwrap_or_else(|e| panic!("{name}: {e}"));
            g.evaluate();
            for id in g.node_ids() {
                assert!(g.error(id).is_none(), "{name}: n{} {:?}", id.0, g.error(id));
            }
            assert!(!result_solids(&g).is_empty(), "{name}");
        }
    }

    #[test]
    fn example_volumes_are_exact() {
        let pi = std::f64::consts::PI;
        let mut block = example("Machined");
        block.evaluate();
        // 60 x 40 x 15 block, minus a 20 x 12 slot through it, plus the part
        // of a 6 x 40 x 25 rib above the block.
        assert!((volume_of_results(&block) - (36000.0 - 3600.0 + 6.0 * 40.0 * 10.0)).abs() < 1e-6);
        let mut plate = example("Drilled");
        plate.evaluate();
        assert!((volume_of_results(&plate) - 6.0 * (800.0 - 27.0 * pi)).abs() < 1e-6);
        let mut turned = example("Turned");
        turned.evaluate();
        let tube = pi * (8.0f64.powi(2) - 5.0f64.powi(2)) * 12.0;
        let ring = 2.0 * pi * pi * 12.0 * 4.0;
        let ball = 4.0 / 3.0 * pi * 216.0;
        assert!((volume_of_results(&turned) - (tube + ring + ball)).abs() < 1e-6);
    }

    #[test]
    fn only_results_are_shown_not_the_steps_of_a_boolean() {
        let mut g = example("Machined");
        g.evaluate();
        let shown: Vec<NodeId> = result_solids(&g).iter().map(|(id, _)| *id).collect();
        assert_eq!(shown, vec![NodeId(21)], "the union only, not the block, cutter or rib");
    }

    #[test]
    fn saved_files_keep_graph_and_layout() {
        let mut g = example("Drilled");
        let positions: Positions = [(NodeId(2), [230.0, 0.0]), (NodeId(13), [690.0, 130.0])].into();
        let text = save_text(&g, &positions);
        assert!(text.contains("n2 = Rectangle(x=n0.N, y=n1.N)  # @ 230,0"), "{text}");
        let (mut back, back_positions) = load_text(&text).unwrap();
        assert_eq!(graph_text(&back), graph_text(&g));
        assert_eq!(back_positions, positions);
        g.evaluate();
        back.evaluate();
        assert_eq!(volume_of_results(&back), volume_of_results(&g));
    }

    #[test]
    fn error_notes_are_not_saved() {
        let mut g = Graph::from_text("n0 = Slider(value=0, range=0..10)\nn1 = Rectangle(x=n0.N)").unwrap();
        g.evaluate();
        assert!(g.to_text().contains("ERROR"));
        assert!(!save_text(&g, &Positions::new()).contains("ERROR"));
    }

    #[test]
    fn history_undoes_and_redoes_in_order() {
        let snap = |t: &str| Snapshot { text: t.into(), positions: Positions::new() };
        let mut h = History::default();
        h.record(snap("a"));
        h.record(snap("b"));
        assert_eq!(h.undo(snap("c")), Some(snap("b")));
        assert_eq!(h.undo(snap("b")), Some(snap("a")));
        assert_eq!(h.undo(snap("a")), None);
        assert_eq!(h.redo(snap("a")), Some(snap("b")));
        // A new edit drops what could have been redone.
        h.record(snap("b"));
        assert!(!h.can_redo());
    }

    #[test]
    fn exports_contain_every_result() {
        let mut g = example("Turned");
        g.evaluate();
        let solids: Vec<_> = result_solids(&g).into_iter().map(|(_, s)| s).collect();
        let text = usda(&solids);
        let breps = usd_brep::BrepArray::read_all(&text).unwrap();
        assert_eq!(breps.len(), 3);
        for b in &breps {
            assert!(usd_brep::validate(b).is_empty(), "{}", b.name);
        }
        let facets = stl(&solids).matches("facet normal").count();
        assert_eq!(facets, solids.iter().map(|s| s.tessellate().len()).sum::<usize>());
    }
}
