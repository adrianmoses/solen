mod chat;

use anyhow::Result;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use edgeclaw_server::startup::{run_server, RunOptions};

#[derive(Parser)]
#[command(name = "edgeclaw", version, about = "EdgeClaw agent runtime CLI")]
struct Cli {
    /// Log level: error, warn, info, debug, trace
    #[arg(long, default_value = "info", global = true)]
    log_level: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the EdgeClaw WebSocket server
    Serve(ServeArgs),
    /// Open a chat session with the agent
    Chat(ChatArgs),
}

#[derive(clap::Args)]
struct ServeArgs {
    /// Port to listen on
    #[arg(long, default_value_t = 7100)]
    port: u16,

    /// Bind address
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
}

#[derive(clap::Args)]
pub struct ChatArgs {
    /// WebSocket URL of a running server
    #[arg(long, default_value = "ws://127.0.0.1:7100/ws")]
    pub connect: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| cli.log_level.clone().into()),
        )
        .init();

    match cli.command {
        Commands::Serve(args) => {
            run_server(RunOptions {
                host: Some(args.host),
                port: Some(args.port),
            })
            .await
        }
        Commands::Chat(args) => chat::run_chat(args).await,
    }
}
