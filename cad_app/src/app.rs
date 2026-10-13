//! The window: menu bar, node canvas on the left, 3D preview in the middle,
//! the AI assistant on the right (hidden until View → AI assistant), the graph's text form (what an AI reads)
//! and status at the bottom.
//!
//! The open file is watched: when something else changes it (the MCP
//! server, while an AI client works on it), the editor reloads it, as one
//! undo step. If there are unsaved edits it only says so.
//!
//! Undo works on snapshots of the graph's text. While the pointer is held
//! (dragging a slider, say) the graph may change every frame; that whole
//! gesture records one undo step.

use crate::ai::{self, Assistant, Entry, Next};
use crate::document::{EXAMPLES, EXTENSION, History, Positions, Snapshot, graph_text, load_text, result_solids, save_text, stl, usda};
use crate::editor::{GraphViewer, canvas_positions, snarl_from_graph};
use crate::viewport::{self, Camera, Scene};
use egui::{Button, Key, KeyboardShortcut, Modifiers};
use egui_snarl::Snarl;
use egui_snarl::ui::{SnarlStyle, get_selected_nodes};
use node_graph::{Graph, NodeId};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, SystemTime};

/// Something the user asked for from a menu or a shortcut, carried out
/// after the frame's UI so it never changes the graph mid-draw.
#[derive(Clone, Copy)]
enum Action {
    New,
    Open,
    Save,
    SaveAs,
    ExportUsda,
    ExportStl,
    Undo,
    Redo,
    Example(usize),
    FitView,
    ToggleAssistant,
}

pub struct CadApp {
    graph: Graph,
    canvas: Snarl<NodeId>,
    camera: Camera,
    scene: Scene,
    /// The 3D pipeline is set up on the GPU, so the preview has a depth buffer.
    gpu: bool,
    history: History,
    /// The file last opened or saved.
    path: Option<PathBuf>,
    /// Changed since it was last opened or saved.
    modified: bool,
    /// The file's modification time when last opened, saved or reloaded.
    file_stamp: Option<SystemTime>,
    /// Graph nodes selected on the canvas.
    selected: Vec<NodeId>,
    /// A pointer gesture is changing the graph; it has recorded its undo step.
    gesture_open: bool,
    /// Why the last edit was refused.
    error: Option<String>,
    /// Outcome of the last file action.
    notice: Option<String>,
    title: String,
    /// The AI assistant panel is shown. Hidden until asked for from View.
    show_assistant: bool,
    assistant: Assistant,
    /// The API key, from the environment.
    api_key: Option<String>,
    /// What the user is typing to the assistant.
    prompt: String,
    /// The reply the assistant is waiting for.
    pending: Option<Receiver<Result<Value, String>>>,
    /// The document before the assistant's current turn, until its first
    /// edit records it as one undo step.
    before_ai: Option<Snapshot>,
}

const CTRL: Modifiers = Modifiers::COMMAND;
const CTRL_SHIFT: Modifiers = Modifiers { shift: true, ..Modifiers::COMMAND };

impl CadApp {
    /// Opens `file` if given (a `.swv` that does not exist yet starts
    /// empty), otherwise the first example.
    pub fn new(file: Option<PathBuf>) -> Self {
        let (graph, positions) = load_text(EXAMPLES[0].1).expect("examples are tested");
        let mut app = CadApp {
            canvas: snarl_from_graph(&graph, &positions),
            graph,
            camera: Camera::default(),
            gpu: false,
            scene: Scene::default(),
            history: History::default(),
            path: None,
            modified: false,
            file_stamp: None,
            selected: Vec::new(),
            gesture_open: false,
            error: None,
            notice: None,
            title: String::new(),
            show_assistant: false,
            assistant: Assistant::new(std::env::var(ai::MODEL_VAR).unwrap_or_else(|_| ai::DEFAULT_MODEL.into())),
            api_key: std::env::var(ai::KEY_VAR).ok().filter(|k| !k.trim().is_empty()),
            prompt: String::new(),
            pending: None,
            before_ai: None,
        };
        app.graph.evaluate();
        app.refresh_scene();
        app.camera.fit(&app.scene);
        if let Some(path) = file {
            let text = if path.exists() { std::fs::read_to_string(&path).map_err(|e| e.to_string()) } else { Ok(String::new()) };
            match text.and_then(|t| load_text(&t)) {
                Ok((graph, positions)) => app.load(graph, positions, Some(path)),
                Err(e) => app.notice = Some(format!("could not open {}: {e}", path.display())),
            }
        }
        app
    }

