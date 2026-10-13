//! The AI assistant: Claude edits the graph through node_graph's commands.
//!
//! The model is given the node catalog, the graph's text form and a set of
//! tools that map one to one onto `node_graph::Command` (plus `write_graph`,
//! which replaces the graph through `Graph::from_text`, and `check`). It
//! never sees or edits geometry. When it says it is done, the graph is
//! checked; if anything is wrong the report goes back to it to fix, a few
//! times at most. That loop is `Assistant::receive`.
//!
//! This module holds the conversation logic only, free of network and UI
//! code, so recorded replies can test it. `send` does the HTTP request, on
//! a background thread in the app.

use node_graph::{Command, Graph, NodeId, catalog};
use serde_json::{Value, json};

/// Read from the environment, never stored.
pub const KEY_VAR: &str = "ANTHROPIC_API_KEY";
/// Overrides the model.
pub const MODEL_VAR: &str = "SOLIDWEAVE_MODEL";
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";
/// Requests per prompt, so a confused model cannot loop forever.
const MAX_REQUESTS: usize = 25;
/// Times a failed check is sent back before giving up.
const MAX_FIX_ROUNDS: usize = 3;

/// One line of the assistant panel.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    /// What the user asked.
    Prompt(String),
    /// The model's own words.
    Reply(String),
    /// A command that changed the graph.
    Edit(String),
    /// A command the graph refused, with why.
    Refused(String),
    /// A check the model asked for, or the one run when it finished.
    Check(String),
    /// The request failed or the loop was cut short.
    Error(String),
}

/// What the app should do after `start` or `receive`.
#[derive(Debug, PartialEq)]
pub enum Next {
    /// Post this body to the Messages API and pass the reply to `receive`.
    Send(Value),
    Done,
}

/// One conversation with the model.
pub struct Assistant {
    model: String,
    messages: Vec<Value>,
    pub log: Vec<Entry>,
    requests: usize,
    fix_rounds: usize,
}

impl Assistant {
    pub fn new(model: impl Into<String>) -> Self {
        Self { model: model.into(), messages: Vec::new(), log: Vec::new(), requests: 0, fix_rounds: 0 }
    }

    /// Begins work on a prompt. Earlier prompts stay in the conversation, so
    /// "make it taller" refers to the last part.
    pub fn start(&mut self, prompt: &str, graph: &mut Graph) -> Next {
        self.log.push(Entry::Prompt(prompt.to_string()));
        self.requests = 0;
        self.fix_rounds = 0;
        let state = state(graph);
        self.messages.push(json!({ "role": "user", "content": format!("{prompt}\n\n{state}") }));
        self.next_request()
    }

    /// Handles one API reply: applies the tool calls it makes and says what
    /// to send next, if anything.
    pub fn receive(&mut self, graph: &mut Graph, reply: &Value) -> Next {
        if let Some(e) = reply.get("error") {
            let msg = e.get("message").and_then(Value::as_str).unwrap_or("unknown error");
            return self.fail(format!("API error: {msg}"));
        }
        let Some(content) = reply.get("content").and_then(Value::as_array) else {
            return self.fail(format!("unexpected reply: {reply}"));
        };
        self.messages.push(json!({ "role": "assistant", "content": content }));
        let mut results = Vec::new();
        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    let text = block["text"].as_str().unwrap_or("").trim();
                    if !text.is_empty() {
                        self.log.push(Entry::Reply(text.to_string()));
                    }
                }
                Some("tool_use") => {
                    let name = block["name"].as_str().unwrap_or("");
                    let (entry, text) = run_tool(graph, name, &block["input"]);
                    let ok = !matches!(entry, Entry::Refused(_));
                    self.log.push(entry);
                    results.push(json!({
                        "type": "tool_result",
                        "tool_use_id": block["id"],
                        "content": text,
                        "is_error": !ok,
                    }));
                }
                _ => {}
            }
        }
        if !results.is_empty() {
            self.messages.push(json!({ "role": "user", "content": results }));
            return self.next_request();
        }
        // The model has finished: check its work.
        let report = graph.check();
        self.log.push(Entry::Check(report.to_string()));
        if report.is_ok() {
            return Next::Done;
        }
        if self.fix_rounds == MAX_FIX_ROUNDS {
            return self.fail(format!("the graph still has problems after {MAX_FIX_ROUNDS} attempts to fix them"));
        }
        self.fix_rounds += 1;
        let text = format!("The check found problems. Fix them, or say why you cannot.\n\n{}", state(graph));
        self.messages.push(json!({ "role": "user", "content": text }));
        self.next_request()
    }

    /// Records that the request itself failed (network, key), and stops.
    pub fn failed(&mut self, error: String) {
        self.fail(error);
    }

    fn fail(&mut self, error: String) -> Next {
        self.log.push(Entry::Error(error));
        // A conversation must alternate and end with a reply, so drop the
        // turn that never got one.
        self.messages.clear();
        Next::Done
    }

    fn next_request(&mut self) -> Next {
        if self.requests == MAX_REQUESTS {
            return self.fail(format!("stopped after {MAX_REQUESTS} requests"));
        }
        self.requests += 1;
        Next::Send(json!({
            "model": self.model,
            "max_tokens": 4096,
            "system": system_prompt(),
            "tools": tools(),
            "messages": self.messages,
        }))
    }
}

