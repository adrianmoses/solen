use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

// ---------------------------------------------------------------------------
// Archetype
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Archetype {
    #[default]
    Assistant,
    Engineer,
    Researcher,
    Operator,
    Mentor,
}

impl Archetype {
    pub fn prompt_fragment(&self) -> &'static str {
        match self {
            Self::Assistant => "You are a versatile personal assistant. You help with tasks efficiently, communicate clearly, and adapt to the user's needs.",
            Self::Engineer => "You are a software engineer. You think in systems, write clean code, debug methodically, and favor practical solutions over theoretical perfection.",
            Self::Researcher => "You are a researcher. You investigate topics deeply, cite sources when possible, weigh evidence carefully, and distinguish established facts from speculation.",
            Self::Operator => "You are an operations specialist. You focus on reliability, automation, monitoring, and keeping systems running smoothly. You think about failure modes and recovery.",
            Self::Mentor => "You are a mentor and teacher. You explain concepts clearly, build on what the learner already knows, ask guiding questions, and encourage independent thinking.",
        }
    }

    pub const ALL: &'static [Archetype] = &[
        Self::Assistant,
        Self::Engineer,
        Self::Researcher,
        Self::Operator,
        Self::Mentor,
    ];
}

impl fmt::Display for Archetype {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Assistant => write!(f, "assistant"),
            Self::Engineer => write!(f, "engineer"),
            Self::Researcher => write!(f, "researcher"),
            Self::Operator => write!(f, "operator"),
            Self::Mentor => write!(f, "mentor"),
        }
    }
}

impl FromStr for Archetype {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "assistant" => Ok(Self::Assistant),
            "engineer" => Ok(Self::Engineer),
            "researcher" => Ok(Self::Researcher),
            "operator" => Ok(Self::Operator),
            "mentor" => Ok(Self::Mentor),
            other => Err(format!(
                "unknown archetype: '{other}'. Expected: assistant, engineer, researcher, operator, mentor"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Tone
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    #[default]
    Neutral,
    Friendly,
    Direct,
    Formal,
}

impl Tone {
    pub fn prompt_fragment(&self) -> &'static str {
        match self {
            Self::Neutral => "Maintain a balanced, professional tone.",
            Self::Friendly => "Be warm, approachable, and conversational.",
            Self::Direct => {
                "Be concise and straightforward. Skip pleasantries and get to the point."
            }
            Self::Formal => "Use formal language and a professional register.",
        }
    }

    pub const ALL: &'static [Tone] = &[Self::Neutral, Self::Friendly, Self::Direct, Self::Formal];
}

impl fmt::Display for Tone {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Neutral => write!(f, "neutral"),
            Self::Friendly => write!(f, "friendly"),
            Self::Direct => write!(f, "direct"),
            Self::Formal => write!(f, "formal"),
        }
    }
}

impl FromStr for Tone {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "neutral" => Ok(Self::Neutral),
            "friendly" => Ok(Self::Friendly),
            "direct" => Ok(Self::Direct),
            "formal" => Ok(Self::Formal),
            other => Err(format!(
                "unknown tone: '{other}'. Expected: neutral, friendly, direct, formal"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Verbosity
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verbosity {
    Terse,
    #[default]
    Balanced,
    Thorough,
}

impl Verbosity {
    pub fn prompt_fragment(&self) -> &'static str {
        match self {
            Self::Terse => "Keep responses short and to the point. Omit filler.",
            Self::Balanced => "Provide enough detail to be helpful without being verbose.",
            Self::Thorough => "Give comprehensive, detailed responses. Explain your reasoning and cover edge cases.",
        }
    }

    pub const ALL: &'static [Verbosity] = &[Self::Terse, Self::Balanced, Self::Thorough];
}

impl fmt::Display for Verbosity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Terse => write!(f, "terse"),
            Self::Balanced => write!(f, "balanced"),
            Self::Thorough => write!(f, "thorough"),
        }
    }
}

