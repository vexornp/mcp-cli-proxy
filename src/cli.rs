use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mcp-cli-proxy", version, about = "Generic-executor MCP server: runs shell commands on the host PC, outside the agent sandbox")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the stdio MCP server (default). Forwards exec_command calls to the
    /// daemon over 127.0.0.1:8130 — start `mcp-cli-proxy daemon` first.
    Serve,
    /// Run the unsandboxed exec daemon. Start this in a separate terminal
    /// before launching the agent. Binds 127.0.0.1:8130 and runs the
    /// shell commands the bridge forwards to it.
    Daemon,
}

pub async fn run(cmd: Option<Command>) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        None | Some(Command::Serve) => crate::server::run_server().await,
        Some(Command::Daemon) => {
            let opts = crate::bridge::DaemonOptions::defaults();
            crate::daemon::run_daemon(opts, None)
                .await
                .map_err(|e| -> Box<dyn std::error::Error> { e })
        }
    }
}
