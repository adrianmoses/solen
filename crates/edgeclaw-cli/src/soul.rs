use anyhow::{bail, Context, Result};

use agent_core::soul::{
    parse_soul_md, to_soul_md, Archetype, DecisionStyle, Soul, Tone, Verbosity,
};

/// Base URL for the server, derived from config or defaults.
fn server_base_url(host: &str, port: u16) -> String {
    format!("http://{}:{}", host, port)
}

// ── Show ────────────────────────────────────────────────────────────────────

pub async fn run_show(host: &str, port: u16, user_id: &str) -> Result<()> {
    let url = format!("{}/soul?user_id={}", server_base_url(host, port), user_id);
    let resp = reqwest::get(&url)
        .await
        .context("failed to connect to server — is it running?")?;

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("server error: {text}");
    }

    let soul: serde_json::Value = resp.json().await?;
    println!("Name:           {}", soul["name"].as_str().unwrap_or("-"));
    println!(
        "Archetype:      {}",
        soul["archetype"].as_str().unwrap_or("-")
    );
    println!("Tone:           {}", soul["tone"].as_str().unwrap_or("-"));
    println!(
        "Verbosity:      {}",
        soul["verbosity"].as_str().unwrap_or("-")
    );
    println!(
        "Decision style: {}",
        soul["decision_style"].as_str().unwrap_or("-")
    );
    let personality = soul["personality"].as_str().unwrap_or("");
    if !personality.is_empty() {
        println!("\nPersonality:\n{personality}");
    }

    Ok(())
}

// ── Set ─────────────────────────────────────────────────────────────────────

pub struct SoulSetOpts {
    pub name: Option<String>,
    pub personality: Option<String>,
    pub archetype: Option<String>,
    pub tone: Option<String>,
    pub verbosity: Option<String>,
    pub decision_style: Option<String>,
}