    /// Replaces the document: a new graph, layout and file, no history.
    fn load(&mut self, graph: Graph, positions: Positions, path: Option<PathBuf>) {
        self.canvas = snarl_from_graph(&graph, &positions);
        self.graph = graph;
        self.graph.evaluate();
        self.path = path;
        self.file_stamp = self.path.as_deref().and_then(modified_time);
        self.modified = false;
        self.history.clear();
        self.selected.clear();
        self.error = None;
        self.refresh_scene();
        self.camera.fit(&self.scene);
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { text: graph_text(&self.graph), positions: canvas_positions(&self.canvas) }
    }

    /// Goes back (or forward) to a snapshot by rebuilding the graph from its text.
    fn restore(&mut self, s: Snapshot) {
        match Graph::from_text(&s.text) {
            Ok(graph) => {
                self.canvas = snarl_from_graph(&graph, &s.positions);
                self.graph = graph;
                self.graph.evaluate();
                self.modified = true;
                self.selected.clear();
                self.refresh_scene();
            }
            Err(e) => self.notice = Some(format!("could not restore: {e}")),
        }
    }

    fn refresh_scene(&mut self) {
        self.scene = Scene::from_graph(&self.graph, &self.selected);
    }

    /// True if there is nothing unsaved, or the user agrees to lose it.
    fn may_discard(&self) -> bool {
        !self.modified
            || rfd::MessageDialog::new()
                .set_title("Solidweave")
                .set_description("Discard the unsaved changes?")
                .set_buttons(rfd::MessageButtons::YesNo)
                .show()
                == rfd::MessageDialogResult::Yes
    }

    fn file_name(&self) -> String {
        self.path.as_ref().and_then(|p| p.file_name()).map_or("untitled".into(), |n| n.to_string_lossy().into_owned())
    }

    fn write(&mut self, path: PathBuf, contents: String, what: &str) -> bool {
        match std::fs::write(&path, contents) {
            Ok(()) => {
                self.notice = Some(format!("{what} {}", path.display()));
                true
            }
            Err(e) => {
                self.notice = Some(format!("could not write {}: {e}", path.display()));
                false
            }
        }
    }

