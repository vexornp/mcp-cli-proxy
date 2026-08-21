use clap::Parser;
use mcp_cli_proxy::cli::{Cli, run};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli.command).await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
