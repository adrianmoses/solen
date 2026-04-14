use anyhow::{bail, Result};
use std::io::IsTerminal;
use std::path::Path;

use super::types::ConnectorConfig;
use super::{load_config, save_config};

pub fn run_connector_add(config_path: &Path, connector: ConnectorConfig) -> Result<()> {
    let mut config = load_config(config_path)?;

    let new_name = connector.name().to_string();
    if config.connectors.iter().any(|c| c.name() == new_name) {
        bail!("connector '{}' already exists", new_name);
    }

    config.connectors.push(connector);
    save_config(config_path, &config)?;
    println!("Added connector '{new_name}'.");
    Ok(())
}

pub fn run_connector_list(config_path: &Path) -> Result<()> {
    let config = load_config(config_path)?;

    if config.connectors.is_empty() {
        println!("No connectors configured.");
        return Ok(());
    }

    println!("{:<20} {:<12}", "NAME", "TYPE");
    println!("{:<20} {:<12}", "────", "────");
    for c in &config.connectors {
        println!("{:<20} {:<12}", c.name(), c.connector_type());
    }
    Ok(())
}

pub fn run_connector_remove(config_path: &Path, name: &str) -> Result<()> {
    let mut config = load_config(config_path)?;
    let before = config.connectors.len();
    config.connectors.retain(|c| c.name() != name);
    if config.connectors.len() == before {
        bail!("connector '{}' not found", name);
    }
    save_config(config_path, &config)?;
    println!("Removed connector '{name}'.");
    Ok(())
}

pub fn run_connector_test(config_path: &Path, name: &str) -> Result<()> {
    let config = load_config(config_path)?;
    if config.connectors.iter().any(|c| c.name() == name) {
        println!("Connector '{name}' configuration looks valid.");
    } else {
        bail!("connector '{}' not found", name);
    }
    Ok(())
}

pub struct ConnectorAddOpts {
    pub connector_type: String,
    pub name: String,
    pub token: Option<String>,
    pub allowed_chat_ids: Option<Vec<i64>>,
    pub guild_id: Option<String>,
    pub channel_id: Option<String>,
    pub app_token: Option<String>,
    pub bot_token: Option<String>,
    pub channel: Option<String>,
}

/// Build a ConnectorConfig from CLI args, prompting interactively for missing fields.
pub fn build_connector_from_args(opts: ConnectorAddOpts) -> Result<ConnectorConfig> {
    let ConnectorAddOpts {
        connector_type,
        name,
        token,
        allowed_chat_ids,
        guild_id,
        channel_id,
        app_token,
        bot_token,
        channel,
    } = opts;
    let is_tty = std::io::stdin().is_terminal();

    match connector_type.as_str() {
        "telegram" => {
            let token = require_or_prompt(token, "Bot token", is_tty)?;
            let allowed_chat_ids = allowed_chat_ids.unwrap_or_default();
            Ok(ConnectorConfig::Telegram {
                name,
                token,
                allowed_chat_ids,
            })
        }
        "discord" => {
            let token = require_or_prompt(token, "Bot token", is_tty)?;
            let guild_id = require_or_prompt(guild_id, "Guild ID", is_tty)?;
            Ok(ConnectorConfig::Discord {
                name,
                token,
                guild_id,
                channel_id,
            })
        }
        "slack" => {
            let app_token = require_or_prompt(app_token, "App token", is_tty)?;
            let bot_token = require_or_prompt(bot_token, "Bot token", is_tty)?;
            Ok(ConnectorConfig::Slack {
                name,
                app_token,
                bot_token,
                channel,
            })
        }
        other => bail!("unknown connector type: '{other}'. Expected: telegram, discord, slack"),
    }
}

fn require_or_prompt(value: Option<String>, label: &str, is_tty: bool) -> Result<String> {
    if let Some(v) = value {
        return Ok(v);
    }
    if !is_tty {
        bail!(
            "--{} is required in non-interactive mode",
            label.to_lowercase().replace(' ', "-")
        );
    }
    let answer = inquire::Text::new(&format!("{label}:")).prompt()?;
    Ok(answer)
}
