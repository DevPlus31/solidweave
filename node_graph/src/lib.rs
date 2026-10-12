//! node_graph — a tiny Grasshopper-style visual-programming engine over mini_brep.
//!
//!   value.rs : data on wires (every port carries a list)
//!   nodes.rs : node types + longest-list matching
//!   graph.rs : wiring, type/cycle checks, cached incremental evaluation, text form
//!   kernel.rs: the geometry backend behind every node (mini_brep by default)

pub mod graph;
pub mod kernel;
pub mod nodes;
pub mod value;

pub use graph::{Graph, NodeId};
pub use kernel::{Kernel, MiniBrep, Solid};
pub use nodes::*;
pub use value::{Data, Kind, Value};

/// Convenience for building sliders.
pub fn slider(value: f64, min: f64, max: f64) -> Box<NumberSlider> {
    Box::new(NumberSlider { value, min, max })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbers(d: &Data) -> Vec<f64> {
        d.iter().map(|v| v.as_number().unwrap()).collect()
    }

    /// width, depth sliders → Rectangle → Extrude by Series heights → Volume → Sum
    fn tower() -> (Graph, [NodeId; 7]) {
        let mut g = Graph::new();
        let w = g.add_node(slider(2.0, 0.0, 10.0));
        let d = g.add_node(slider(3.0, 0.0, 10.0));
        let rect = g.add_node(Box::new(Rectangle));
        let heights = g.add_node(Box::new(Series));
        let ext = g.add_node(Box::new(Extrude));
        let vol = g.add_node(Box::new(Volume));
        let sum = g.add_node(Box::new(MassAddition));
        g.connect(w, 0, rect, 0).unwrap();
        g.connect(d, 0, rect, 1).unwrap();
        // Series heights 1, 2, 3, 4 (start and count from sliders, step default 1)
        let start = g.add_node(slider(1.0, 0.0, 10.0));
        let count = g.add_node(slider(4.0, 0.0, 20.0));
        g.connect(start, 0, heights, 0).unwrap();
        g.connect(count, 0, heights, 2).unwrap();
        g.connect(rect, 0, ext, 0).unwrap();
        g.connect(heights, 0, ext, 1).unwrap();
        g.connect(ext, 0, vol, 0).unwrap();
        g.connect(vol, 0, sum, 0).unwrap();
        g.evaluate();
        (g, [w, d, rect, heights, ext, vol, sum])
    }

    #[test]
    fn longest_list_matching() {
        let (g, [.., ext, vol, sum]) = tower();
        // 1 rectangle × 4 heights → 4 solids
        assert_eq!(g.output(ext, 0).len(), 4);
        assert_eq!(numbers(g.output(vol, 0)), vec![6.0, 12.0, 18.0, 24.0]);
        assert_eq!(numbers(g.output(sum, 0)), vec![60.0]);
    }

    #[test]
    fn only_downstream_recomputes() {
        let (mut g, [w, .., sum]) = tower();
        g.evaluate();
        assert_eq!(g.last_solve_count, 0, "nothing changed → nothing recomputed");
        g.set_number(w, 4.0).unwrap();
        g.evaluate();
        // width slider, Rectangle, Extrude, Volume, MassAdd = 5 (not depth, Series, its sliders)
        assert_eq!(g.last_solve_count, 5);
        assert_eq!(numbers(g.output(sum, 0)), vec![120.0]);
    }

    #[test]
    fn rejects_type_mismatch_and_cycles() {
        let (mut g, [w, _, rect, _, ext, vol, _]) = tower();
        assert!(g.connect(rect, 0, vol, 0).unwrap_err().contains("type mismatch"));
        assert!(g.connect(w, 0, w, 0).is_err());
        // Volume output feeding back into extrude height would form a loop.
        assert!(g.connect(vol, 0, ext, 1).unwrap_err().contains("cycle"));
    }

    #[test]
    fn errors_stay_local_and_propagate_as_empty() {
        let (mut g, [w, _, rect, _, ext, _, sum]) = tower();
        g.set_number(w, 0.0).unwrap(); // zero-width rectangle is invalid
        g.evaluate();
        assert!(g.error(rect).unwrap().contains("positive"));
        assert!(g.error(ext).unwrap().contains("upstream"));
        assert!(g.output(sum, 0).is_empty());
        g.set_number(w, 1.0).unwrap(); // fixing it heals the whole chain
        g.evaluate();
        assert!(g.error(ext).is_none());
        assert_eq!(numbers(g.output(sum, 0)), vec![30.0]);
    }

    fn exported(g: &Graph, export: NodeId) -> Vec<usd_brep::BrepArray> {
        let text = g.output(export, 0)[0].as_text().unwrap();
        usd_brep::BrepArray::read_all(text).unwrap()
    }

    #[test]
    fn usd_export_writes_one_valid_prim_per_solid() {
        let (mut g, [.., ext, vol, _]) = tower();
        let export = g.add_node(Box::new(UsdExport));
        g.connect(ext, 0, export, 0).unwrap();
        g.evaluate();
        assert!(g.error(export).is_none(), "{:?}", g.error(export));
        assert_eq!(g.output(export, 0).len(), 1, "all solids go into one layer");
        let breps = exported(&g, export);
        let names: Vec<_> = breps.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["Solid_0", "Solid_1", "Solid_2", "Solid_3"]);
        // Each prim imports back to the solid it came from.
        let volumes: Vec<f64> =
            breps.iter().map(|b| usd_brep::convert::to_mini_brep(b, 0).unwrap().volume()).collect();
        assert_eq!(volumes, numbers(g.output(vol, 0)));
        // Only Solid inputs are accepted.
        assert!(g.connect(vol, 0, export, 0).unwrap_err().contains("type mismatch"));
    }

    #[test]
    fn usd_export_follows_upstream_edits_and_errors() {
        let (mut g, [w, .., ext, _, _]) = tower();
        let export = g.add_node(Box::new(UsdExport));
        g.connect(ext, 0, export, 0).unwrap();
        g.evaluate();
        g.set_number(w, 0.0).unwrap(); // invalid rectangle upstream
        g.evaluate();
        assert!(g.error(export).unwrap().contains("upstream"));
        assert!(g.output(export, 0).is_empty());
        g.set_number(w, 4.0).unwrap();
        g.evaluate();
        assert!(g.error(export).is_none());
        let first = usd_brep::convert::to_mini_brep(&exported(&g, export)[0], 0).unwrap();
        assert!((first.volume() - 12.0).abs() < 1e-9, "4 x 3 x 1 after the edit");
    }

    /// A backend that cannot build anything, to show nodes only use the kernel.
    struct NoGeometry;
    impl Kernel for NoGeometry {
        fn name(&self) -> &'static str { "none" }
        fn extrude(&self, _: &[mini_brep::Vec3], _: f64) -> Result<std::rc::Rc<dyn Solid>, String> {
            Err("this kernel cannot extrude".into())
        }
    }

    #[test]
    fn kernel_can_be_swapped_and_back() {
        let (mut g, [_, _, rect, _, ext, _, sum]) = tower();
        assert_eq!(g.kernel().name(), "mini_brep");
        g.set_kernel(NoGeometry);
        g.evaluate();
        assert_eq!(g.last_solve_count, 9, "a new kernel recomputes every node");
        assert_eq!(g.error(ext), Some("this kernel cannot extrude"));
        assert!(g.error(rect).is_none(), "profiles do not need a kernel");
        assert!(g.output(sum, 0).is_empty());
        g.set_kernel(MiniBrep);
        g.evaluate();
        assert!(g.error(ext).is_none());
        assert_eq!(numbers(g.output(sum, 0)), vec![60.0]);
    }

    #[test]
    fn graph_built_on_a_kernel_uses_it_from_the_start() {
        let mut g = Graph::with_kernel(NoGeometry);
        let ext = g.add_node(Box::new(Extrude));
        let rect = g.add_node(Box::new(Rectangle));
        g.connect(rect, 0, ext, 0).unwrap();
        g.evaluate();
        assert_eq!(g.kernel().name(), "none");
        assert_eq!(g.error(ext), Some("this kernel cannot extrude"));
    }

    #[test]
    fn text_form_lists_every_node() {
        let (g, _) = tower();
        let t = g.to_text();
        assert_eq!(t.lines().count(), 9);
        assert!(t.contains("Extrude(profile=n2.R, height=n3.S)"));
    }
}
