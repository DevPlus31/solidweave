//! The parts of Solidweave's editor that need no window: the document
//! (examples, file format, undo, export), the AI assistant's conversation
//! logic, and the MCP server that lets an outside AI client (Claude Code,
//! Claude Desktop) edit a graph. The `cad_app` binary adds the egui window;
//! `solidweave-mcp` serves MCP over stdio.

pub mod ai;
pub mod document;
pub mod mcp;
