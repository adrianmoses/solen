use anyhow::Result;
use std::path::Path;

use super::types::{
    ApprovalMode, ConnectorConfig, EdgeclawConfig, ModelConfig, PersonalityConfig, DEFAULT_MODEL,
    DEFAULT_PROVIDER,
};
use super::{load_config, save_config};

/// Entry point: run first-run wizard or edit menu depending on config state.
pub fn run_wizard(config_path: &Path) -> Result<()> {
    if config_path.is_file() {
        run_edit_menu(config_path)
    } else {
        run_first_run(config_path)
    }
}

fn run_first_run(config_path: &Path) -> Result<()> {
    println!("Welcome to EdgeClaw! Let's set up your configuration.\n");

    let provider = inquire::Text::new("Model provider:")
        .with_default(DEFAULT_PROVIDER)
        .prompt()?;

    let model = inquire::Text::new("Model ID:")
        .with_default(DEFAULT_MODEL)
        .prompt()?;

    let api_key_input = inquire::Password::new("API key (Enter to skip, use env var instead):")
        .without_confirmation()
        .prompt()?;
    let api_key = if api_key_input.is_empty() {
        None
    } else {
        Some(api_key_input)
    };

    let personality_name = inquire::Text::new("Default personality name:")
        .with_default("default")
        .prompt()?;

    let identity = inquire::Text::new("Identity:")
        .with_default("You are a helpful assistant.")
        .prompt()?;

    let mut config = EdgeclawConfig {
        model: ModelConfig {
            provider: Some(provider),
            model: Some(model),
            api_key,
            ..Default::default()
        },
        personalities: vec![PersonalityConfig {
            name: personality_name,
            identity,
        }],
        ..Default::default()
    };

    let add_connector = inquire::Confirm::new("Add a connector now?")
        .with_default(false)
        .prompt()?;

    if add_connector {
        if let Ok(connector) = prompt_connector() {
            config.connectors.push(connector);
        }
    }

    save_config(config_path, &config)?;
    println!("\nConfig saved to {}", config_path.display());
    Ok(())
}

fn run_edit_menu(config_path: &Path) -> Result<()> {
    loop {
        let choices = vec![
            "Model settings",
            "Personalities",
            "Approval policy",
            "Tool settings",
            "Connectors",
            "Done",
        ];

        let selection =
            inquire::Select::new("What would you like to configure?", choices).prompt()?;

        match selection {
            "Model settings" => edit_model(config_path)?,
            "Personalities" => edit_personality(config_path)?,
            "Approval policy" => edit_approval(config_path)?,
            "Tool settings" => edit_tools(config_path)?,
            "Connectors" => edit_connectors(config_path)?,
            "Done" => break,
            _ => unreachable!(),
        }
        println!();
    }
    Ok(())
}

fn edit_model(config_path: &Path) -> Result<()> {
    let mut config = load_config(config_path)?;

    let provider = inquire::Text::new("Provider:")
        .with_default(config.model.provider.as_deref().unwrap_or(DEFAULT_PROVIDER))
        .prompt()?;
    config.model.provider = Some(provider);

    let model = inquire::Text::new("Model ID:")
        .with_default(config.model.model.as_deref().unwrap_or(DEFAULT_MODEL))
        .prompt()?;
    config.model.model = Some(model);

    let change_key = inquire::Confirm::new("Update API key?")
        .with_default(false)
        .prompt()?;
    if change_key {
        let key = inquire::Password::new("API key:")
            .without_confirmation()
            .prompt()?;
        if !key.is_empty() {
            config.model.api_key = Some(key);
        }
    }

    save_config(config_path, &config)?;
    println!("Updated [model] section.");
    Ok(())
}

fn edit_personality(config_path: &Path) -> Result<()> {
    let mut config = load_config(config_path)?;

    let choices = vec!["Add new personality", "Edit existing", "Back"];
    let selection = inquire::Select::new("Personality:", choices).prompt()?;

    match selection {
        "Add new personality" => {
            let name = inquire::Text::new("Name:").prompt()?;
            let identity = inquire::Text::new("Identity:").prompt()?;
            config
                .personalities
                .push(PersonalityConfig { name, identity });
        }
        "Edit existing" => {
            if config.personalities.is_empty() {
                println!("No personalities configured.");
                return Ok(());
            }
            let names: Vec<String> = config
                .personalities
                .iter()
                .map(|p| p.name.clone())
                .collect();
            let selected = inquire::Select::new("Select personality:", names).prompt()?;
            if let Some(p) = config.personalities.iter_mut().find(|p| p.name == selected) {
                let new_identity = inquire::Text::new("Identity:")
                    .with_default(&p.identity)
                    .prompt()?;
                p.identity = new_identity;
            }
        }
        _ => return Ok(()),
    }

    save_config(config_path, &config)?;
    println!("Updated [[personalities]].");
    Ok(())
}

