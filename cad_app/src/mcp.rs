//! An MCP server, so an AI client such as Claude Code can design parts.
//!
//! MCP (Model Context Protocol) is JSON-RPC 2.0, one message per line over
//! stdin/stdout. The server offers the same tools as the in-app assistant
//! (`ai::tools`, carried out by `ai::run_tool`), so the client edits the
//! graph through node_graph's commands and never touches geometry. On top of
//! those: `open` a `.swv` file, `get_graph`, `guide` (how graphs work and
//! the node catalog) and `export` to `.usda` or `.stl`.
//!
//! With a file open, every edit is saved to it at once, and the editor
//! showing that file reloads it, so a person can watch and adjust the part
//! while the AI works. Edits the person saves in the editor are read back
//! before the next tool call. Run it as `solidweave-mcp`; see `.mcp.json`.

use crate::ai::{self, Entry};
use crate::document::{Positions, load_text, result_solids, save_text, stl, usda};
use node_graph::Graph;
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::SystemTime;

/// The protocol version used when the client does not ask for one.
const PROTOCOL_VERSION: &str = "2025-06-18";

pub struct Server {
    graph: Graph,
    positions: Positions,
    /// The open file, and its modification time when last read or written.
    file: Option<(PathBuf, Option<SystemTime>)>,
}

impl Default for Server {
    fn default() -> Self {
        Self { graph: Graph::new(), positions: Positions::new(), file: None }
    }
}

impl Server {
    /// Answers one message; `None` for notifications, which get no reply.
    pub fn handle(&mut self, msg: &Value) -> Option<Value> {
        let id = msg.get("id")?.clone();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result = match msg.get("method").and_then(Value::as_str).unwrap_or("") {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSION),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "solidweave", "version": env!("CARGO_PKG_VERSION") },
                "instructions": instructions(),
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools() })),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let (ok, text) = self.call(name, &args);
                Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": !ok }))
            }
            other => Err(json!({ "code": -32601, "message": format!("method not found: {other}") })),
        };
        Some(match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err(e) => json!({ "jsonrpc": "2.0", "id": id, "error": e }),
        })
    }

    /// Runs one tool. Returns (success, text for the client).
    fn call(&mut self, name: &str, args: &Value) -> (bool, String) {
        let path_arg = || args.get("path").and_then(Value::as_str).map(PathBuf::from).ok_or_else(|| format!("{name} needs a string 'path'"));
        match name {
            "open" => match path_arg().and_then(|p| self.open(p)) {
                Ok(text) => (true, text),
                Err(e) => (false, e),
            },
            "guide" => (true, instructions()),
            "export" => match path_arg().and_then(|p| self.export(p)) {
                Ok(text) => (true, text),
                Err(e) => (false, e),
            },
            _ => {
                if let Err(e) = self.reload_if_changed() {
                    return (false, e);
                }
                if name == "get_graph" {
                    return (true, self.describe());
                }
                let (entry, mut text) = ai::run_tool(&mut self.graph, name, args);
                match entry {
                    Entry::Refused(_) => return (false, text),
                    Entry::Edit(_) => {
                        if name == "write_graph" {
                            self.positions.clear(); // new nodes; the editor lays them out
                        }
                        match self.save() {
                            Ok(Some(path)) => text.push_str(&format!(" (saved to {})", path.display())),
                            Ok(None) => text.push_str(" (in memory only: call open to work on a file the editor can show)"),
                            Err(e) => return (false, format!("{text}, but saving failed: {e}")),
                        }
                    }
                    _ => {}
                }
                (true, text)
            }
        }
    }

    /// Opens a graph file, or starts an empty one there if it does not exist.
    fn open(&mut self, path: PathBuf) -> Result<String, String> {
        if path.exists() {
            let text = std::fs::read_to_string(&path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
            (self.graph, self.positions) = load_text(&text).map_err(|e| format!("could not open {}: {e}", path.display()))?;
            self.file = Some((path.clone(), modified(&path)));
        } else {
            (self.graph, self.positions) = (Graph::new(), Positions::new());
            self.file = Some((path.clone(), None));
            self.save()?;
        }
        Ok(self.describe())
    }

    fn export(&mut self, path: PathBuf) -> Result<String, String> {
        self.reload_if_changed()?;
        self.graph.evaluate();
        let solids: Vec<_> = result_solids(&self.graph).into_iter().map(|(_, s)| s).collect();
        if solids.is_empty() {
            return Err("nothing to export: the graph makes no solids".into());
        }
        let text = match path.extension().and_then(|e| e.to_str()) {
            Some("usda") => usda(&solids),
            Some("stl") => stl(&solids),
            _ => return Err("export to a .usda or .stl path".into()),
        };
        std::fs::write(&path, text).map_err(|e| format!("could not write {}: {e}", path.display()))?;
        Ok(format!("exported {} solids to {}", solids.len(), path.display()))
    }

    /// Writes the graph to the open file, if any.
    fn save(&mut self) -> Result<Option<PathBuf>, String> {
        let Some((path, stamp)) = &mut self.file else { return Ok(None) };
        std::fs::write(&*path, save_text(&self.graph, &self.positions)).map_err(|e| format!("could not write {}: {e}", path.display()))?;
        *stamp = modified(path);
        Ok(Some(path.clone()))
    }

    /// Picks up edits saved to the open file by someone else (the editor).
    fn reload_if_changed(&mut self) -> Result<(), String> {
        let Some((path, stamp)) = &self.file else { return Ok(()) };
        let now = modified(path);
        if now.is_none() || now == *stamp {
            return Ok(());
        }
        let path = path.clone();
        self.open(path).map(|_| ())
    }

    fn describe(&mut self) -> String {
        let file = self.file.as_ref().map_or("no file open".to_string(), |(p, _)| format!("file: {}", p.display()));
        format!("{file}\n{}", ai::state(&mut self.graph))
    }
}

