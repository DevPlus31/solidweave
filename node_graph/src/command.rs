//! The edits an AI (or a script) makes, and the report it reads back.
//!
//! An AI never touches geometry. It sends `Command`s, which name nodes and
//! ports the way the text form does (`n2.R`, `n4.profile`). Each command is
//! applied whole or refused with a message that says what would work: the
//! node types, ports or range that are valid. After a batch of edits it
//! calls `Graph::check`, which evaluates the graph, validates every result
//! solid as NVIDIA's BrepArray validator would, and lists what the graph
//! makes and what is wrong. That report is the feedback loop: the AI fixes
//! its own mistakes from it.

use crate::graph::{Graph, NodeId};
use crate::nodes::{NODE_NAMES, NodeType, Port, create, library};
use std::fmt::{self, Write as _};

/// One edit, as an AI tool call describes it.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// A new node of a type in `NODE_NAMES`. The reply gives its id.
    AddNode { name: String },
    /// Removes a node and every wire to or from it.
    RemoveNode { node: NodeId },
    /// Wires `from.output` into `to.input`, replacing any wire already there.
    Connect { from: NodeId, output: String, to: NodeId, input: String },
    /// Removes the wire into `node.input`, which goes back to its default.
    Disconnect { node: NodeId, input: String },
    /// Sets a Slider's value and, if given, its (min, max) range first.
    SetSlider { node: NodeId, value: f64, range: Option<(f64, f64)> },
}

impl Graph {
    /// Applies one command. `Ok` says what changed; `Err` says why nothing
    /// did and what would work instead. A refused command leaves the graph
    /// as it was.
    pub fn apply(&mut self, command: &Command) -> Result<String, String> {
        match command {
            Command::AddNode { name } => {
                let ty = create(name)
                    .ok_or_else(|| format!("there is no node type '{name}'; the types are: {}", NODE_NAMES.join(", ")))?;
                let summary = signature(ty.as_ref());
                let id = self.add_node(ty);
                Ok(format!("added n{} = {summary}", id.0))
            }
            Command::RemoveNode { node } => {
                let name = self.type_of(*node)?.name();
                self.remove_node(*node)?;
                Ok(format!("removed n{} ({name})", node.0))
            }
            Command::Connect { from, output, to, input } => {
                let out = port_index(&self.type_of(*from)?.outputs(), output, *from, "output")?;
                let inp = port_index(&self.type_of(*to)?.inputs(), input, *to, "input")?;
                self.connect(*from, out, *to, inp).map_err(|e| format!("cannot wire n{}.{output} into n{}.{input}: {e}", from.0, to.0))?;
                Ok(format!("wired n{}.{output} into n{}.{input}", from.0, to.0))
            }
            Command::Disconnect { node, input } => {
                let inp = port_index(&self.type_of(*node)?.inputs(), input, *node, "input")?;
                if !self.wires().iter().any(|w| w.to == *node && w.in_port == inp) {
                    return Ok(format!("n{}.{input} had no wire", node.0));
                }
                self.disconnect(*node, inp);
                Ok(format!("unwired n{}.{input}", node.0))
            }
            Command::SetSlider { node, value, range } => {
                let ty = self.type_of(*node)?;
                let (_, current) = ty
                    .number_setting()
                    .ok_or_else(|| format!("n{} is a {}, not a Slider; only Sliders hold numbers", node.0, ty.name()))?;
                let (lo, hi) = range.unwrap_or((*current.start(), *current.end()));
                if !(value.is_finite() && lo.is_finite() && hi.is_finite()) {
                    return Err("slider numbers must be finite".into());
                }
                if lo > hi {
                    return Err(format!("range {lo}..{hi} is reversed"));
                }
                if !(lo..=hi).contains(value) {
                    return Err(format!("value {value} is outside n{}'s range {lo}..{hi}; give a range that includes it", node.0));
                }
                if range.is_some() {
                    self.set_state(*node, "range", &format!("{lo}..{hi}"))?;
                }
                self.set_number(*node, *value)?;
                Ok(format!("n{} = Slider(value={value}, range={lo}..{hi})", node.0))
            }
        }
    }

