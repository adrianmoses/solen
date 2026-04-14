pub mod connector;
pub mod edit;
pub mod set;
pub mod show;
pub mod types;
pub mod wizard;

pub use types::EdgeclawConfig;

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Resolve the config file path: CLI flag > EDGECLAW_CONFIG env > default.
pub fn resolve_config_path(cli_flag: Option<&Path>) -> PathBuf {
    if let Some(path) = cli_flag {
        return path.to_path_buf();
    }
    if let Ok(path) = std::env::var("EDGECLAW_CONFIG") {
        return PathBuf::from(path);
    }
    let xdg_config = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .expect("could not determine home directory")
                .join(".config")
        });
    xdg_config.join("edgeclaw").join("config.toml")
}

/// Load config from a TOML file. Returns defaults if the file does not exist.
pub fn load_config(path: &Path) -> Result<EdgeclawConfig> {
    if !path.is_file() {
        return Ok(EdgeclawConfig::default());
    }
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file: {}", path.display()))?;
    let config: EdgeclawConfig = toml::from_str(&contents)
        .with_context(|| format!("failed to parse config file: {}", path.display()))?;
    Ok(config)
}

/// Save config to a TOML file. Creates parent directories and writes atomically.
pub fn save_config(path: &Path, config: &EdgeclawConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory: {}", parent.display()))?;
    }
    let content = toml::to_string_pretty(config).context("failed to serialize config")?;
    let tmp_path = path.with_extension("toml.tmp");
    std::fs::write(&tmp_path, &content)
        .with_context(|| format!("failed to write temp config file: {}", tmp_path.display()))?;
    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("failed to rename temp config to: {}", path.display()))?;
    Ok(())
}

/// Apply environment variable overrides to a loaded config.
pub fn apply_env_overrides(config: &mut EdgeclawConfig) {
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        config.model.api_key = Some(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn load_missing_file_returns_defaults() {
        let path = Path::new("/tmp/edgeclaw-test-nonexistent.toml");
        let config = load_config(path).unwrap();
        assert!(config.model.provider.is_none());
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut config = EdgeclawConfig::default();
        config.model.provider = Some("anthropic".into());
        config.server.port = Some(7100);

        save_config(&path, &config).unwrap();
        let loaded = load_config(&path).unwrap();

        assert_eq!(loaded.model.provider.as_deref(), Some("anthropic"));
        assert_eq!(loaded.server.port, Some(7100));
    }

    #[test]
    fn save_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("deep").join("config.toml");

        let config = EdgeclawConfig::default();
        save_config(&path, &config).unwrap();
        assert!(path.is_file());
    }

    #[test]
    fn resolve_path_prefers_cli_flag() {
        let flag = PathBuf::from("/custom/config.toml");
        assert_eq!(resolve_config_path(Some(&flag)), flag);
    }

    #[test]
    fn load_invalid_toml_returns_error() {
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "this is not valid [[ toml {{").unwrap();
        let result = load_config(tmp.path());
        assert!(result.is_err());
    }
}
