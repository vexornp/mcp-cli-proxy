use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mcp-cli-proxy", version, about = "Generic-executor MCP server: runs shell commands on the host PC, outside the agent sandbox")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the stdio MCP server (default).
    Serve,
}

pub async fn run(cmd: Option<Command>) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        None | Some(Command::Serve) => crate::server::run_server().await,
    }
}