fn modified(path: &PathBuf) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn instructions() -> String {
    format!(
        "Solidweave tools. Call `open` with a .swv path first: every edit is saved there, and the Solidweave \
editor (`cargo run -p cad_app`, File → Open) shows that file and reloads it on every change, so the user \
can watch. `export` writes the result as OpenUSD BrepArray (.usda) or STL.\n\n{}",
        ai::system_prompt()
    )
}

/// The assistant's tools in MCP's spelling, plus the file tools.
fn tools() -> Vec<Value> {
    let path = |what: &str| json!({ "type": "object", "properties": { "path": { "type": "string", "description": what } }, "required": ["path"] });
    let mut tools: Vec<Value> = ai::tools()
        .as_array()
        .expect("a list")
        .iter()
        .map(|t| json!({ "name": t["name"], "description": t["description"], "inputSchema": t["input_schema"] }))
        .collect();
    tools.extend([
        json!({ "name": "open", "description": "Open a Solidweave graph file (.swv), or start an empty one at that path. Later edits are saved to it.", "inputSchema": path("Path of the .swv file") }),
        json!({ "name": "get_graph", "description": "The graph's text form and its check report.", "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "guide", "description": "How Solidweave graphs work, and every node type with its ports.", "inputSchema": { "type": "object", "properties": {} } }),
        json!({ "name": "export", "description": "Write the graph's result solids to a .usda (OpenUSD BrepArray) or .stl file.", "inputSchema": path("Path ending in .usda or .stl") }),
    ]);
    tools
}

/// Serves MCP until the input ends.
pub fn serve(input: impl BufRead, mut output: impl Write) -> io::Result<()> {
    let mut server = Server::default();
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => server.handle(&msg),
            Err(e) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("parse error: {e}") } })),
        };
        if let Some(r) = reply {
            writeln!(output, "{r}")?;
            output.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(server: &mut Server, name: &str, arguments: Value) -> (bool, String) {
        let reply = server.handle(&json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": { "name": name, "arguments": arguments } })).unwrap();
        let r = &reply["result"];
        (!r["isError"].as_bool().unwrap(), r["content"][0]["text"].as_str().unwrap().to_string())
    }

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("solidweave-mcp-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("part.swv");
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn handshake_and_tool_list() {
        let mut s = Server::default();
        let init = s.handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-03-26" } })).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "solidweave");
        assert!(init["result"]["instructions"].as_str().unwrap().contains("SolidDifference(A: Solid, B: Solid)"));
        assert_eq!(s.handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })), None);
        let list = s.handle(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })).unwrap();
        let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["write_graph", "add_node", "remove_node", "connect", "disconnect", "set_slider", "check", "open", "get_graph", "guide", "export"]);
        assert!(list["result"]["tools"].as_array().unwrap().iter().all(|t| t["inputSchema"]["type"] == "object"));
        let unknown = s.handle(&json!({ "jsonrpc": "2.0", "id": 3, "method": "resources/list" })).unwrap();
        assert_eq!(unknown["error"]["code"], -32601);
    }

    #[test]
    fn edits_are_saved_to_the_open_file() {
        let path = temp_file("save");
        let mut s = Server::default();
        let (ok, text) = call(&mut s, "open", json!({ "path": path }));
        assert!(ok && text.contains("(empty)"), "{text}");
        assert!(path.exists(), "a new file is created at once, so the editor can open it");
        let (ok, text) = call(&mut s, "add_node", json!({ "type": "Sphere" }));
        assert!(ok && text.contains("(saved to"), "{text}");
        let (graph, _) = load_text(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(graph.node_ids().len(), 1);
        // Refused edits change nothing and say why.
        let (ok, text) = call(&mut s, "connect", json!({ "from": "n0.S", "to": "n9.solid" }));
        assert!(!ok && text.starts_with("there is no node n9"), "{text}");
        // The result exports.
        let usda_path = path.with_extension("usda");
        let (ok, text) = call(&mut s, "export", json!({ "path": usda_path }));
        assert!(ok, "{text}");
        let breps = usd_brep::BrepArray::read_all(&std::fs::read_to_string(&usda_path).unwrap()).unwrap();
        assert!(usd_brep::validate(&breps[0]).is_empty());
    }

    #[test]
    fn edits_saved_by_the_editor_are_picked_up() {
        let path = temp_file("reload");
        let mut s = Server::default();
        call(&mut s, "open", json!({ "path": path }));
        call(&mut s, "write_graph", json!({ "text": "n0 = Slider(value=2, range=0..5)\nn1 = Sphere(radius=n0.N)" }));
        // The user drags the slider in the editor and saves. Wait so the
        // file's time stamp moves on even on coarse file systems.
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(&path, "n0 = Slider(value=3, range=0..5)\nn1 = Sphere(radius=n0.N)\n").unwrap();
        let (_, text) = call(&mut s, "get_graph", json!({}));
        assert!(text.contains("n0 = Slider(value=3, range=0..5)"), "{text}");
    }

    #[test]
    fn without_a_file_edits_stay_in_memory_and_say_so() {
        let mut s = Server::default();
        let (ok, text) = call(&mut s, "add_node", json!({ "type": "Rectangle" }));
        assert!(ok && text.ends_with("(in memory only: call open to work on a file the editor can show)"), "{text}");
        let (ok, text) = call(&mut s, "export", json!({ "path": "part.step" }));
        assert!(!ok && text == "nothing to export: the graph makes no solids", "{text}");
    }

    #[test]
    fn serve_reads_and_writes_one_message_per_line() {
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n\nnot json\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n";
        let mut out = Vec::new();
        serve(&input[..], &mut out).unwrap();
        let lines: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], json!({ "jsonrpc": "2.0", "id": 1, "result": {} }));
        assert_eq!(lines[1]["error"]["code"], -32700);
    }
}
