use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

use super::save_config;
use super::types::EdgeclawConfig;

pub fn run_edit(config_path: &Path) -> Result<()> {
    // Ensure the config file exists (create default if not)
    if !config_path.is_file() {
        save_config(config_path, &EdgeclawConfig::default())?;
    }

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());

    loop {
        let status = Command::new(&editor)
            .arg(config_path)
            .status()
            .with_context(|| format!("failed to launch editor: {editor}"))?;

        if !status.success() {
            bail!("editor exited with status: {status}");
        }

        // Validate the edited file
        let contents = std::fs::read_to_string(config_path)
            .context("failed to read config file after editing")?;

        match toml::from_str::<EdgeclawConfig>(&contents) {
            Ok(_) => {
                println!("Config is valid.");
                return Ok(());
            }
            Err(e) => {
                eprintln!("\x1b[31mConfig parse error:\x1b[0m {e}");
                let reopen = inquire::Confirm::new("Re-open editor to fix?")
                    .with_default(true)
                    .prompt()?;
                if !reopen {
                    println!("Changes kept as-is (may contain errors).");
                    return Ok(());
                }
            }
        }
    }
}
