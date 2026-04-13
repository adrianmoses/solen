use anyhow::Result;
use tracing_subscriber::EnvFilter;

use edgeclaw_server::startup::{run_server, RunOptions};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    run_server(RunOptions::default()).await
}
