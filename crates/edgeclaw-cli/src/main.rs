mod chat;
mod config;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

use edgeclaw_server::startup::{run_server, RunOptions};

use config::connector::{build_connector_from_args, ConnectorAddOpts};
use config::types::ApprovalMode;

#[derive(Parser)]
#[command(name = "edgeclaw", version, about = "EdgeClaw agent runtime CLI")]
struct Cli {
    /// Log level: error, warn, info, debug, trace
    #[arg(long, default_value = "info", global = true)]
    log_level: String,

    /// Path to config file
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the EdgeClaw WebSocket server
    Serve(ServeArgs),
    /// Open a chat session with the agent
    Chat(ChatArgs),
    /// Read and write agent configuration
    Config(ConfigArgs),
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

// ── Config subcommands ──────────────────────────────────────────────────────

#[derive(clap::Args)]
struct ConfigArgs {
    #[command(subcommand)]
    command: Option<ConfigCommands>,
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Pretty-print the full config
    Show(ShowArgs),
    /// Open the config file in $EDITOR
    Edit,
    /// Write values in a specific domain
    Set(SetArgs),
    /// Manage messaging connectors
    Connector(ConnectorArgs),
}

#[derive(clap::Args)]
struct ShowArgs {
    /// Print secrets in plaintext
    #[arg(long)]
    reveal_secrets: bool,
}

#[derive(clap::Args)]
struct SetArgs {
    #[command(subcommand)]
    domain: SetDomain,
}

#[derive(Subcommand)]
enum SetDomain {
    /// Configure model provider and parameters
    Model(SetModelArgs),
    /// Configure a named personality
    Personality(SetPersonalityArgs),
    /// Configure tool approval policy
    Approval(SetApprovalArgs),
    /// Enable or disable tools
    Tools(SetToolsArgs),
}

#[derive(clap::Args)]
struct SetModelArgs {
    /// Model provider (e.g. anthropic, openai, ollama)
    #[arg(long)]
    provider: Option<String>,
    /// Model identifier (e.g. claude-sonnet-4-20250514)
    #[arg(long)]
    model: Option<String>,
    /// API key for the provider
    #[arg(long)]
    api_key: Option<String>,
    /// Override the provider base URL
    #[arg(long)]
    base_url: Option<String>,
    /// Max tokens per response
    #[arg(long)]
    max_tokens: Option<u32>,
    /// Sampling temperature (0.0–2.0)
    #[arg(long)]
    temperature: Option<f32>,
}

#[derive(clap::Args)]
struct SetPersonalityArgs {
    /// Identifier for this personality
    #[arg(long)]
    name: String,
    /// Identity text describing the personality
    #[arg(long)]
    identity: Option<String>,
    /// Load identity from a file
    #[arg(long)]
    identity_file: Option<String>,
}

#[derive(clap::Args)]
struct SetApprovalArgs {
    /// Approval mode: always-ask, auto-approve, deny-all, allowlist
    #[arg(long)]
    mode: String,
}

#[derive(clap::Args)]
struct SetToolsArgs {
    /// Add a tool to the enabled set
    #[arg(long)]
    enable: Option<String>,
    /// Remove a tool from the enabled set
    #[arg(long)]
    disable: Option<String>,
    /// Print available tools and their enabled status
    #[arg(long)]
    list: bool,
}

// ── Connector subcommands ───────────────────────────────────────────────────

#[derive(clap::Args)]
struct ConnectorArgs {
    #[command(subcommand)]
    command: ConnectorCommands,
}

#[derive(Subcommand)]
enum ConnectorCommands {
    /// Register a new connector
    Add(ConnectorAddArgs),
    /// Print all configured connectors
    List,
    /// Deregister a connector by name
    Remove(ConnectorRemoveArgs),
    /// Verify connector credentials
    Test(ConnectorTestArgs),
}

#[derive(clap::Args)]
struct ConnectorAddArgs {
    /// Connector type: telegram, discord, slack
    #[arg(long = "type")]
    connector_type: String,
    /// Connector name
    #[arg(long)]
    name: String,
    /// Bot token (telegram, discord)
    #[arg(long)]
    token: Option<String>,
    /// Allowed chat IDs (telegram, comma-separated)
    #[arg(long, value_delimiter = ',')]
    allowed_chat_ids: Option<Vec<i64>>,
    /// Guild/server ID (discord)
    #[arg(long)]
    guild_id: Option<String>,
    /// Channel ID (discord)
    #[arg(long)]
    channel_id: Option<String>,
    /// App-level token (slack)
    #[arg(long)]
    app_token: Option<String>,
    /// Bot OAuth token (slack)
    #[arg(long)]
    bot_token: Option<String>,
    /// Channel name or ID (slack)
    #[arg(long)]
    channel: Option<String>,
}

#[derive(clap::Args)]
struct ConnectorRemoveArgs {
    /// Name of the connector to remove
    name: String,
}

#[derive(clap::Args)]
struct ConnectorTestArgs {
    /// Name of the connector to test
    name: String,
}

// ── Main ────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| cli.log_level.clone().into()),
        )
        .init();

    let config_path = config::resolve_config_path(cli.config.as_deref());

    match cli.command {
        Commands::Serve(args) => {
            // Load config and set env vars so ServerConfig::from_env() picks them up
            let cfg = config::load_config(&config_path)?;
            if let Some(key) = &cfg.model.api_key {
                std::env::set_var("ANTHROPIC_API_KEY", key);
            }
            if let Some(model) = &cfg.model.model {
                std::env::set_var("CLAUDE_MODEL", model);
            }
            if let Some(base_url) = &cfg.model.base_url {
                std::env::set_var("ANTHROPIC_BASE_URL", base_url);
            }

            let host = args.host;
            let port = args.port;

            run_server(RunOptions {
                host: Some(host),
                port: Some(port),
            })
            .await
        }
        Commands::Chat(args) => chat::run_chat(args).await,
        Commands::Config(config_args) => match config_args.command {
            None => config::wizard::run_wizard(&config_path),
            Some(ConfigCommands::Show(args)) => {
                config::show::run_show(&config_path, args.reveal_secrets)
            }
            Some(ConfigCommands::Edit) => config::edit::run_edit(&config_path),
            Some(ConfigCommands::Set(set_args)) => match set_args.domain {
                SetDomain::Model(args) => config::set::run_set_model(
                    &config_path,
                    config::set::SetModelOpts {
                        provider: args.provider,
                        model: args.model,
                        api_key: args.api_key,
                        base_url: args.base_url,
                        max_tokens: args.max_tokens,
                        temperature: args.temperature,
                    },
                ),
                SetDomain::Personality(args) => config::set::run_set_personality(
                    &config_path,
                    config::set::SetPersonalityOpts {
                        name: args.name,
                        identity: args.identity,
                        identity_file: args.identity_file,
                    },
                ),
                SetDomain::Approval(args) => {
                    let mode: ApprovalMode = args
                        .mode
                        .parse()
                        .map_err(|e: String| anyhow::anyhow!("{e}"))?;
                    config::set::run_set_approval(&config_path, mode)
                }
                SetDomain::Tools(args) => {
                    if args.list {
                        config::set::run_set_tools_list(&config_path)
                    } else if let Some(tool) = args.enable {
                        config::set::run_set_tools_enable(&config_path, tool)
                    } else if let Some(tool) = args.disable {
                        config::set::run_set_tools_disable(&config_path, tool)
                    } else {
                        anyhow::bail!("specify --enable <tool>, --disable <tool>, or --list");
                    }
                }
            },
            Some(ConfigCommands::Connector(conn_args)) => match conn_args.command {
                ConnectorCommands::Add(args) => {
                    let connector = build_connector_from_args(ConnectorAddOpts {
                        connector_type: args.connector_type,
                        name: args.name,
                        token: args.token,
                        allowed_chat_ids: args.allowed_chat_ids,
                        guild_id: args.guild_id,
                        channel_id: args.channel_id,
                        app_token: args.app_token,
                        bot_token: args.bot_token,
                        channel: args.channel,
                    })?;
                    config::connector::run_connector_add(&config_path, connector)
                }
                ConnectorCommands::List => config::connector::run_connector_list(&config_path),
                ConnectorCommands::Remove(args) => {
                    config::connector::run_connector_remove(&config_path, &args.name)
                }
                ConnectorCommands::Test(args) => {
                    config::connector::run_connector_test(&config_path, &args.name)
                }
            },
        },
    }
}