pub async fn run_set(host: &str, port: u16, user_id: &str, opts: SoulSetOpts) -> Result<()> {
    // Validate enum values locally before sending
    if let Some(ref a) = opts.archetype {
        a.parse::<Archetype>().map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    if let Some(ref t) = opts.tone {
        t.parse::<Tone>().map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    if let Some(ref v) = opts.verbosity {
        v.parse::<Verbosity>().map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    if let Some(ref d) = opts.decision_style {
        d.parse::<DecisionStyle>()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }

    let url = format!("{}/soul", server_base_url(host, port));
    let mut body = serde_json::json!({ "user_id": user_id });
    if let Some(v) = opts.name {
        body["name"] = serde_json::Value::String(v);
    }
    if let Some(v) = opts.personality {
        body["personality"] = serde_json::Value::String(v);
    }
    if let Some(v) = opts.archetype {
        body["archetype"] = serde_json::Value::String(v);
    }
    if let Some(v) = opts.tone {
        body["tone"] = serde_json::Value::String(v);
    }
    if let Some(v) = opts.verbosity {
        body["verbosity"] = serde_json::Value::String(v);
    }
    if let Some(v) = opts.decision_style {
        body["decision_style"] = serde_json::Value::String(v);
    }

    let client = reqwest::Client::new();
    let resp = client
        .patch(&url)
        .json(&body)
        .send()
        .await
        .context("failed to connect to server — is it running?")?;

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("server error: {text}");
    }

    let soul: serde_json::Value = resp.json().await?;
    println!("Updated soul: {}", soul["name"].as_str().unwrap_or("-"));

    Ok(())
}

// ── Edit ────────────────────────────────────────────────────────────────────

pub async fn run_edit(host: &str, port: u16, user_id: &str) -> Result<()> {
    let base = server_base_url(host, port);

    // Fetch current soul
    let resp = reqwest::get(&format!("{base}/soul?user_id={user_id}"))
        .await
        .context("failed to connect to server")?;
    let soul_json: serde_json::Value = resp.json().await?;

    let soul = Soul {
        name: soul_json["name"].as_str().unwrap_or("").to_string(),
        personality: soul_json["personality"].as_str().unwrap_or("").to_string(),
        archetype: soul_json["archetype"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or_default(),
        tone: soul_json["tone"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or_default(),
        verbosity: soul_json["verbosity"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or_default(),
        decision_style: soul_json["decision_style"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or_default(),
    };

    let md = to_soul_md(&soul);

    // Write to temp file
    let tmp_dir = std::env::temp_dir();
    let tmp_path = tmp_dir.join("edgeclaw-soul.md");
    std::fs::write(&tmp_path, &md)?;

    // Open in editor
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor)
        .arg(&tmp_path)
        .status()
        .context("failed to launch editor")?;

    if !status.success() {
        bail!("editor exited with non-zero status");
    }

    // Read back and parse
    let edited = std::fs::read_to_string(&tmp_path)?;
    let _ = std::fs::remove_file(&tmp_path);

    let new_soul = parse_soul_md(&edited)?;

    // POST to server
    let body = serde_json::json!({
        "user_id": user_id,
        "name": new_soul.name,
        "personality": new_soul.personality,
        "archetype": new_soul.archetype.to_string(),
        "tone": new_soul.tone.to_string(),
        "verbosity": new_soul.verbosity.to_string(),
        "decision_style": new_soul.decision_style.to_string(),
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/soul"))
        .json(&body)
        .send()
        .await?;

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("server error: {text}");
    }

    println!("Soul updated: {}", new_soul.name);
    Ok(())
}

// ── Generate ────────────────────────────────────────────────────────────────

pub async fn run_generate(host: &str, port: u16, user_id: &str, description: &str) -> Result<()> {
    let url = format!("{}/soul/generate", server_base_url(host, port));
    let body = serde_json::json!({
        "user_id": user_id,
        "description": description,
    });

    println!("Generating soul from: \"{description}\"...");

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("failed to connect to server — is it running?")?;

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("server error: {text}");
    }

    let soul: serde_json::Value = resp.json().await?;
    println!("\nGenerated soul:");
    println!("  Name:           {}", soul["name"].as_str().unwrap_or("-"));
    println!(
        "  Archetype:      {}",
        soul["archetype"].as_str().unwrap_or("-")
    );
    println!("  Tone:           {}", soul["tone"].as_str().unwrap_or("-"));
    println!(
        "  Verbosity:      {}",
        soul["verbosity"].as_str().unwrap_or("-")
    );
    println!(
        "  Decision style: {}",
        soul["decision_style"].as_str().unwrap_or("-")
    );
    let personality = soul["personality"].as_str().unwrap_or("");
    if !personality.is_empty() {
        println!("\n  Personality:\n  {personality}");
    }

    Ok(())
}

// ── Import ──────────────────────────────────────────────────────────────────

pub async fn run_import(host: &str, port: u16, user_id: &str, file_path: &str) -> Result<()> {
    let content =
        std::fs::read_to_string(file_path).context(format!("failed to read '{file_path}'"))?;
    let soul = parse_soul_md(&content)?;

    let url = format!("{}/soul", server_base_url(host, port));
    let body = serde_json::json!({
        "user_id": user_id,
        "name": soul.name,
        "personality": soul.personality,
        "archetype": soul.archetype.to_string(),
        "tone": soul.tone.to_string(),
        "verbosity": soul.verbosity.to_string(),
        "decision_style": soul.decision_style.to_string(),
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("failed to connect to server — is it running?")?;

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        bail!("server error: {text}");
    }

    println!("Imported soul '{}' from {file_path}", soul.name);
    Ok(())
}

// ── Export ───────────────────────────────────────────────────────────────────

pub async fn run_export(host: &str, port: u16, user_id: &str) -> Result<()> {
    let url = format!("{}/soul?user_id={}", server_base_url(host, port), user_id);
    let resp = reqwest::get(&url)
        .await
        .context("failed to connect to server — is it running?")?;

    let soul_json: serde_json::Value = resp.json().await?;
    let soul = Soul {
        name: soul_json["name"].as_str().unwrap_or("").to_string(),
        personality: soul_json["personality"].as_str().unwrap_or("").to_string(),
        archetype: soul_json["archetype"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or_default(),
        tone: soul_json["tone"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or_default(),
        verbosity: soul_json["verbosity"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or_default(),
        decision_style: soul_json["decision_style"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or_default(),
    };

    print!("{}", to_soul_md(&soul));
    Ok(())
}
