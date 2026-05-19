//! Binary entry point for the `rho` coding agent.
//!
//! Thin orchestration only: parse CLI, build the application, and run.
//! All setup logic lives in [`rho::app::App`].

use clap::Parser;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = rho::cli::Cli::parse();
    rho::app::App::build(cli).await?.run().await
}
