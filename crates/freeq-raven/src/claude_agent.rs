use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone)]
pub struct ClaudeAgentConfig {
    pub command: String,
    pub workdir: Option<PathBuf>,
    pub alexandria_plugin_path: Option<PathBuf>,
    pub model: Option<String>,
    pub permission_mode: String,
    pub max_turns: u32,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct ClaudeAgentTurn {
    pub channel: String,
    pub asker: String,
    pub source: String,
    pub question: String,
    pub session_context: String,
    pub system_prompt: String,
}

#[derive(Debug, Clone)]
pub struct ClaudeAgentAnswer {
    pub text: String,
    pub session_id: Option<String>,
    pub plugins: Vec<ClaudeAgentPlugin>,
    pub skills: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaudeAgentPlugin {
    pub name: String,
    pub path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SidecarRequest<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    request_type: &'a str,
    channel: &'a str,
    asker: &'a str,
    source: &'a str,
    question: &'a str,
    session_context: &'a str,
    system_prompt: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    alexandria_plugin_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<&'a str>,
    permission_mode: &'a str,
    max_turns: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SidecarResponse {
    ok: bool,
    #[serde(default)]
    text: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    plugins: Vec<ClaudeAgentPlugin>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    error: Option<String>,
}

pub type ClaudeSessionMap = std::sync::Arc<tokio::sync::Mutex<HashMap<String, String>>>;

pub async fn ask(
    cfg: &ClaudeAgentConfig,
    sessions: &ClaudeSessionMap,
    turn: ClaudeAgentTurn,
) -> Result<ClaudeAgentAnswer> {
    let remembered_session = {
        let guard = sessions.lock().await;
        guard.get(&turn.channel).cloned()
    };
    let session_id = remembered_session.as_deref();
    let req = SidecarRequest {
        id: "freeq-raven-turn",
        request_type: "turn",
        channel: &turn.channel,
        asker: &turn.asker,
        source: &turn.source,
        question: &turn.question,
        session_context: &turn.session_context,
        system_prompt: &turn.system_prompt,
        session_id,
        cwd: cfg.workdir.as_ref().map(|p| p.display().to_string()),
        alexandria_plugin_path: cfg
            .alexandria_plugin_path
            .as_ref()
            .map(|p| p.display().to_string()),
        model: cfg.model.as_deref(),
        permission_mode: &cfg.permission_mode,
        max_turns: cfg.max_turns,
    };
    let payload = serde_json::to_vec(&req).context("encoding claude agent request")?;

    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.arg("-lc")
        .arg(&cfg.command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(workdir) = &cfg.workdir {
        cmd.current_dir(workdir);
    }

    let mut child = cmd.spawn().context("starting claude agent sidecar")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&payload)
            .await
            .context("writing claude agent request")?;
        stdin
            .shutdown()
            .await
            .context("closing claude agent stdin")?;
    }

    let output = tokio::time::timeout(cfg.timeout, child.wait_with_output())
        .await
        .context("claude agent sidecar timed out")?
        .context("waiting for claude agent sidecar")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        anyhow::bail!(
            "claude agent sidecar exited {}: {}",
            output.status,
            stderr.trim()
        );
    }
    let line = stdout
        .lines()
        .find(|line| !line.trim().is_empty())
        .context("claude agent sidecar returned no JSON")?;
    let parsed: SidecarResponse =
        serde_json::from_str(line).context("decoding claude agent sidecar response")?;
    if !parsed.ok {
        anyhow::bail!(
            "claude agent sidecar failed: {}",
            parsed.error.unwrap_or_else(|| "unknown error".to_string())
        );
    }
    if parsed.text.trim().is_empty() {
        anyhow::bail!("claude agent sidecar returned empty text");
    }
    if let Some(session_id) = parsed.session_id.clone() {
        let mut guard = sessions.lock().await;
        guard.insert(turn.channel, session_id.clone());
    }

    Ok(ClaudeAgentAnswer {
        text: parsed.text.trim().to_string(),
        session_id: parsed.session_id,
        plugins: parsed.plugins,
        skills: parsed.skills,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node_available() -> bool {
        std::process::Command::new("node")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn mock_sidecar_preserves_channel_session() -> Result<()> {
        if !node_available() {
            eprintln!("skipping mock_sidecar_preserves_channel_session: node not available");
            return Ok(());
        }
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("crate is under repo/crates/freeq-raven")
            .to_path_buf();
        let mock_state = std::env::temp_dir().join(format!(
            "freeq-raven-claude-agent-mock-{}.json",
            std::process::id()
        ));
        let cfg = ClaudeAgentConfig {
            command: format!(
                "RAVEN_CLAUDE_AGENT_MOCK=1 RAVEN_CLAUDE_AGENT_MOCK_STATE={} node {}",
                mock_state.display(),
                repo_root.join("scripts/claude-agent-sidecar.mjs").display()
            ),
            workdir: Some(repo_root),
            alexandria_plugin_path: None,
            model: None,
            permission_mode: "dontAsk".to_string(),
            max_turns: 2,
            timeout: Duration::from_secs(10),
        };
        let sessions: ClaudeSessionMap =
            std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let first = ask(
            &cfg,
            &sessions,
            ClaudeAgentTurn {
                channel: "#alexandria".to_string(),
                asker: "alice".to_string(),
                source: "chat".to_string(),
                question: "Raven, remember that the launch codename is Night Library.".to_string(),
                session_context: String::new(),
                system_prompt: "You are Raven.".to_string(),
            },
        )
        .await?;
        assert!(first.session_id.is_some());
        assert!(first.skills.iter().any(|s| s == "alexandria:ax-start"));

        let second = ask(
            &cfg,
            &sessions,
            ClaudeAgentTurn {
                channel: "#alexandria".to_string(),
                asker: "alice".to_string(),
                source: "chat".to_string(),
                question: "Raven, what did I ask you to remember?".to_string(),
                session_context: String::new(),
                system_prompt: "You are Raven.".to_string(),
            },
        )
        .await?;
        assert_eq!(first.session_id, second.session_id);
        assert!(second.text.contains("Night Library"));
        Ok(())
    }
}