impl FromStr for Verbosity {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "terse" => Ok(Self::Terse),
            "balanced" => Ok(Self::Balanced),
            "thorough" => Ok(Self::Thorough),
            other => Err(format!(
                "unknown verbosity: '{other}'. Expected: terse, balanced, thorough"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// DecisionStyle
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStyle {
    Cautious,
    #[default]
    Balanced,
    Autonomous,
}

impl DecisionStyle {
    pub fn prompt_fragment(&self) -> &'static str {
        match self {
            Self::Cautious => "Always ask for confirmation before taking significant actions. Err on the side of caution.",
            Self::Balanced => "Use your judgment for routine decisions but confirm before high-impact actions.",
            Self::Autonomous => "Act independently when possible. Make decisions and proceed without asking unless the consequences are irreversible.",
        }
    }

    pub const ALL: &'static [DecisionStyle] = &[Self::Cautious, Self::Balanced, Self::Autonomous];
}

impl fmt::Display for DecisionStyle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cautious => write!(f, "cautious"),
            Self::Balanced => write!(f, "balanced"),
            Self::Autonomous => write!(f, "autonomous"),
        }
    }
}

impl FromStr for DecisionStyle {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "cautious" => Ok(Self::Cautious),
            "balanced" => Ok(Self::Balanced),
            "autonomous" => Ok(Self::Autonomous),
            other => Err(format!(
                "unknown decision style: '{other}'. Expected: cautious, balanced, autonomous"
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Soul
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Soul {
    pub name: String,
    pub personality: String,
    pub archetype: Archetype,
    pub tone: Tone,
    pub verbosity: Verbosity,
    pub decision_style: DecisionStyle,
}

impl Default for Soul {
    fn default() -> Self {
        Self {
            name: "Assistant".to_string(),
            personality: String::new(),
            archetype: Archetype::default(),
            tone: Tone::default(),
            verbosity: Verbosity::default(),
            decision_style: DecisionStyle::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// System prompt composer
// ---------------------------------------------------------------------------

/// Compose a system prompt from a Soul configuration.
///
/// Sections are separated by double newlines:
/// 1. Identity line: "You are {name}."
/// 2. Archetype fragment
/// 3. Personality (free-text, if non-empty)
/// 4. Behavioral traits
pub fn compose_system_prompt(soul: &Soul) -> String {
    let identity = if soul.name.is_empty() {
        soul.archetype.prompt_fragment().to_string()
    } else {
        format!(
            "Your name is {}. {}",
            soul.name,
            soul.archetype.prompt_fragment()
        )
    };

    let mut prompt = identity;

    if !soul.personality.is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(&soul.personality);
    }

    // Behavioral traits
    let traits_line = format!(
        "{} {} {}",
        soul.tone.prompt_fragment(),
        soul.verbosity.prompt_fragment(),
        soul.decision_style.prompt_fragment(),
    );
    prompt.push_str("\n\n");
    prompt.push_str(&traits_line);

    prompt
}

// ---------------------------------------------------------------------------
// SOUL.md format parsing & serialization
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum SoulParseError {
    #[error("missing frontmatter delimiters (expected --- ... ---)")]
    MissingFrontmatter,
    #[error("invalid frontmatter YAML: {0}")]
    InvalidYaml(String),
}

/// YAML frontmatter helper (subset of fields, all optional for lenient parsing).
#[derive(Deserialize, Default)]
struct SoulFrontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    archetype: Option<String>,
    #[serde(default)]
    tone: Option<String>,
    #[serde(default)]
    verbosity: Option<String>,
    #[serde(default)]
    decision_style: Option<String>,
}

/// Parse a SOUL.md string into a `Soul`.
///
/// Format:
/// ```text
/// ---
/// name: Atlas
/// archetype: engineer
/// tone: direct
/// verbosity: balanced
/// decision_style: autonomous
/// ---
/// Free-text personality description here...
/// ```
pub fn parse_soul_md(input: &str) -> Result<Soul, SoulParseError> {
    let trimmed = input.trim_start();

    // Find the opening ---
    let rest = trimmed
        .strip_prefix("---")
        .ok_or(SoulParseError::MissingFrontmatter)?;

    // Find the closing ---
    let (yaml_str, body) = rest
        .split_once("\n---")
        .ok_or(SoulParseError::MissingFrontmatter)?;

    let fm: SoulFrontmatter =
        serde_yaml::from_str(yaml_str).map_err(|e| SoulParseError::InvalidYaml(e.to_string()))?;

    let personality = body
        .trim_start_matches(['\n', '\r', '-'])
        .trim()
        .to_string();

    let defaults = Soul::default();

    Ok(Soul {
        name: fm.name.unwrap_or(defaults.name),
        archetype: fm
            .archetype
            .and_then(|s| Archetype::from_str(&s).ok())
            .unwrap_or(defaults.archetype),
        tone: fm
            .tone
            .and_then(|s| Tone::from_str(&s).ok())
            .unwrap_or(defaults.tone),
        verbosity: fm
            .verbosity
            .and_then(|s| Verbosity::from_str(&s).ok())
            .unwrap_or(defaults.verbosity),
        decision_style: fm
            .decision_style
            .and_then(|s| DecisionStyle::from_str(&s).ok())
            .unwrap_or(defaults.decision_style),
        personality,
    })
}

/// Serialize a `Soul` into the SOUL.md format.
pub fn to_soul_md(soul: &Soul) -> String {
    let mut out = String::with_capacity(256);
    out.push_str("---\n");
    out.push_str(&format!("name: {}\n", soul.name));
    out.push_str(&format!("archetype: {}\n", soul.archetype));
    out.push_str(&format!("tone: {}\n", soul.tone));
    out.push_str(&format!("verbosity: {}\n", soul.verbosity));
    out.push_str(&format!("decision_style: {}\n", soul.decision_style));
    out.push_str("---\n");
    if !soul.personality.is_empty() {
        out.push('\n');
        out.push_str(&soul.personality);
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_soul() {
        let soul = Soul::default();
        assert_eq!(soul.name, "Assistant");
        assert_eq!(soul.archetype, Archetype::Assistant);
        assert_eq!(soul.tone, Tone::Neutral);
        assert_eq!(soul.verbosity, Verbosity::Balanced);
        assert_eq!(soul.decision_style, DecisionStyle::Balanced);
        assert!(soul.personality.is_empty());
    }

    #[test]
    fn archetype_prompt_fragments_non_empty() {
        for a in Archetype::ALL {
            assert!(!a.prompt_fragment().is_empty(), "{a} has empty fragment");
        }
    }

    #[test]
    fn tone_prompt_fragments_non_empty() {
        for t in Tone::ALL {
            assert!(!t.prompt_fragment().is_empty(), "{t} has empty fragment");
        }
    }

    #[test]
    fn verbosity_prompt_fragments_non_empty() {
        for v in Verbosity::ALL {
            assert!(!v.prompt_fragment().is_empty(), "{v} has empty fragment");
        }
    }

    #[test]
    fn decision_style_prompt_fragments_non_empty() {
        for d in DecisionStyle::ALL {
            assert!(!d.prompt_fragment().is_empty(), "{d} has empty fragment");
        }
    }

    #[test]
    fn serde_round_trip_archetype() {
        for a in Archetype::ALL {
            let json = serde_json::to_string(a).unwrap();
            let parsed: Archetype = serde_json::from_str(&json).unwrap();
            assert_eq!(*a, parsed);
        }
    }

    #[test]
    fn serde_round_trip_soul() {
        let soul = Soul {
            name: "Atlas".to_string(),
            personality: "A sharp engineer".to_string(),
            archetype: Archetype::Engineer,
            tone: Tone::Direct,
            verbosity: Verbosity::Terse,
            decision_style: DecisionStyle::Autonomous,
        };
        let json = serde_json::to_string(&soul).unwrap();
        let parsed: Soul = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "Atlas");
        assert_eq!(parsed.archetype, Archetype::Engineer);
    }

    #[test]
    fn compose_default_prompt() {
        let soul = Soul::default();
        let prompt = compose_system_prompt(&soul);
        assert!(prompt.contains("Assistant"));
        assert!(prompt.contains(Archetype::Assistant.prompt_fragment()));
        assert!(prompt.contains(Tone::Neutral.prompt_fragment()));
    }

    #[test]
    fn compose_with_personality() {
        let soul = Soul {
            name: "Kai".to_string(),
            personality: "You love Rust and functional programming.".to_string(),
            ..Soul::default()
        };
        let prompt = compose_system_prompt(&soul);
        assert!(prompt.contains("Kai"));
        assert!(prompt.contains("You love Rust"));
    }

    #[test]
    fn compose_empty_name() {
        let soul = Soul {
            name: String::new(),
            ..Soul::default()
        };
        let prompt = compose_system_prompt(&soul);
        // Should not contain "Your name is ."
        assert!(!prompt.contains("Your name is"));
        // Should still have archetype
        assert!(prompt.contains(Archetype::Assistant.prompt_fragment()));
    }

    #[test]
    fn soul_md_round_trip() {
        let soul = Soul {
            name: "Atlas".to_string(),
            personality: "A sharp, systems-minded engineer.".to_string(),
            archetype: Archetype::Engineer,
            tone: Tone::Direct,
            verbosity: Verbosity::Balanced,
            decision_style: DecisionStyle::Autonomous,
        };
        let md = to_soul_md(&soul);
        let parsed = parse_soul_md(&md).unwrap();
        assert_eq!(parsed.name, "Atlas");
        assert_eq!(parsed.archetype, Archetype::Engineer);
        assert_eq!(parsed.tone, Tone::Direct);
        assert_eq!(parsed.decision_style, DecisionStyle::Autonomous);
        assert_eq!(parsed.personality, "A sharp, systems-minded engineer.");
    }

    #[test]
    fn soul_md_missing_fields_use_defaults() {
        let md = "---\nname: Minimal\n---\n";
        let parsed = parse_soul_md(md).unwrap();
        assert_eq!(parsed.name, "Minimal");
        assert_eq!(parsed.archetype, Archetype::Assistant);
        assert_eq!(parsed.tone, Tone::Neutral);
    }

    #[test]
    fn soul_md_empty_body() {
        let md = "---\nname: NoBody\narchetype: mentor\n---\n";
        let parsed = parse_soul_md(md).unwrap();
        assert_eq!(parsed.name, "NoBody");
        assert_eq!(parsed.archetype, Archetype::Mentor);
        assert!(parsed.personality.is_empty());
    }

    #[test]
    fn soul_md_invalid_frontmatter() {
        let md = "no frontmatter here";
        assert!(parse_soul_md(md).is_err());
    }

    #[test]
    fn from_str_round_trip() {
        assert_eq!(
            Archetype::from_str("engineer").unwrap(),
            Archetype::Engineer
        );
        assert_eq!(Tone::from_str("direct").unwrap(), Tone::Direct);
        assert_eq!(Verbosity::from_str("terse").unwrap(), Verbosity::Terse);
        assert_eq!(
            DecisionStyle::from_str("autonomous").unwrap(),
            DecisionStyle::Autonomous
        );
    }

    #[test]
    fn from_str_invalid() {
        assert!(Archetype::from_str("hacker").is_err());
        assert!(Tone::from_str("angry").is_err());
        assert!(Verbosity::from_str("max").is_err());
        assert!(DecisionStyle::from_str("yolo").is_err());
    }
}