    /// Evaluates the graph and reports what it makes and what is wrong.
    pub fn check(&mut self) -> Report {
        self.evaluate();
        let mut report = Report::default();
        for id in self.node_ids() {
            let ty = self.node_type(id).expect("live node");
            let at = format!("n{} {}", id.0, ty.name());
            // A node downstream of a failure only says "upstream failed";
            // the failure itself is reported where it happens.
            if let Some(e) = self.error(id).filter(|e| !e.starts_with("upstream")) {
                report.problems.push(format!("{at}: {e}"));
            }
            // Some nodes treat a loose input as an empty list and still
            // produce output (Difference cuts no holes), so only warn when
            // the node really made nothing.
            let made_nothing = (0..ty.outputs().len()).all(|o| self.output(id, o).is_empty());
            for p in ty.inputs().iter().filter(|p| made_nothing && p.default.is_none()) {
                let port = ty.inputs().iter().position(|q| q.name == p.name).expect("own port");
                if !self.wires().iter().any(|w| w.to == id && w.in_port == port) {
                    report.problems.push(format!("{at}: input '{}' is not connected, so the node makes nothing", p.name));
                }
            }
        }
        for (id, solid) in self.result_solids() {
            let name = self.node_type(id).expect("live node").name();
            let (v, e, f) = solid.counts();
            let mut line = format!("n{} {name}: solid, volume {}, area {}", id.0, num(solid.volume()), num(solid.surface_area()));
            if let Some((lo, hi)) = solid.bounding_box() {
                let _ = write!(line, ", box ({}, {}, {}) to ({}, {}, {})", num(lo.x), num(lo.y), num(lo.z), num(hi.x), num(hi.y), num(hi.z));
            }
            let _ = write!(line, ", {v} vertices, {e} edges, {f} faces");
            report.results.push(line);
            // The exchange form must pass NVIDIA's rules (BA_xxx codes).
            let brep = solid.to_brep_array("Solid");
            for issue in usd_brep::validate(&brep).into_iter().filter(|i| i.severity == usd_brep::Severity::Failure) {
                report.problems.push(format!("n{} {name}: BrepArray check {issue}", id.0));
            }
        }
        if report.results.is_empty() {
            report.problems.push("the graph makes no solid".into());
        }
        report
    }

    fn type_of(&self, id: NodeId) -> Result<&dyn NodeType, String> {
        self.node_type(id).ok_or_else(|| {
            let ids: Vec<String> = self.node_ids().iter().map(|n| format!("n{}", n.0)).collect();
            format!("there is no node n{}; the nodes are: {}", id.0, if ids.is_empty() { "none".into() } else { ids.join(", ") })
        })
    }
}

/// What `Graph::check` found.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Report {
    /// One line per result solid: node, volume, area, bounding box, counts.
    pub results: Vec<String>,
    /// Node errors, unconnected inputs, failed BrepArray checks.
    pub problems: Vec<String>,
}

impl Report {
    pub fn is_ok(&self) -> bool {
        self.problems.is_empty()
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "Results:")?;
        if self.results.is_empty() {
            writeln!(f, "  none")?;
        }
        for r in &self.results {
            writeln!(f, "  {r}")?;
        }
        writeln!(f, "Problems:")?;
        if self.problems.is_empty() {
            writeln!(f, "  none")?;
        }
        for p in &self.problems {
            writeln!(f, "  {p}")?;
        }
        Ok(())
    }
}

/// Every node type with its ports and description: what an AI is shown so
/// it knows what it can build.
pub fn catalog() -> String {
    let mut s = String::new();
    for ty in library() {
        let _ = writeln!(s, "{}\n    {}", signature(ty.as_ref()), ty.description());
    }
    s
}

/// `Rectangle(x: Number = 10, y: Number = 5) -> R: Profile`
fn signature(ty: &dyn NodeType) -> String {
    let input = |p: &Port| match &p.default {
        Some(d) => format!("{}: {:?} = {d}", p.name, p.kind),
        None => format!("{}: {:?}", p.name, p.kind),
    };
    let ins: Vec<String> = ty.inputs().iter().map(input).collect();
    let outs: Vec<String> = ty.outputs().iter().map(|p| format!("{}: {:?}", p.name, p.kind)).collect();
    let state = ty.state().map_or(String::new(), |s| format!(" [{s}]"));
    format!("{}({}) -> {}{state}", ty.name(), ins.join(", "), outs.join(", "))
}

fn port_index(ports: &[Port], name: &str, node: NodeId, what: &str) -> Result<usize, String> {
    ports.iter().position(|p| p.name == name).ok_or_else(|| {
        let names: Vec<&str> = ports.iter().map(|p| p.name).collect();
        let list = if names.is_empty() { "none".to_string() } else { names.join(", ") };
        format!("n{} has no {what} '{name}'; its {what}s are: {list}", node.0)
    })
}

