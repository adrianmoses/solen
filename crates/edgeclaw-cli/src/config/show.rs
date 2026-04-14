use anyhow::Result;
use std::path::Path;

use super::types::{is_secret_field, redact, EdgeclawConfig};
use super::{apply_env_overrides, load_config};

pub fn run_show(config_path: &Path, reveal_secrets: bool) -> Result<()> {
    let mut config = load_config(config_path)?;
    apply_env_overrides(&mut config);
    print_config(&config, reveal_secrets);
    Ok(())
}

fn print_config(config: &EdgeclawConfig, reveal_secrets: bool) {
    println!("\x1b[1m[model]\x1b[0m");
    print_opt("provider", config.model.provider.as_deref(), reveal_secrets);
    print_opt("model", config.model.model.as_deref(), reveal_secrets);
    print_opt("api_key", config.model.api_key.as_deref(), reveal_secrets);
    print_opt("base_url", config.model.base_url.as_deref(), reveal_secrets);
    if let Some(v) = config.model.max_tokens {
        println!("  {:<20} {}", "max_tokens", v);
    }
    if let Some(v) = config.model.temperature {
        println!("  {:<20} {}", "temperature", v);
    }
    println!();

    println!("\x1b[1m[server]\x1b[0m");
    print_opt("host", config.server.host.as_deref(), reveal_secrets);
    if let Some(v) = config.server.port {
        println!("  {:<20} {}", "port", v);
    }
    println!();

    println!("\x1b[1m[approval]\x1b[0m");
    println!("  {:<20} {}", "mode", config.approval.mode);
    if !config.approval.allowed_tools.is_empty() {
        println!(
            "  {:<20} {}",
            "allowed_tools",
            config.approval.allowed_tools.join(", ")
        );
    }
    println!();

    println!("\x1b[1m[tools]\x1b[0m");
    if config.tools.enabled.is_empty() {
        println!("  (none configured)");
    } else {
        println!("  {:<20} {}", "enabled", config.tools.enabled.join(", "));
    }
    println!();

    println!("\x1b[1m[[personalities]]\x1b[0m");
    if config.personalities.is_empty() {
        println!("  (none configured)");
    } else {
        for p in &config.personalities {
            println!("  {:<20} {}", "name", p.name);
            let identity_preview: String = if p.identity.chars().count() > 60 {
                let truncated: String = p.identity.chars().take(60).collect();
                format!("{truncated}...")
            } else {
                p.identity.clone()
            };
            println!("  {:<20} {}", "identity", identity_preview);
            println!();
        }
    }

    println!("\x1b[1m[[connectors]]\x1b[0m");
    if config.connectors.is_empty() {
        println!("  (none configured)");
    } else {
        for c in &config.connectors {
            println!("  {:<20} {}", "name", c.name());
            println!("  {:<20} {}", "type", c.connector_type());
            println!();
        }
    }
}

fn print_opt(label: &str, value: Option<&str>, reveal_secrets: bool) {
    if let Some(v) = value {
        let display = if !reveal_secrets && is_secret_field(label) {
            redact(v)
        } else {
            v.to_string()
        };
        println!("  {:<20} {}", label, display);
    }
}
