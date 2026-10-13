//! Reading a graph back from its text form (what `Graph::to_text` writes and
//! an LLM reads), so the same text can be saved, loaded, kept as an undo
//! snapshot, or written by an AI.
//!
//! One node per line, in any order:
//!
//! ```text
//! n0 = Slider(value=40, range=0..100)
//! n2 = Rectangle(x=n0.N, y=5)
//! n4 = Extrude(profile=n2.R, height=n3.N)   # ERROR: anything after # is ignored
//! ```
//!
//! An input is wired from `n<id>.<output>`, left at its default (the default
//! value, or `∅` for inputs without one), or omitted. Other constants are
//! refused: numbers come from Sliders, so they stay editable. Leading
//! `key=value` items that are not inputs set the node's state, such as a
//! Slider's value and range. Node ids are kept as written.

use crate::graph::{Graph, NodeId};
use crate::nodes::create;
use crate::value::Value;

/// One `n<id> = Name(args)` line.
struct Line<'a> {
    number: usize,
    id: NodeId,
    name: &'a str,
    args: Vec<(&'a str, &'a str)>,
}

fn parse_line(number: usize, line: &str) -> Result<Option<Line<'_>>, String> {
    let line = line.split('#').next().unwrap_or("").trim();
    if line.is_empty() {
        return Ok(None);
    }
    let err = |msg: &str| format!("line {number}: {msg}: {line}");
    let (lhs, rhs) = line.split_once('=').ok_or_else(|| err("expected 'n<id> = Node(...)'"))?;
    let id = parse_id(lhs.trim()).ok_or_else(|| err("expected a node id like n3"))?;
    let rhs = rhs.trim();
    let open = rhs.find('(').ok_or_else(|| err("expected '('"))?;
    let body = rhs[open + 1..].strip_suffix(')').ok_or_else(|| err("expected ')' at the end"))?;
    let args = body
        .split(',')
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(|a| a.split_once('=').map(|(k, v)| (k.trim(), v.trim())).ok_or_else(|| err(&format!("'{a}' is not key=value"))))
        .collect::<Result<_, _>>()?;
    Ok(Some(Line { number, id, name: rhs[..open].trim(), args }))
}

fn parse_id(s: &str) -> Option<NodeId> {
    s.strip_prefix('n')?.parse().ok().map(NodeId)
}

impl Graph {
    /// Builds a graph from its text form; see the module docs. Errors name
    /// the line at fault. Every wire goes through `connect`, so type
    /// mismatches and cycles are refused as they are in an editor.
    pub fn from_text(text: &str) -> Result<Graph, String> {
        let lines: Vec<Line> =
            text.lines().enumerate().filter_map(|(i, l)| parse_line(i + 1, l).transpose()).collect::<Result<_, _>>()?;
        let mut g = Graph::new();
        let mut wires = Vec::new();
        for l in &lines {
            let mut ty = create(l.name).ok_or_else(|| format!("line {}: unknown node '{}'", l.number, l.name))?;
            let inputs = ty.inputs();
            for &(key, value) in &l.args {
                let Some((port, spec)) = inputs.iter().enumerate().find(|(_, p)| p.name == key) else {
                    ty.set_state(key, value).map_err(|e| format!("line {}: {e}", l.number))?;
                    continue;
                };
                if let Some((from, output)) = value.split_once('.').and_then(|(n, o)| Some((parse_id(n)?, o))) {
                    wires.push((l.number, from, output, l.id, port));
                    continue;
                }
                // Not a wire: only the input's own default is allowed.
                let default = spec.default.as_ref().map_or("∅".to_string(), Value::to_string);
                if value != default {
                    return Err(format!(
                        "line {}: {key}={value}: constant inputs are not supported (default is {default}); wire a Slider instead",
                        l.number
                    ));
                }
            }
            g.place_node(l.id, ty).map_err(|e| format!("line {}: {e}", l.number))?;
        }
        for (number, from, output, to, port) in wires {
            let outputs = g.node_type(from).ok_or_else(|| format!("line {number}: no node n{}", from.0))?.outputs();
            let out = outputs
                .iter()
                .position(|p| p.name == output)
                .ok_or_else(|| format!("line {number}: n{} has no output '{output}'", from.0))?;
            g.connect(from, out, to, port).map_err(|e| format!("line {number}: {e}"))?;
        }
        Ok(g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nodes::*;
    use crate::slider;

    fn plate() -> Graph {
        let mut g = Graph::new();
        let rect = g.add_node(Box::new(Rectangle));
        let w = g.add_node(slider(40.0, 1.0, 100.0));
        let ext = g.add_node(Box::new(Extrude));
        let h = g.add_node(slider(6.5, 0.5, 20.0));
        g.connect(w, 0, rect, 0).unwrap();
        g.connect(rect, 0, ext, 0).unwrap();
        g.connect(h, 0, ext, 1).unwrap();
        let vol = g.add_node(Box::new(Volume));
        g.connect(ext, 0, vol, 0).unwrap();
        g
    }

    #[test]
    fn text_round_trips_exactly() {
        let mut g = plate();
        g.remove_node(NodeId(1)).unwrap(); // a gap in the ids must survive
        let text = g.to_text();
        let mut back = Graph::from_text(&text).unwrap();
        assert_eq!(back.to_text(), text);
        assert_eq!(back.node_ids(), g.node_ids());
        g.evaluate();
        back.evaluate();
        assert_eq!(back.output(NodeId(4), 0)[0].as_number().unwrap(), g.output(NodeId(4), 0)[0].as_number().unwrap());
    }

    #[test]
    fn hand_written_text_builds_a_working_graph() {
        // As an AI might write it: any order, comments, omitted defaults.
        let text = "
            n5 = Volume(solid=n4.S)
            n4 = Extrude(profile=n2.R, height=n3.N)  # the part
            n2 = Rectangle(x=n0.N, y=n1.N)
            n0 = Slider(value=3, range=0..10)
            n1 = Slider(value=2, range=0..10)
            n3 = Slider(value=4, range=0..10)
        ";
        let mut g = Graph::from_text(text).unwrap();
        g.evaluate();
        assert_eq!(g.output(NodeId(5), 0)[0].as_number().unwrap(), 24.0);
        assert_eq!(g.node_type(NodeId(0)).unwrap().number_setting(), Some((3.0, 0.0..=10.0)));
    }

    #[test]
    fn errors_name_the_line() {
        let cases = [
            ("n0 = Teleport()", "line 1: unknown node 'Teleport'"),
            ("n0 = Rectangle(x=n7.N)", "line 1: no node n7"),
            ("n0 = Slider(value=1, range=0..2)\nn1 = Volume(solid=n0.N)", "line 2: type mismatch"),
            ("n0 = Rectangle(x=12)", "constant inputs are not supported"),
            ("n0 = Slider(colour=red)", "line 1: Slider has no setting 'colour'"),
            ("n0 = Rectangle()\nn0 = Circle()", "line 2: n0 is defined twice"),
            ("rectangle please", "line 1: expected 'n<id> = Node(...)'"),
        ];
        for (text, expected) in cases {
            let err = Graph::from_text(text).err().unwrap_or_default();
            assert!(err.contains(expected), "{text:?}: got {err:?}");
        }
    }

    #[test]
    fn cycles_are_refused_when_reading_too() {
        let text = "n0 = Move(profile=n1.P)\nn1 = Move(profile=n0.P)";
        assert!(Graph::from_text(text).err().unwrap().contains("cycle"));
    }
}
