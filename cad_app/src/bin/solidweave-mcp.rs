//! Solidweave's MCP server over stdio. Register it with an MCP client, for
//! example Claude Code: `claude mcp add solidweave -- solidweave-mcp`, or
//! through the `.mcp.json` at the repository root.

fn main() -> std::io::Result<()> {
    cad_app::mcp::serve(std::io::stdin().lock(), std::io::stdout().lock())
}
