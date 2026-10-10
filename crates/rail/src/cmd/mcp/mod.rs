//! `rail mcp`: an MCP server on stdin and stdout for AI coding agents. Every tool is one daemon
//! operation. Only MCP frames go to stdout; logs go to `rail.log`.

mod confirm;
mod guarded;
mod reply;
mod server;
mod summary;

use std::process::ExitCode;

use color_eyre::eyre::eyre;
use rail_core::paths::Paths;
use rmcp::ServiceExt;
use rmcp::transport::stdio;

use crate::daemon_link;

use server::RailMcp;

/// Serve until the client closes stdin.
pub async fn run(paths: &Paths) -> color_eyre::Result<ExitCode> {
    let server = RailMcp::new(daemon_link::connector(paths));
    let running = server
        .serve(stdio())
        .await
        .map_err(|e| eyre!("could not start the MCP server: {e}"))?;
    running
        .waiting()
        .await
        .map_err(|e| eyre!("the MCP server stopped: {e}"))?;
    Ok(ExitCode::SUCCESS)
}