    /// Reloads the open file if something else changed it.
    fn watch_file(&mut self, ctx: &egui::Context) {
        let Some(path) = self.path.clone() else { return };
        ctx.request_repaint_after(Duration::from_millis(500));
        let now = modified_time(&path);
        if now.is_none() || now == self.file_stamp {
            return;
        }
        self.file_stamp = now;
        if self.modified {
            self.notice = Some(format!("{} changed on disk; save to keep your version, or reopen it", path.display()));
            return;
        }
        let loaded = std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| load_text(&t));
        match loaded {
            Ok((graph, positions)) => {
                let before = self.snapshot();
                if graph_text(&graph) == before.text && positions == before.positions {
                    return;
                }
                self.history.record(before);
                self.canvas = snarl_from_graph(&graph, &positions);
                self.graph = graph;
                self.graph.evaluate();
                self.selected.clear();
                self.refresh_scene();
                self.notice = Some(format!("reloaded {}: it changed on disk", path.display()));
            }
            // Caught mid-write, perhaps: try again next time.
            Err(_) => self.file_stamp = None,
        }
    }

    // ───────────────────────── AI assistant ─────────────────────────

    fn ask_ai(&mut self, ctx: &egui::Context) {
        let prompt = std::mem::take(&mut self.prompt);
        self.before_ai = Some(self.snapshot());
        let next = self.assistant.start(prompt.trim(), &mut self.graph);
        self.ai_step(next, ctx);
    }

    /// Sends the next request on a background thread, or finishes the turn.
    fn ai_step(&mut self, next: Next, ctx: &egui::Context) {
        let (Next::Send(body), Some(key)) = (next, self.api_key.clone()) else {
            self.pending = None;
            self.before_ai = None;
            return;
        };
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(ai::send(&key, &body));
            ctx.request_repaint();
        });
        self.pending = Some(rx);
    }

    /// Applies a reply that has arrived. The assistant's edits go through
    /// the same command API as the canvas; its whole turn is one undo step.
    fn poll_ai(&mut self, ctx: &egui::Context) {
        let Some(reply) = self.pending.as_ref().and_then(|rx| rx.try_recv().ok()) else { return };
        let before = graph_text(&self.graph);
        let next = match reply {
            Ok(v) => self.assistant.receive(&mut self.graph, &v),
            Err(e) => {
                self.assistant.failed(e);
                Next::Done
            }
        };
        if graph_text(&self.graph) != before {
            if let Some(b) = self.before_ai.take() {
                self.history.record(b);
            }
            self.canvas = snarl_from_graph(&self.graph, &canvas_positions(&self.canvas));
            self.graph.evaluate();
            self.modified = true;
            self.refresh_scene();
        }
        self.ai_step(next, ctx);
    }

    /// Draws the preview on the GPU; call after [`viewport::init_gpu`].
    pub fn use_gpu(&mut self) {
        self.gpu = true;
    }

    fn ai_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("AI assistant");
        if self.api_key.is_none() {
            ui.label(format!("Set the {} environment variable to your Anthropic API key and restart.", ai::KEY_VAR));
            return;
        }
        ui.add(egui::TextEdit::multiline(&mut self.prompt).hint_text("Describe a part or a change, e.g. a 60 x 40 plate, 8 thick, with four 6 mm holes").desired_rows(3).desired_width(f32::INFINITY));
        ui.horizontal(|ui| {
            let busy = self.pending.is_some();
            if ui.add_enabled(!busy && !self.prompt.trim().is_empty(), Button::new("Send")).clicked() {
                self.ask_ai(ui.ctx());
            }
            if busy {
                ui.spinner();
                if ui.button("Stop").clicked() {
                    self.pending = None;
                    self.before_ai = None;
                    self.assistant.failed("stopped".into());
                }
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
            for entry in &self.assistant.log {
                match entry {
                    Entry::Prompt(t) => ui.strong(format!("You: {t}")),
                    Entry::Reply(t) => ui.label(t),
                    Entry::Edit(t) => ui.colored_label(egui::Color32::from_rgb(120, 200, 140), format!("✔ {t}")),
                    Entry::Refused(t) => ui.colored_label(egui::Color32::from_rgb(240, 180, 90), format!("✘ {t}")),
                    Entry::Check(t) => ui.monospace(t),
                    Entry::Error(t) => ui.colored_label(egui::Color32::from_rgb(255, 120, 110), t),
                };
            }
        });
    }

    fn run(&mut self, action: Action) {
        let graph_files = || rfd::FileDialog::new().add_filter("Solidweave graph", &[EXTENSION]);
        match action {
            Action::New if self.may_discard() => self.load(Graph::new(), Positions::new(), None),
            Action::Open if self.may_discard() => {
                let Some(path) = graph_files().pick_file() else { return };
                match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| load_text(&t)) {
                    Ok((graph, positions)) => {
                        self.load(graph, positions, Some(path.clone()));
                        self.notice = Some(format!("opened {}", path.display()));
                    }
                    Err(e) => self.notice = Some(format!("could not open {}: {e}", path.display())),
                }
            }
            Action::Save if self.path.is_some() => {
                let path = self.path.clone().unwrap();
                let text = save_text(&self.graph, &canvas_positions(&self.canvas));
                if self.write(path, text, "saved") {
                    self.modified = false;
                    self.file_stamp = self.path.as_deref().and_then(modified_time);
                }
            }
            Action::Save | Action::SaveAs => {
                let Some(path) = graph_files().set_file_name(format!("part.{EXTENSION}")).save_file() else { return };
                let text = save_text(&self.graph, &canvas_positions(&self.canvas));
                if self.write(path.clone(), text, "saved") {
                    self.file_stamp = modified_time(&path);
                    self.path = Some(path);
                    self.modified = false;
                }
            }
            Action::ExportUsda | Action::ExportStl => {
                let solids: Vec<_> = result_solids(&self.graph).into_iter().map(|(_, s)| s).collect();
                if solids.is_empty() {
                    self.notice = Some("nothing to export: the graph makes no solids".into());
                    return;
                }
                let (kind, ext, text) = match action {
                    Action::ExportUsda => ("OpenUSD BrepArray", "usda", usda(&solids)),
                    _ => ("STL mesh", "stl", stl(&solids)),
                };
                let dialog = rfd::FileDialog::new().add_filter(kind, &[ext]).set_file_name(format!("part.{ext}"));
                if let Some(path) = dialog.save_file() {
                    self.write(path, text, &format!("exported {} solids to", solids.len()));
                }
            }
            Action::Undo => {
                let current = self.snapshot();
                if let Some(s) = self.history.undo(current) {
                    self.restore(s);
                }
            }
            Action::Redo => {
                let current = self.snapshot();
                if let Some(s) = self.history.redo(current) {
                    self.restore(s);
                }
            }
            Action::Example(i) if self.may_discard() => {
                let (graph, positions) = load_text(EXAMPLES[i].1).expect("examples are tested");
                self.load(graph, positions, None);
            }
            Action::FitView => self.camera.fit(&self.scene),
            Action::ToggleAssistant => self.show_assistant = !self.show_assistant,
            // A New, Open or Example the user chose not to discard work for.
            _ => {}
        }
    }

    fn shortcuts(&self, ctx: &egui::Context) -> Option<Action> {
        // Ctrl+Shift+Z before Ctrl+Z, so the longer one wins.
        let keys = [
            (CTRL_SHIFT, Key::Z, Action::Redo),
            (CTRL, Key::Z, Action::Undo),
            (CTRL, Key::Y, Action::Redo),
            (CTRL_SHIFT, Key::S, Action::SaveAs),
            (CTRL, Key::S, Action::Save),
            (CTRL, Key::O, Action::Open),
            (CTRL, Key::N, Action::New),
            (CTRL, Key::F, Action::FitView),
        ];
        ctx.input_mut(|i| keys.into_iter().find(|&(m, k, _)| i.consume_shortcut(&KeyboardShortcut::new(m, k))).map(|(.., a)| a))
    }

    fn menu_bar(&self, ui: &mut egui::Ui) -> Option<Action> {
        let mut action = None;
        let mut item = |ui: &mut egui::Ui, label: &str, keys: &str, enabled: bool, a: Action| {
            if ui.add_enabled(enabled, Button::new(label).shortcut_text(keys)).clicked() {
                action = Some(a);
                ui.close();
            }
        };
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                item(ui, "New", "Ctrl+N", true, Action::New);
                item(ui, "Open…", "Ctrl+O", true, Action::Open);
                item(ui, "Save", "Ctrl+S", true, Action::Save);
                item(ui, "Save As…", "Ctrl+Shift+S", true, Action::SaveAs);
                ui.separator();
                item(ui, "Export OpenUSD (.usda)…", "", true, Action::ExportUsda);
                item(ui, "Export STL mesh (.stl)…", "", true, Action::ExportStl);
                ui.separator();
                ui.menu_button("Examples", |ui| {
                    for (i, (name, _)) in EXAMPLES.iter().enumerate() {
                        item(ui, name, "", true, Action::Example(i));
                    }
                });
            });
            ui.menu_button("Edit", |ui| {
                item(ui, "Undo", "Ctrl+Z", self.history.can_undo(), Action::Undo);
                item(ui, "Redo", "Ctrl+Y", self.history.can_redo(), Action::Redo);
            });
            ui.menu_button("View", |ui| {
                item(ui, "Fit to view", "Ctrl+F", true, Action::FitView);
                let assistant = if self.show_assistant { "✔ AI assistant" } else { "AI assistant" };
                item(ui, assistant, "", true, Action::ToggleAssistant);
            });
        });
        action
    }
}

