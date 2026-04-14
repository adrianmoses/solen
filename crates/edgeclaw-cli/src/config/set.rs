use anyhow::{bail, Result};
use std::path::Path;

use super::types::{ApprovalMode, PersonalityConfig};
use super::{load_config, save_config};

// ── config set model ────────────────────────────────────────────────────────

pub struct SetModelOpts {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

pub fn run_set_model(config_path: &Path, opts: SetModelOpts) -> Result<()> {
    let mut config = load_config(config_path)?;

    if let Some(v) = opts.provider {
        config.model.provider = Some(v);
    }
    if let Some(v) = opts.model {
        config.model.model = Some(v);
    }
    if let Some(v) = opts.api_key {
        config.model.api_key = Some(v);
    }
    if let Some(v) = opts.base_url {
        config.model.base_url = Some(v);
    }
    if let Some(v) = opts.max_tokens {
        config.model.max_tokens = Some(v);
    }
    if let Some(v) = opts.temperature {
        config.model.temperature = Some(v);
    }

    save_config(config_path, &config)?;
    println!("Updated [model] section.");
    Ok(())
}

// ── config set personality ──────────────────────────────────────────────────

pub struct SetPersonalityOpts {
    pub name: String,
    pub identity: Option<String>,
    pub identity_file: Option<String>,
}

pub fn run_set_personality(config_path: &Path, opts: SetPersonalityOpts) -> Result<()> {
    let mut config = load_config(config_path)?;

    let identity = if let Some(path) = &opts.identity_file {
        std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read identity file '{}': {}", path, e))?
    } else if let Some(text) = opts.identity {
        text
    } else {
        bail!("either --identity or --identity-file is required");
    };

    if let Some(existing) = config
        .personalities
        .iter_mut()
        .find(|p| p.name == opts.name)
    {
        existing.identity = identity;
        println!("Updated personality '{}'.", opts.name);
    } else {
        config.personalities.push(PersonalityConfig {
            name: opts.name.clone(),
            identity,
        });
        println!("Added personality '{}'.", opts.name);
    }

    save_config(config_path, &config)?;
    Ok(())
}

// ── config set approval ─────────────────────────────────────────────────────

pub fn run_set_approval(config_path: &Path, mode: ApprovalMode) -> Result<()> {
    let mut config = load_config(config_path)?;
    config.approval.mode = mode;
    save_config(config_path, &config)?;
    println!("Updated [approval] section.");
    Ok(())
}

// ── config set tools ────────────────────────────────────────────────────────

pub fn run_set_tools_enable(config_path: &Path, tool: String) -> Result<()> {
    let mut config = load_config(config_path)?;
    if !config.tools.enabled.contains(&tool) {
        config.tools.enabled.push(tool.clone());
    }
    save_config(config_path, &config)?;
    println!("Enabled tool '{tool}'.");
    Ok(())
}

pub fn run_set_tools_disable(config_path: &Path, tool: String) -> Result<()> {
    let mut config = load_config(config_path)?;
    config.tools.enabled.retain(|t| t != &tool);
    save_config(config_path, &config)?;
    println!("Disabled tool '{tool}'.");
    Ok(())
}

pub fn run_set_tools_list(config_path: &Path) -> Result<()> {
    let config = load_config(config_path)?;
    if config.tools.enabled.is_empty() {
        println!("No tools explicitly enabled.");
    } else {
        println!("Enabled tools:");
        for tool in &config.tools.enabled {
            println!("  - {tool}");
        }
    }
    Ok(())
}