/// Runs one tool call on the graph: the log entry for people and the text
/// for the model. A `Refused` entry means the call failed and changed nothing.
/// Shared by the in-app assistant and the MCP server.
pub fn run_tool(graph: &mut Graph, name: &str, input: &Value) -> (Entry, String) {
    if name == "check" {
        let report = graph.check();
        return (Entry::Check(report.to_string()), state(graph));
    }
    if name == "write_graph" {
        let text = input["text"].as_str().unwrap_or("");
        return match Graph::from_text(text) {
            Ok(g) => {
                *graph = g;
                let n = graph.node_ids().len();
                (Entry::Edit(format!("wrote a new graph of {n} nodes")), format!("the graph now has {n} nodes"))
            }
            Err(e) => (Entry::Refused(e.clone()), e),
        };
    }
    match command(name, input).and_then(|c| graph.apply(&c)) {
        Ok(msg) => (Entry::Edit(msg.clone()), msg),
        Err(e) => (Entry::Refused(e.clone()), e),
    }
}

/// The graph's text form and its check report, as the model reads them.
pub fn state(graph: &mut Graph) -> String {
    let report = graph.check();
    let text = graph.to_text();
    let text = if text.is_empty() { "(empty)\n".to_string() } else { text };
    format!("Current graph:\n{text}\nCheck:\n{report}")
}

pub fn system_prompt() -> String {
    format!(
        "You design mechanical parts in Solidweave, a parametric CAD system. A part is a dataflow graph of \
nodes; you edit the graph with the tools, never geometry directly.

How graphs work:
- Each line of the text form is one node: `n4 = Extrude(profile=n2.R, height=n3.N)`. An input is either \
wired from another node's output (`n2.R`) or left at its default.
- Every number comes from a Slider, so the user can adjust it later. Add a Slider, set its value and a \
sensible range with set_slider, and wire it in. Inputs left at their default show the default.
- Lengths are in millimetres. Profiles lie on the XY plane; Extrude goes up along +Z.
- Every port carries a list. Feeding a list (from Series) into an input makes one result per item.
- Booleans (SolidUnion, SolidDifference, SolidIntersection) work on solids with planar faces only, and \
must give one connected piece. Use Difference on profiles for round through-holes instead.

Work like this: build the part with the tools (write_graph is quickest for a new part; the other tools \
for changes), then call check and fix every problem it lists. Keep node ids stable when editing. When \
the check is clean, reply with one or two sentences saying what you made and which sliders matter.

Node types:
{}",
        catalog()
    )
}