impl eframe::App for CadApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_ai(&ctx);
        self.watch_file(&ctx);
        self.graph.evaluate();
        let mut scene_dirty = self.graph.last_solve_count > 0;
        let mut action = self.shortcuts(&ctx);

        egui::Panel::top("menu").show(ui, |ui| action = self.menu_bar(ui).or(action));

        egui::Panel::bottom("status").resizable(true).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(format!("{}{}", self.file_name(), if self.modified { " (modified)" } else { "" }));
                ui.separator();
                ui.label(format!("kernel: {}", self.graph.kernel().name()));
                ui.separator();
                ui.label(format!("{} nodes, {} wires", self.graph.node_ids().len(), self.graph.wires().len()));
                if let Some(e) = &self.error {
                    ui.separator();
                    ui.colored_label(egui::Color32::from_rgb(255, 120, 110), e);
                }
                if let Some(n) = &self.notice {
                    ui.separator();
                    ui.label(n);
                }
            });
            egui::CollapsingHeader::new("Text form (what the AI reads; also the file format)").default_open(true).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                    ui.monospace(self.graph.to_text());
                });
            });
        });

        egui::Panel::left("canvas").resizable(true).default_size(700.0).show(ui, |ui| {
            ui.label("Right-click empty space to add a node, a node to remove it. Drag between pins to wire. Click a node to highlight its solids.");
            let before = self.snapshot();
            let mut viewer = GraphViewer { graph: &mut self.graph, message: &mut self.error };
            self.canvas.show(&mut viewer, &SnarlStyle::new(), "graph", ui);
            // An edit changed the graph: one undo step per pointer gesture.
            if graph_text(&self.graph) != before.text {
                if !self.gesture_open {
                    self.history.record(before);
                    self.gesture_open = true;
                }
                self.modified = true;
                scene_dirty = true;
            }
            if !ui.input(|i| i.pointer.any_down()) {
                self.gesture_open = false;
            }
            let selected: Vec<NodeId> = get_selected_nodes(ui.make_persistent_id("graph"), ui.ctx())
                .into_iter()
                .filter_map(|n| self.canvas.get_node(n).copied())
                .collect();
            if selected != self.selected {
                self.selected = selected;
                scene_dirty = true;
            }
        });

        if self.show_assistant {
            egui::Panel::right("ai").resizable(true).default_size(340.0).show(ui, |ui| self.ai_panel(ui));
        }

        // Apply this frame's edits before drawing the preview.
        self.graph.evaluate();
        if scene_dirty || self.graph.last_solve_count > 0 {
            self.refresh_scene();
        }
        egui::CentralPanel::default().show(ui, |ui| viewport::show(ui, &mut self.camera, &self.scene, self.gpu));

        if let Some(a) = action {
            self.run(a);
        }
        let title = format!("Solidweave — {}{}", self.file_name(), if self.modified { " *" } else { "" });
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

fn modified_time(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_assistant_panel_starts_hidden_and_view_toggles_it() {
        let mut app = CadApp::new(None);
        assert!(!app.show_assistant);
        app.run(Action::ToggleAssistant);
        assert!(app.show_assistant);
        app.run(Action::ToggleAssistant);
        assert!(!app.show_assistant);
    }
}