/// A number with at most 4 decimals and no trailing zeros.
fn num(x: f64) -> String {
    let s = format!("{x:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add(g: &mut Graph, name: &str) -> NodeId {
        let reply = g.apply(&Command::AddNode { name: name.into() }).unwrap();
        NodeId(reply.strip_prefix("added n").unwrap().split(' ').next().unwrap().parse().unwrap())
    }

    fn wire(g: &mut Graph, from: NodeId, output: &str, to: NodeId, input: &str) -> Result<String, String> {
        g.apply(&Command::Connect { from, output: output.into(), to, input: input.into() })
    }

    fn set(g: &mut Graph, node: NodeId, value: f64, range: Option<(f64, f64)>) -> Result<String, String> {
        g.apply(&Command::SetSlider { node, value, range })
    }

    /// A 20 x 10 x 4 block, built the way an AI would: command by command.
    fn block() -> (Graph, [NodeId; 3]) {
        let mut g = Graph::new();
        let rect = add(&mut g, "Rectangle");
        let ext = add(&mut g, "Extrude");
        let h = add(&mut g, "Slider");
        let (w, d) = (add(&mut g, "Slider"), add(&mut g, "Slider"));
        set(&mut g, w, 20.0, Some((1.0, 100.0))).unwrap();
        set(&mut g, d, 10.0, Some((1.0, 100.0))).unwrap();
        set(&mut g, h, 4.0, None).unwrap();
        wire(&mut g, w, "N", rect, "x").unwrap();
        wire(&mut g, d, "N", rect, "y").unwrap();
        wire(&mut g, rect, "R", ext, "profile").unwrap();
        wire(&mut g, h, "N", ext, "height").unwrap();
        (g, [rect, ext, h])
    }

    #[test]
    fn commands_build_a_part_and_check_reports_it() {
        let (mut g, [_, ext, _]) = block();
        let report = g.check();
        assert!(report.is_ok(), "{report}");
        assert_eq!(
            report.results,
            [format!("n{} Extrude: solid, volume 800, area 640, box (0, 0, 0) to (20, 10, 4), 8 vertices, 12 edges, 6 faces", ext.0)]
        );
    }

    #[test]
    fn replies_say_what_changed() {
        let mut g = Graph::new();
        assert_eq!(
            g.apply(&Command::AddNode { name: "Rectangle".into() }),
            Ok("added n0 = Rectangle(x: Number = 10, y: Number = 5) -> R: Profile".into())
        );
        let s = add(&mut g, "Slider");
        assert_eq!(set(&mut g, s, 7.0, Some((0.0, 50.0))), Ok("n1 = Slider(value=7, range=0..50)".into()));
        assert_eq!(wire(&mut g, s, "N", NodeId(0), "y"), Ok("wired n1.N into n0.y".into()));
        let unwire = |g: &mut Graph| g.apply(&Command::Disconnect { node: NodeId(0), input: "y".into() });
        assert_eq!(unwire(&mut g), Ok("unwired n0.y".into()));
        assert_eq!(unwire(&mut g), Ok("n0.y had no wire".into()));
        assert_eq!(g.apply(&Command::RemoveNode { node: s }), Ok("removed n1 (Slider)".into()));
    }

    #[test]
    fn refusals_say_what_would_work_and_change_nothing() {
        let (mut g, [rect, ext, h]) = block();
        let before = g.to_text();
        let cases = [
            (Command::AddNode { name: "Cube".into() }, "there is no node type 'Cube'; the types are: Slider, Series"),
            (Command::RemoveNode { node: NodeId(42) }, "there is no node n42; the nodes are: n0, n1, n2, n3, n4"),
            (
                Command::Connect { from: rect, output: "P".into(), to: ext, input: "profile".into() },
                "n0 has no output 'P'; its outputs are: R",
            ),
            (
                Command::Connect { from: rect, output: "R".into(), to: ext, input: "length".into() },
                "n1 has no input 'length'; its inputs are: profile, height",
            ),
            (
                Command::Connect { from: rect, output: "R".into(), to: ext, input: "height".into() },
                "cannot wire n0.R into n1.height: type mismatch",
            ),
            (Command::Disconnect { node: h, input: "x".into() }, "n2 has no input 'x'; its inputs are: none"),
            (Command::SetSlider { node: rect, value: 3.0, range: None }, "n0 is a Rectangle, not a Slider"),
            (Command::SetSlider { node: h, value: 12.0, range: None }, "value 12 is outside n2's range 0..10"),
            (Command::SetSlider { node: h, value: 1.0, range: Some((5.0, 2.0)) }, "range 5..2 is reversed"),
            (Command::SetSlider { node: h, value: f64::NAN, range: None }, "slider numbers must be finite"),
        ];
        for (command, expected) in cases {
            let err = g.apply(&command).unwrap_err();
            assert!(err.starts_with(expected), "{command:?}: got {err:?}");
        }
        assert_eq!(g.to_text(), before);
    }

    #[test]
    fn a_wider_range_lets_a_slider_go_further() {
        let (mut g, [_, ext, h]) = block();
        set(&mut g, h, 12.0, Some((0.0, 50.0))).unwrap();
        assert!(g.check().results[0].starts_with(&format!("n{} Extrude: solid, volume 2400,", ext.0)));
    }

    #[test]
    fn a_loose_input_that_still_gives_output_is_no_problem() {
        // Difference with no holes wired passes the outline through, so
        // check must not claim it makes nothing.
        let (mut g, [rect, ext, _]) = block();
        let cut = add(&mut g, "Difference");
        wire(&mut g, rect, "R", cut, "outer").unwrap();
        wire(&mut g, cut, "P", ext, "profile").unwrap();
        let report = g.check();
        assert!(report.is_ok(), "{report}");
        assert!(report.results[0].contains("volume 800,"), "{report}");
    }

    #[test]
    fn check_finds_root_causes_and_loose_inputs() {
        let (mut g, [rect, ext, _]) = block();
        let vol = add(&mut g, "Volume");
        wire(&mut g, ext, "S", vol, "solid").unwrap();
        // A zero-width rectangle fails; Extrude and Volume downstream only
        // say "upstream failed", which the report leaves out.
        let w = add(&mut g, "Slider");
        set(&mut g, w, 0.0, None).unwrap();
        wire(&mut g, w, "N", rect, "x").unwrap();
        let loose = add(&mut g, "Revolve");
        let report = g.check();
        assert_eq!(
            report.problems,
            [
                "n0 Rectangle: rectangle size must be positive, got 0 x 10".to_string(),
                format!("n{} Revolve: input 'profile' is not connected, so the node makes nothing", loose.0),
                "the graph makes no solid".to_string(),
            ]
        );
        assert!(report.to_string().contains("Results:\n  none\nProblems:\n  n0 Rectangle"), "{report}");
    }

    #[test]
    fn check_reports_kernel_refusals() {
        let (mut g, [_, ext, _]) = block();
        // A wall across the block splits it in two: one shell only for now.
        let (r, m, e, cut) = (add(&mut g, "Rectangle"), add(&mut g, "Move"), add(&mut g, "Extrude"), add(&mut g, "SolidDifference"));
        let (w, d, x, y, h) = (add(&mut g, "Slider"), add(&mut g, "Slider"), add(&mut g, "Slider"), add(&mut g, "Slider"), add(&mut g, "Slider"));
        set(&mut g, w, 2.0, None).unwrap();
        set(&mut g, d, 14.0, Some((0.0, 20.0))).unwrap();
        set(&mut g, x, 9.0, None).unwrap();
        set(&mut g, y, -2.0, Some((-5.0, 5.0))).unwrap();
        set(&mut g, h, 6.0, None).unwrap();
        for (from, out, to, inp) in [(w, "N", r, "x"), (d, "N", r, "y"), (r, "R", m, "profile"), (x, "N", m, "x"), (y, "N", m, "y")] {
            wire(&mut g, from, out, to, inp).unwrap();
        }
        for (from, out, to, inp) in [(m, "P", e, "profile"), (h, "N", e, "height"), (ext, "S", cut, "A"), (e, "S", cut, "B")] {
            wire(&mut g, from, out, to, inp).unwrap();
        }
        let report = g.check();
        assert_eq!(report.problems.len(), 2, "{report}");
        assert!(report.problems[0].starts_with(&format!("n{} SolidDifference: the result has 2 shells", cut.0)), "{report}");
        assert_eq!(report.problems[1], "the graph makes no solid");
    }

    #[test]
    fn every_result_passes_the_brep_array_checks() {
        // The turned-parts mix: cylinders, cones, tori and spheres all export.
        let text = "
            n0 = Polygon(radius=n1.N, sides=n2.N)
            n1 = Slider(value=4, range=0..10)
            n2 = Slider(value=3, range=3..10)
            n3 = Move(profile=n0.P, x=n4.N, y=n4.N)
            n4 = Slider(value=6, range=0..10)
            n5 = Revolve(profile=n3.P)
            n6 = Sphere(x=0, y=0, z=0, radius=n1.N)
        ";
        let report = Graph::from_text(text).unwrap().check();
        assert!(report.is_ok(), "{report}");
        assert_eq!(report.results.len(), 2);
    }

    #[test]
    fn catalog_lists_every_node_type_with_ports() {
        let c = catalog();
        assert_eq!(c.lines().count(), 2 * NODE_NAMES.len());
        assert!(c.contains("Extrude(profile: Profile, height: Number = 1) -> S: Solid\n    Extrude a profile"));
        assert!(c.contains("Slider() -> N: Number [value=1, range=0..10]"));
        assert!(c.contains("Volume(solid: Solid) -> V: Number, A: Number"));
    }

    #[test]
    fn numbers_print_short() {
        assert_eq!([num(800.0), num(2.5), num(1.0 / 3.0), num(-0.00001)], ["800", "2.5", "0.3333", "0"]);
    }
}
