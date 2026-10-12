//! The graph: nodes + wires, with incremental, cached evaluation.
//!
//! Every edit (UI, script or AI) goes through these few methods:
//!   add_node · connect · disconnect · set_number · remove_node · set_kernel
//! so they are also the natural "tool calls" to expose to an LLM.

use crate::kernel::{Kernel, MiniBrep};
use crate::nodes::NodeType;
use crate::value::Data;
use std::collections::{HashSet, VecDeque};
use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wire {
    pub from: NodeId,
    pub out_port: usize,
    pub to: NodeId,
    pub in_port: usize,
}

pub struct Node {
    pub ty: Box<dyn NodeType>,
    /// Cached output lists, valid while `dirty` is false.
    pub outputs: Vec<Data>,
    pub error: Option<String>,
    dirty: bool,
}

pub struct Graph {
    nodes: Vec<Option<Node>>, // None = removed (keeps ids stable)
    wires: Vec<Wire>,
    /// The one geometry backend every node in this graph uses.
    kernel: Box<dyn Kernel>,
    /// How many node solves the last evaluate() performed (shows caching).
    pub last_solve_count: usize,
}

impl Default for Graph {
    fn default() -> Self {
        Self::with_kernel(MiniBrep)
    }
}

impl Graph {
    /// An empty graph on the default mini_brep kernel.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_kernel(kernel: impl Kernel + 'static) -> Self {
        Self { nodes: Vec::new(), wires: Vec::new(), kernel: Box::new(kernel), last_solve_count: 0 }
    }

    pub fn kernel(&self) -> &dyn Kernel {
        self.kernel.as_ref()
    }

    // ───────────────────────── Editing ─────────────────────────

    /// Swap the geometry backend. Every node recomputes on the next evaluate,
    /// since any of them may hold solids from the old kernel.
    pub fn set_kernel(&mut self, kernel: impl Kernel + 'static) {
        self.kernel = Box::new(kernel);
        for node in self.nodes.iter_mut().flatten() {
            node.dirty = true;
        }
    }

    pub fn add_node(&mut self, ty: Box<dyn NodeType>) -> NodeId {
        self.nodes.push(Some(Node { ty, outputs: Vec::new(), error: None, dirty: true }));
        NodeId(self.nodes.len() - 1)
    }

    pub fn remove_node(&mut self, id: NodeId) -> Result<(), String> {
        self.node(id)?;
        let downstream: Vec<NodeId> =
            self.wires.iter().filter(|w| w.from == id).map(|w| w.to).collect();
        self.wires.retain(|w| w.from != id && w.to != id);
        self.nodes[id.0] = None;
        for d in downstream {
            self.mark_dirty(d);
        }
        Ok(())
    }

    /// Wire an output port to an input port. An input accepts one wire;
    /// connecting again replaces the old wire.
    pub fn connect(&mut self, from: NodeId, out_port: usize, to: NodeId, in_port: usize) -> Result<(), String> {
        let out_kind = self.node(from)?.ty.outputs().get(out_port)
            .ok_or_else(|| format!("{from:?} has no output port {out_port}"))?.kind;
        let in_spec = self.node(to)?.ty.inputs().get(in_port).cloned()
            .ok_or_else(|| format!("{to:?} has no input port {in_port}"))?;
        if out_kind != in_spec.kind {
            return Err(format!("type mismatch: {out_kind:?} output → {:?} input '{}'", in_spec.kind, in_spec.name));
        }
        if from == to || self.downstream_of(to).contains(&from) {
            return Err("connection would create a cycle".into());
        }
        self.wires.retain(|w| !(w.to == to && w.in_port == in_port));
        self.wires.push(Wire { from, out_port, to, in_port });
        self.mark_dirty(to);
        Ok(())
    }

    pub fn disconnect(&mut self, to: NodeId, in_port: usize) {
        self.wires.retain(|w| !(w.to == to && w.in_port == in_port));
        self.mark_dirty(to);
    }

    /// Change a slider. Only it and nodes downstream of it will recompute.
    pub fn set_number(&mut self, id: NodeId, value: f64) -> Result<(), String> {
        let node = self.nodes.get_mut(id.0).and_then(Option::as_mut).ok_or("no such node")?;
        if !node.ty.set_number(value) {
            return Err(format!("{} has no settable value", node.ty.name()));
        }
        self.mark_dirty(id);
        Ok(())
    }

    // ─────────────────────── Evaluation ────────────────────────

    /// Recompute dirty nodes in dependency order. Errors stay local to the
    /// failing node; its downstream nodes receive empty data (like Grasshopper's
    /// red/orange components) instead of the whole graph failing.
    pub fn evaluate(&mut self) {
        self.last_solve_count = 0;
        for id in self.topological_order() {
            if !self.nodes[id.0].as_ref().unwrap().dirty {
                continue;
            }
            let n_in = self.nodes[id.0].as_ref().unwrap().ty.inputs().len();
            let mut inputs: Vec<Data> = Vec::with_capacity(n_in);
            let mut upstream_error = None;

            for (p, spec) in self.nodes[id.0].as_ref().unwrap().ty.inputs().iter().enumerate() {
                match self.wires.iter().find(|w| w.to == id && w.in_port == p) {
                    Some(w) => {
                        let src = self.nodes[w.from.0].as_ref().unwrap();
                        if src.error.is_some() {
                            upstream_error = Some(format!("upstream {} failed", src.ty.name()));
                        }
                        inputs.push(src.outputs.get(w.out_port).cloned().unwrap_or_default());
                    }
                    None => inputs.push(spec.default.clone().into_iter().collect()),
                }
            }

            let node = self.nodes[id.0].as_mut().unwrap();
            node.dirty = false;
            self.last_solve_count += 1;
            let n_out = node.ty.outputs().len();
            match upstream_error {
                Some(e) => {
                    node.outputs = vec![Vec::new(); n_out];
                    node.error = Some(e);
                }
                None => match node.ty.solve_lists(self.kernel.as_ref(), &inputs) {
                    Ok(out) => {
                        node.outputs = out;
                        node.error = None;
                    }
                    Err(e) => {
                        node.outputs = vec![Vec::new(); n_out];
                        node.error = Some(e);
                    }
                },
            }
        }
    }

    pub fn output(&self, id: NodeId, port: usize) -> &Data {
        &self.nodes[id.0].as_ref().expect("removed node").outputs[port]
    }

    pub fn error(&self, id: NodeId) -> Option<&str> {
        self.nodes[id.0].as_ref()?.error.as_deref()
    }

    // ─────────────────── Text form (for AI / diff / save) ───────────────────

    /// A compact, readable description of the whole graph — what an LLM
    /// reads before editing it, and what a reviewer sees as a diff.
    pub fn to_text(&self) -> String {
        let mut s = String::new();
        for id in self.topological_order() {
            let node = self.nodes[id.0].as_ref().unwrap();
            let args: Vec<String> = node.ty.inputs().iter().enumerate().map(|(p, spec)| {
                let src = match self.wires.iter().find(|w| w.to == id && w.in_port == p) {
                    Some(w) => {
                        let from = self.nodes[w.from.0].as_ref().unwrap();
                        format!("n{}.{}", w.from.0, from.ty.outputs()[w.out_port].name)
                    }
                    None => match &spec.default {
                        Some(v) => v.to_string(),
                        None => "∅".into(),
                    },
                };
                format!("{}={src}", spec.name)
            }).collect();
            let args = node.ty.state().into_iter().chain(args).collect::<Vec<_>>().join(", ");
            let _ = write!(s, "n{} = {}({args})", id.0, node.ty.name());
            if let Some(e) = &node.error {
                let _ = write!(s, "   # ERROR: {e}");
            }
            s.push('\n');
        }
        s
    }

    // ───────────────────────── Internals ─────────────────────────

    fn node(&self, id: NodeId) -> Result<&Node, String> {
        self.nodes.get(id.0).and_then(Option::as_ref).ok_or_else(|| format!("no node {id:?}"))
    }

    fn downstream_of(&self, start: NodeId) -> HashSet<NodeId> {
        let mut seen = HashSet::new();
        let mut queue = VecDeque::from([start]);
        while let Some(n) = queue.pop_front() {
            for w in self.wires.iter().filter(|w| w.from == n) {
                if seen.insert(w.to) {
                    queue.push_back(w.to);
                }
            }
        }
        seen
    }

    fn mark_dirty(&mut self, id: NodeId) {
        for n in std::iter::once(id).chain(self.downstream_of(id)) {
            if let Some(node) = self.nodes[n.0].as_mut() {
                node.dirty = true;
            }
        }
    }

    /// Kahn's algorithm. Cycles are rejected in `connect`, so this always
    /// covers every live node.
    fn topological_order(&self) -> Vec<NodeId> {
        let live: Vec<NodeId> =
            (0..self.nodes.len()).filter(|&i| self.nodes[i].is_some()).map(NodeId).collect();
        let mut indegree = vec![0usize; self.nodes.len()];
        for w in &self.wires {
            indegree[w.to.0] += 1;
        }
        let mut queue: VecDeque<NodeId> = live.iter().copied().filter(|n| indegree[n.0] == 0).collect();
        let mut order = Vec::with_capacity(live.len());
        while let Some(n) = queue.pop_front() {
            order.push(n);
            for w in self.wires.iter().filter(|w| w.from == n) {
                indegree[w.to.0] -= 1;
                if indegree[w.to.0] == 0 {
                    queue.push_back(w.to);
                }
            }
        }
        order
    }
}
