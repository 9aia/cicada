//! `cicada-mcp` — MCP server over stdio. Thin adapter exposing
//! cicada-core use cases as MCP tools for agents (ADR-0001 §8a).

#![forbid(unsafe_code)]

use std::process::ExitCode;

use clap::Parser;

/// MCP server exposing cicada's plan/apply/drift lifecycle to agents
/// over stdio. Working root: `--root <dir>` or `CICADA_ROOT` or cwd.
#[derive(Debug, Parser)]
#[command(
    name = "cicada-mcp",
    version,
    about = "cicada MCP server — calendars as code, for agents (stdio)"
)]
struct Cli {
    /// Server options shared with `cicada mcp`.
    #[command(flatten)]
    opts: cicada_mcp::ServerOptions,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    cicada_mcp::run_stdio(&cli.opts)
}
