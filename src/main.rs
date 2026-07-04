use anyhow::Result;
use clap::Parser;
use smarter_recipes::cli::{run, Cli};
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    // Load .env early so keys like SMARTER_RECIPES_FDC_KEY are available.
    smarter_recipes::dotenv::load();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    run(cli)
}
