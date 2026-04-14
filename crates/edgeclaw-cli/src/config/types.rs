use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

pub const DEFAULT_MODEL: &str = "claude-sonnet-4-20250514";
pub const DEFAULT_PROVIDER: &str = "anthropic";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EdgeclawConfig {
    #[serde(default)]
    pub model: ModelConfig,
    #[serde(default)]
    pub server: ServerSection,
    #[serde(default)]
    pub approval: ApprovalConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub personalities: Vec<PersonalityConfig>,
    #[serde(default)]
    pub connectors: Vec<ConnectorConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerSection {
    pub host: Option<String>,
    pub port: Option<u16>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApprovalConfig {
    #[serde(default)]
    pub mode: ApprovalMode,
    #[serde(default)]
    pub allowed_tools: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalMode {
    #[default]
    AlwaysAsk,
    AutoApprove,
    DenyAll,
    Allowlist,
}

impl fmt::Display for ApprovalMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlwaysAsk => write!(f, "always-ask"),
            Self::AutoApprove => write!(f, "auto-approve"),
            Self::DenyAll => write!(f, "deny-all"),
            Self::Allowlist => write!(f, "allowlist"),
        }
    }
}

impl FromStr for ApprovalMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "always-ask" => Ok(Self::AlwaysAsk),
            "auto-approve" => Ok(Self::AutoApprove),
            "deny-all" => Ok(Self::DenyAll),
            "allowlist" => Ok(Self::Allowlist),
            other => Err(format!(
                "unknown approval mode: '{other}'. Expected: always-ask, auto-approve, deny-all, allowlist"
            )),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolsConfig {
    #[serde(default)]
    pub enabled: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalityConfig {
    pub name: String,
    pub identity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ConnectorConfig {
    Telegram {
        name: String,
        token: String,
        #[serde(default)]
        allowed_chat_ids: Vec<i64>,
    },
    Discord {
        name: String,
        token: String,
        guild_id: String,
        channel_id: Option<String>,
    },
    Slack {
        name: String,
        app_token: String,
        bot_token: String,
        channel: Option<String>,
    },
}

impl ConnectorConfig {
    pub fn name(&self) -> &str {
        match self {
            Self::Telegram { name, .. } => name,
            Self::Discord { name, .. } => name,
            Self::Slack { name, .. } => name,
        }
    }

    pub fn connector_type(&self) -> &str {
        match self {
            Self::Telegram { .. } => "telegram",
            Self::Discord { .. } => "discord",
            Self::Slack { .. } => "slack",
        }
    }
}

/// Fields whose names indicate they contain secrets.
pub fn is_secret_field(name: &str) -> bool {
    let lower = name.to_lowercase();
    lower.contains("key") || lower.contains("token") || lower.contains("secret")
}

/// Redact a secret value, showing the first 8 characters followed by "****".
pub fn redact(value: &str) -> String {
    if value.len() <= 8 {
        "****".to_string()
    } else {
        format!("{}****", &value[..8])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_full_config() {
        let config = EdgeclawConfig {
            model: ModelConfig {
                provider: Some("anthropic".into()),
                model: Some("claude-sonnet-4-20250514".into()),
                api_key: Some("sk-ant-test".into()),
                base_url: None,
                max_tokens: Some(8096),
                temperature: Some(1.0),
            },
            server: ServerSection {
                host: Some("127.0.0.1".into()),
                port: Some(7100),
            },
            approval: ApprovalConfig {
                mode: ApprovalMode::AlwaysAsk,
                allowed_tools: vec!["read_file".into()],
            },
            tools: ToolsConfig {
                enabled: vec!["bash_tool".into(), "web_search".into()],
            },
            personalities: vec![
                PersonalityConfig {
                    name: "default".into(),
                    identity: "You are a helpful assistant.".into(),
                },
                PersonalityConfig {
                    name: "friday".into(),
                    identity: "You are Friday.".into(),
                },
            ],
            connectors: vec![
                ConnectorConfig::Telegram {
                    name: "my-telegram".into(),
                    token: "123456:ABC".into(),
                    allowed_chat_ids: vec![987654321],
                },
                ConnectorConfig::Discord {
                    name: "my-discord".into(),
                    token: "MTI3".into(),
                    guild_id: "1234567890".into(),
                    channel_id: None,
                },
                ConnectorConfig::Slack {
                    name: "my-slack".into(),
                    app_token: "xapp-test".into(),
                    bot_token: "xoxb-test".into(),
                    channel: Some("engineering".into()),
                },
            ],
        };

        let toml_str = toml::to_string_pretty(&config).expect("serialize");
        let parsed: EdgeclawConfig = toml::from_str(&toml_str).expect("deserialize");

        assert_eq!(parsed.model.provider.as_deref(), Some("anthropic"));
        assert_eq!(parsed.server.port, Some(7100));
        assert_eq!(parsed.approval.mode, ApprovalMode::AlwaysAsk);
        assert_eq!(parsed.tools.enabled.len(), 2);
        assert_eq!(parsed.personalities.len(), 2);
        assert_eq!(parsed.connectors.len(), 3);
        assert_eq!(parsed.connectors[0].name(), "my-telegram");
        assert_eq!(parsed.connectors[0].connector_type(), "telegram");
        assert_eq!(parsed.connectors[1].connector_type(), "discord");
        assert_eq!(parsed.connectors[2].connector_type(), "slack");
    }

    #[test]
    fn empty_toml_deserializes_to_defaults() {
        let parsed: EdgeclawConfig = toml::from_str("").expect("deserialize empty");
        assert!(parsed.model.provider.is_none());
        assert_eq!(parsed.approval.mode, ApprovalMode::AlwaysAsk);
        assert!(parsed.personalities.is_empty());
        assert!(parsed.connectors.is_empty());
    }

    #[test]
    fn redact_short_value() {
        assert_eq!(redact("short"), "****");
    }

    #[test]
    fn redact_long_value() {
        assert_eq!(redact("sk-ant-api1234567890"), "sk-ant-a****");
    }

    #[test]
    fn secret_field_detection() {
        assert!(is_secret_field("api_key"));
        assert!(is_secret_field("bot_token"));
        assert!(is_secret_field("client_secret"));
        assert!(!is_secret_field("provider"));
        assert!(!is_secret_field("name"));
    }
}