fn edit_approval(config_path: &Path) -> Result<()> {
    let mut config = load_config(config_path)?;

    let modes = vec!["always-ask", "auto-approve", "deny-all", "allowlist"];
    let current = config.approval.mode.to_string();
    let start = modes.iter().position(|m| *m == current).unwrap_or(0);

    let selected = inquire::Select::new("Approval mode:", modes)
        .with_starting_cursor(start)
        .prompt()?;

    config.approval.mode = selected.parse::<ApprovalMode>().unwrap_or_default();

    save_config(config_path, &config)?;
    println!("Updated [approval] section.");
    Ok(())
}

fn edit_tools(config_path: &Path) -> Result<()> {
    let mut config = load_config(config_path)?;

    println!(
        "Currently enabled: {}",
        if config.tools.enabled.is_empty() {
            "(none)".to_string()
        } else {
            config.tools.enabled.join(", ")
        }
    );

    let choices = vec!["Enable a tool", "Disable a tool", "Back"];
    let selection = inquire::Select::new("Tool settings:", choices).prompt()?;

    match selection {
        "Enable a tool" => {
            let tool = inquire::Text::new("Tool name to enable:").prompt()?;
            if !config.tools.enabled.contains(&tool) {
                config.tools.enabled.push(tool);
            }
        }
        "Disable a tool" => {
            if config.tools.enabled.is_empty() {
                println!("No tools to disable.");
                return Ok(());
            }
            let tools: Vec<String> = config.tools.enabled.clone();
            let selected = inquire::Select::new("Tool to disable:", tools).prompt()?;
            config.tools.enabled.retain(|t| t != &selected);
        }
        _ => return Ok(()),
    }

    save_config(config_path, &config)?;
    println!("Updated [tools] section.");
    Ok(())
}

fn edit_connectors(config_path: &Path) -> Result<()> {
    let mut config = load_config(config_path)?;

    let choices = vec!["Add connector", "Remove connector", "Back"];
    let selection = inquire::Select::new("Connectors:", choices).prompt()?;

    match selection {
        "Add connector" => {
            let connector = prompt_connector()?;
            config.connectors.push(connector);
            save_config(config_path, &config)?;
            println!("Added connector.");
        }
        "Remove connector" => {
            if config.connectors.is_empty() {
                println!("No connectors to remove.");
                return Ok(());
            }
            let names: Vec<String> = config
                .connectors
                .iter()
                .map(|c| c.name().to_string())
                .collect();
            let selected = inquire::Select::new("Connector to remove:", names).prompt()?;
            config.connectors.retain(|c| c.name() != selected);
            save_config(config_path, &config)?;
            println!("Removed connector.");
        }
        _ => {}
    }

    Ok(())
}

fn prompt_connector() -> Result<ConnectorConfig> {
    let types = vec!["telegram", "discord", "slack"];
    let connector_type = inquire::Select::new("Connector type:", types).prompt()?;

    let name = inquire::Text::new("Connector name:").prompt()?;

    match connector_type {
        "telegram" => {
            let token = inquire::Password::new("Bot token:")
                .without_confirmation()
                .prompt()?;
            Ok(ConnectorConfig::Telegram {
                name,
                token,
                allowed_chat_ids: vec![],
            })
        }
        "discord" => {
            let token = inquire::Password::new("Bot token:")
                .without_confirmation()
                .prompt()?;
            let guild_id = inquire::Text::new("Guild ID:").prompt()?;
            let channel_id = inquire::Text::new("Channel ID (Enter to skip):")
                .prompt_skippable()?
                .filter(|s| !s.is_empty());
            Ok(ConnectorConfig::Discord {
                name,
                token,
                guild_id,
                channel_id,
            })
        }
        "slack" => {
            let app_token = inquire::Password::new("App token:")
                .without_confirmation()
                .prompt()?;
            let bot_token = inquire::Password::new("Bot token:")
                .without_confirmation()
                .prompt()?;
            let channel = inquire::Text::new("Channel (Enter to skip):")
                .prompt_skippable()?
                .filter(|s| !s.is_empty());
            Ok(ConnectorConfig::Slack {
                name,
                app_token,
                bot_token,
                channel,
            })
        }
        _ => unreachable!(),
    }
}
