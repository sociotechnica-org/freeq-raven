use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

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
    pub silent_allowed: bool,
}

#[derive(Debug, Clone)]
pub struct ClaudeAgentAnswer {
    pub text: String,
    pub action: ClaudeAgentAction,
    pub session_id: Option<String>,
    pub plugins: Vec<ClaudeAgentPlugin>,
    pub skills: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeAgentAction {
    Reply,
    Ignore,
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
    silent_allowed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SidecarResponse {
    ok: bool,
    #[serde(default)]
    action: Option<String>,
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
        silent_allowed: turn.silent_allowed,
    };
    let payload = serde_json::to_vec(&req).context("encoding claude agent request")?;

    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.arg("-lc")
        .arg(&cfg.command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.kill_on_drop(true);
    if let Some(workdir) = &cfg.workdir {
        cmd.current_dir(workdir);
    }

    let mut child = cmd.spawn().context("starting claude agent sidecar")?;
    let stdout = child
        .stdout
        .take()
        .context("capturing claude agent sidecar stdout")?;
    let stderr = child
        .stderr
        .take()
        .context("capturing claude agent sidecar stderr")?;
    let stdout_task = tokio::spawn(async move {
        let mut stdout = stdout;
        let mut output = Vec::new();
        stdout.read_to_end(&mut output).await.map(|_| output)
    });
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut captured = String::new();
        while let Some(line) = lines.next_line().await? {
            if !captured.is_empty() {
                captured.push('\n');
            }
            captured.push_str(&line);
            tracing::info!(line = %line, "claude agent sidecar");
        }
        Ok::<String, std::io::Error>(captured)
    });
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

    let status = match tokio::time::timeout(cfg.timeout, child.wait()).await {
        Ok(result) => result.context("waiting for claude agent sidecar")?,
        Err(_) => {
            let _ = child.kill().await;
            let stderr = stderr_task
                .await
                .ok()
                .and_then(|result| result.ok())
                .unwrap_or_default();
            anyhow::bail!(
                "claude agent sidecar timed out after {}s{}",
                cfg.timeout.as_secs(),
                if stderr.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", stderr.trim())
                }
            );
        }
    };

    let stdout_bytes = stdout_task
        .await
        .context("joining claude agent stdout task")?
        .context("reading claude agent sidecar stdout")?;
    let stderr = stderr_task
        .await
        .context("joining claude agent stderr task")?
        .context("reading claude agent sidecar stderr")?;
    let stdout = String::from_utf8_lossy(&stdout_bytes);
    if !status.success() {
        anyhow::bail!("claude agent sidecar exited {}: {}", status, stderr.trim());
    }
    let mut parsed = None;
    let mut last_error = None;
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        match serde_json::from_str::<SidecarResponse>(line) {
            Ok(response) => parsed = Some(response),
            Err(error) => {
                tracing::warn!(
                    error = ?error,
                    chars = line.chars().count(),
                    "ignoring non-JSON claude agent sidecar stdout"
                );
                last_error = Some(error);
            }
        }
    }
    let parsed = parsed.ok_or_else(|| {
        if let Some(error) = last_error {
            anyhow::anyhow!("decoding claude agent sidecar response: {error}")
        } else {
            anyhow::anyhow!("claude agent sidecar returned no JSON")
        }
    })?;
    if !parsed.ok {
        anyhow::bail!(
            "claude agent sidecar failed: {}",
            parsed.error.unwrap_or_else(|| "unknown error".to_string())
        );
    }
    let action = match parsed.action.as_deref() {
        Some("ignore") => ClaudeAgentAction::Ignore,
        _ => ClaudeAgentAction::Reply,
    };
    if action == ClaudeAgentAction::Reply && parsed.text.trim().is_empty() {
        anyhow::bail!("claude agent sidecar returned empty text");
    }
    if let Some(session_id) = parsed.session_id.clone() {
        let mut guard = sessions.lock().await;
        guard.insert(turn.channel, session_id.clone());
    }

    Ok(ClaudeAgentAnswer {
        text: parsed.text.trim().to_string(),
        action,
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
    async fn sidecar_requires_anthropic_api_key() -> Result<()> {
        if !node_available() {
            eprintln!("skipping sidecar_requires_anthropic_api_key: node not available");
            return Ok(());
        }
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("crate is under repo/crates/freeq-raven")
            .to_path_buf();
        let sidecar = repo_root.join("scripts/claude-agent-sidecar.mjs");
        let cfg = ClaudeAgentConfig {
            command: format!(
                "env -u ANTHROPIC_API_KEY node '{}'",
                sidecar.display().to_string().replace('\'', "'\\''")
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
        let err = ask(
            &cfg,
            &sessions,
            ClaudeAgentTurn {
                channel: "#alexandria".to_string(),
                asker: "alice".to_string(),
                source: "chat".to_string(),
                question: "Raven, are you connected to Claude?".to_string(),
                session_context: String::new(),
                system_prompt: "You are Raven.".to_string(),
                silent_allowed: false,
            },
        )
        .await
        .expect_err("sidecar without ANTHROPIC_API_KEY should fail");
        assert!(
            err.to_string()
                .contains("ANTHROPIC_API_KEY is required for Claude Agent SDK sidecar"),
            "unexpected error: {err:#}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn sidecar_ignore_response_allows_empty_text() -> Result<()> {
        let cfg = ClaudeAgentConfig {
            command:
                "printf '%s\n' '{\"ok\":true,\"action\":\"ignore\",\"text\":\"\",\"sessionId\":\"s-1\"}'"
                    .to_string(),
            workdir: None,
            alexandria_plugin_path: None,
            model: None,
            permission_mode: "dontAsk".to_string(),
            max_turns: 2,
            timeout: Duration::from_secs(10),
        };
        let sessions: ClaudeSessionMap =
            std::sync::Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let answer = ask(
            &cfg,
            &sessions,
            ClaudeAgentTurn {
                channel: "#alexandria".to_string(),
                asker: "alice".to_string(),
                source: "chat".to_string(),
                question: "candidate follow-up".to_string(),
                session_context: String::new(),
                system_prompt: "You are Raven.".to_string(),
                silent_allowed: true,
            },
        )
        .await?;

        assert_eq!(answer.action, ClaudeAgentAction::Ignore);
        assert_eq!(answer.text, "");
        assert_eq!(answer.session_id.as_deref(), Some("s-1"));
        assert_eq!(
            sessions.lock().await.get("#alexandria").map(String::as_str),
            Some("s-1")
        );
        Ok(())
    }
}