pub fn tools() -> Value {
    let node = json!({ "type": "string", "description": "A node id such as n3" });
    let pin = |what: &str| json!({ "type": "string", "description": format!("{what}, written node.port, such as n2.R") });
    let tool = |name: &str, description: &str, properties: Value, required: &[&str]| {
        json!({
            "name": name,
            "description": description,
            "input_schema": { "type": "object", "properties": properties, "required": required },
        })
    };
    json!([
        tool(
            "write_graph",
            "Replace the whole graph with one written in the text form, one node per line. Best for building a new part.",
            json!({ "text": { "type": "string", "description": "The graph's text form" } }),
            &["text"],
        ),
        tool("add_node", "Add a node. The reply gives its id and ports.", json!({ "type": { "type": "string", "description": "A node type from the catalog" } }), &["type"]),
        tool("remove_node", "Remove a node and its wires.", json!({ "node": node }), &["node"]),
        tool("connect", "Wire an output into an input, replacing any wire there.", json!({ "from": pin("The output"), "to": pin("The input") }), &["from", "to"]),
        tool("disconnect", "Remove the wire into an input, which goes back to its default.", json!({ "input": pin("The input") }), &["input"]),
        tool(
            "set_slider",
            "Set a Slider's value, and its range if min and max are given.",
            json!({ "node": node, "value": { "type": "number" }, "min": { "type": "number" }, "max": { "type": "number" } }),
            &["node", "value"],
        ),
        tool("check", "Evaluate the graph. Returns its text form, the solids it makes and every problem.", json!({}), &[]),
    ])
}

/// A tool call as a graph command.
pub fn command(name: &str, input: &Value) -> Result<Command, String> {
    let text = |key: &str| input.get(key).and_then(Value::as_str).ok_or_else(|| format!("{name} needs a string '{key}'"));
    let number = |key: &str| input.get(key).and_then(Value::as_f64);
    Ok(match name {
        "add_node" => Command::AddNode { name: text("type")?.to_string() },
        "remove_node" => Command::RemoveNode { node: node_id(text("node")?)? },
        "connect" => {
            let (from, output) = pin(text("from")?)?;
            let (to, input) = pin(text("to")?)?;
            Command::Connect { from, output, to, input }
        }
        "disconnect" => {
            let (node, input) = pin(text("input")?)?;
            Command::Disconnect { node, input }
        }
        "set_slider" => Command::SetSlider {
            node: node_id(text("node")?)?,
            value: number("value").ok_or("set_slider needs a number 'value'")?,
            range: match (number("min"), number("max")) {
                (Some(lo), Some(hi)) => Some((lo, hi)),
                (None, None) => None,
                _ => return Err("give both min and max, or neither".into()),
            },
        },
        _ => return Err(format!("there is no tool '{name}'")),
    })
}

fn node_id(s: &str) -> Result<NodeId, String> {
    let s = s.trim();
    s.strip_prefix('n').unwrap_or(s).parse().map(NodeId).map_err(|_| format!("'{s}' is not a node id like n3"))
}

/// `n2.R` → (n2, "R")
fn pin(s: &str) -> Result<(NodeId, String), String> {
    let (node, port) = s.split_once('.').ok_or_else(|| format!("'{s}' should be node.port, such as n2.R"))?;
    Ok((node_id(node)?, port.trim().to_string()))
}

/// Posts a request body to the Messages API. Blocking; run it off the UI thread.
pub fn send(key: &str, body: &Value) -> Result<Value, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let mut response = agent
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .send_json(body)
        .map_err(|e| format!("request failed: {e}"))?;
    response.body_mut().read_json::<Value>().map_err(|e| format!("could not read the reply: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_use(id: &str, name: &str, input: Value) -> Value {
        json!({ "type": "tool_use", "id": id, "name": name, "input": input })
    }

    fn reply(content: Vec<Value>) -> Value {
        json!({ "role": "assistant", "content": content, "stop_reason": if content.iter().any(|b| b["type"] == "tool_use") { "tool_use" } else { "end_turn" } })
    }

    fn text(t: &str) -> Value {
        json!({ "type": "text", "text": t })
    }

    fn last_message(next: &Next) -> &Value {
        let Next::Send(body) = next else { panic!("expected a request, got {next:?}") };
        body["messages"].as_array().unwrap().last().unwrap()
    }

    const PLATE: &str = "n0 = Slider(value=30, range=1..100)\nn1 = Slider(value=20, range=1..100)\n\
n2 = Rectangle(x=n0.N, y=n1.N)\nn3 = Slider(value=5, range=1..20)\nn4 = Extrude(profile=n2.R, height=n3.N)";

    #[test]
    fn tool_calls_become_commands() {
        let ok = [
            ("add_node", json!({ "type": "Circle" }), Command::AddNode { name: "Circle".into() }),
            ("remove_node", json!({ "node": "n3" }), Command::RemoveNode { node: NodeId(3) }),
            (
                "connect",
                json!({ "from": "n2.R", "to": "4.profile" }),
                Command::Connect { from: NodeId(2), output: "R".into(), to: NodeId(4), input: "profile".into() },
            ),
            ("disconnect", json!({ "input": "n4.height" }), Command::Disconnect { node: NodeId(4), input: "height".into() }),
            ("set_slider", json!({ "node": "n0", "value": 3 }), Command::SetSlider { node: NodeId(0), value: 3.0, range: None }),
            (
                "set_slider",
                json!({ "node": "n0", "value": 3, "min": 0, "max": 9.5 }),
                Command::SetSlider { node: NodeId(0), value: 3.0, range: Some((0.0, 9.5)) },
            ),
        ];
        for (name, input, expected) in ok {
            assert_eq!(command(name, &input), Ok(expected));
        }
        let bad = [
            ("connect", json!({ "from": "n2", "to": "n4.profile" }), "'n2' should be node.port"),
            ("remove_node", json!({ "node": "block" }), "'block' is not a node id"),
            ("set_slider", json!({ "node": "n0", "value": 3, "min": 0 }), "give both min and max"),
            ("set_slider", json!({ "node": "n0" }), "set_slider needs a number 'value'"),
            ("teleport", json!({}), "there is no tool 'teleport'"),
        ];
        for (name, input, expected) in bad {
            let err = command(name, &input).unwrap_err();
            assert!(err.starts_with(expected), "{name}: {err}");
        }
    }

    #[test]
    fn first_request_carries_the_catalog_tools_and_graph() {
        let mut g = Graph::from_text(PLATE).unwrap();
        let next = Assistant::new("test-model").start("make it 8 thick", &mut g);
        let Next::Send(body) = &next else { panic!() };
        assert_eq!(body["model"], "test-model");
        assert!(body["system"].as_str().unwrap().contains("Extrude(profile: Profile, height: Number = 1) -> S: Solid"));
        let tools: Vec<&str> = body["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(tools, ["write_graph", "add_node", "remove_node", "connect", "disconnect", "set_slider", "check"]);
        let prompt = last_message(&next)["content"].as_str().unwrap();
        assert!(prompt.starts_with("make it 8 thick\n\nCurrent graph:\nn0 = Slider"), "{prompt}");
        assert!(prompt.contains("n4 Extrude: solid, volume 3000"), "{prompt}");
    }

    /// A recorded exchange: the model builds a plate, checks it, finishes.
    #[test]
    fn a_recorded_session_builds_a_part() {
        let mut g = Graph::new();
        let mut ai = Assistant::new("m");
        ai.start("a 30 x 20 x 5 plate", &mut g);
        let next = ai.receive(&mut g, &reply(vec![text("Building it."), tool_use("t1", "write_graph", json!({ "text": PLATE }))]));
        let result = &last_message(&next)["content"][0];
        assert_eq!((result["tool_use_id"].as_str(), result["is_error"].as_bool()), (Some("t1"), Some(false)));
        let next = ai.receive(&mut g, &reply(vec![tool_use("t2", "set_slider", json!({ "node": "n3", "value": 8 })), tool_use("t3", "check", json!({}))]));
        let results = last_message(&next)["content"].as_array().unwrap();
        assert_eq!(results.len(), 2, "one result per call, in one message");
        assert!(results[1]["content"].as_str().unwrap().contains("volume 4800"));
        assert_eq!(ai.receive(&mut g, &reply(vec![text("A 30 x 20 x 8 plate.")])), Next::Done);
        assert_eq!(g.result_solids()[0].1.volume(), 4800.0);
        assert!(matches!(&ai.log[..], [
            Entry::Prompt(_), Entry::Reply(_), Entry::Edit(_), Entry::Edit(_), Entry::Check(_), Entry::Reply(_), Entry::Check(_)
        ]), "{:#?}", ai.log);
    }

    #[test]
    fn refused_calls_go_back_as_errors() {
        let mut g = Graph::from_text(PLATE).unwrap();
        let mut ai = Assistant::new("m");
        ai.start("taller", &mut g);
        let next = ai.receive(&mut g, &reply(vec![tool_use("t1", "set_slider", json!({ "node": "n3", "value": 50 }))]));
        let result = &last_message(&next)["content"][0];
        assert_eq!(result["is_error"], true);
        assert!(result["content"].as_str().unwrap().contains("outside n3's range 1..20"));
        assert!(matches!(ai.log.last(), Some(Entry::Refused(_))));
    }

    #[test]
    fn problems_found_at_the_end_go_back_to_be_fixed() {
        let mut g = Graph::new();
        let mut ai = Assistant::new("m");
        ai.start("a block", &mut g);
        // The model forgets to wire the profile, then says it is done.
        ai.receive(&mut g, &reply(vec![tool_use("t1", "add_node", json!({ "type": "Extrude" }))]));
        let next = ai.receive(&mut g, &reply(vec![text("Done.")]));
        let msg = last_message(&next)["content"].as_str().unwrap();
        assert!(msg.starts_with("The check found problems."), "{msg}");
        assert!(msg.contains("n0 Extrude: input 'profile' is not connected"), "{msg}");
        // It fixes the graph and finishes cleanly.
        ai.receive(&mut g, &reply(vec![tool_use("t2", "add_node", json!({ "type": "Rectangle" })), tool_use("t3", "connect", json!({ "from": "n1.R", "to": "n0.profile" }))]));
        assert_eq!(ai.receive(&mut g, &reply(vec![text("Fixed.")])), Next::Done);
        assert!(matches!(ai.log.last(), Some(Entry::Check(r)) if r.contains("Problems:\n  none")));
    }

    #[test]
    fn a_model_that_never_fixes_its_graph_is_stopped() {
        let mut g = Graph::new();
        let mut ai = Assistant::new("m");
        ai.start("anything", &mut g);
        for _ in 0..MAX_FIX_ROUNDS {
            assert!(matches!(ai.receive(&mut g, &reply(vec![text("Done.")])), Next::Send(_)));
        }
        assert_eq!(ai.receive(&mut g, &reply(vec![text("Done.")])), Next::Done);
        assert!(matches!(ai.log.last(), Some(Entry::Error(e)) if e.contains("still has problems")));
    }

    /// Needs the network: `cargo test -p cad_app -- --ignored`. With
    /// ANTHROPIC_API_KEY set it builds a part for real; without, it checks
    /// that a refused key comes back as a readable error.
    #[test]
    #[ignore]
    fn live_session() {
        let mut g = Graph::new();
        let model = std::env::var(MODEL_VAR).unwrap_or_else(|_| DEFAULT_MODEL.into());
        let mut ai = Assistant::new(model);
        let key = std::env::var(KEY_VAR).unwrap_or_else(|_| "not-a-key".into());
        let mut next = ai.start("A 60 x 40 plate, 8 thick, with a row of three 6 mm diameter holes along its middle.", &mut g);
        while let Next::Send(body) = next {
            next = match send(&key, &body) {
                Ok(reply) => ai.receive(&mut g, &reply),
                Err(e) => panic!("{e}"),
            };
        }
        for entry in &ai.log {
            println!("{entry:?}");
        }
        if std::env::var(KEY_VAR).is_err() {
            assert!(matches!(ai.log.last(), Some(Entry::Error(e)) if e.starts_with("API error:")), "{:?}", ai.log.last());
            return;
        }
        let report = g.check();
        assert!(report.is_ok(), "{report}");
        let pi = std::f64::consts::PI;
        let volume: f64 = g.result_solids().iter().map(|(_, s)| s.volume()).sum();
        assert!((volume - 8.0 * (2400.0 - 3.0 * pi * 9.0)).abs() < 1e-6, "volume {volume}\n{}", g.to_text());
    }

    #[test]
    fn api_errors_end_the_turn_and_leave_a_clean_conversation() {
        let mut g = Graph::new();
        let mut ai = Assistant::new("m");
        ai.start("anything", &mut g);
        let error = json!({ "type": "error", "error": { "type": "authentication_error", "message": "invalid x-api-key" } });
        assert_eq!(ai.receive(&mut g, &error), Next::Done);
        assert_eq!(ai.log.last(), Some(&Entry::Error("API error: invalid x-api-key".into())));
        // The next prompt starts a fresh, valid conversation.
        let next = ai.start("again", &mut g);
        let Next::Send(body) = next else { panic!() };
        assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    }
}
