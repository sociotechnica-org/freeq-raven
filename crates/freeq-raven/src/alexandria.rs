use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use freeq_sdk::client::ClientHandle;
use serde::Deserialize;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

#[derive(Debug, Clone)]
pub struct AlexandriaConfig {
    pub ax_bin: String,
    pub connection_id: String,
    pub poll_interval_ms: u64,
    pub workdir: PathBuf,
}

#[derive(Debug, Clone)]
pub struct PendingFeedback {
    pub choices: Vec<String>,
    pub draft_artifact_path: Option<String>,
    pub fabro_run_id: String,
    pub play_id: String,
    pub play_run_id: String,
    pub prompt: String,
    pub question_id: String,
}

pub type PendingFeedbackStore = Arc<Mutex<HashMap<String, Vec<PendingFeedback>>>>;

#[derive(Debug, Clone, Deserialize)]
struct StateEvent {
    #[serde(default)]
    id: String,
    #[serde(rename = "type")]
    event_type: String,
    payload: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HumanInputPayload {
    #[serde(default)]
    choices: Vec<String>,
    draft_artifact_path: Option<String>,
    fabro_run_id: String,
    play_id: String,
    play_run_id: String,
    prompt: String,
    question_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TerminalPayload {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    fabro_run_id: Option<String>,
    play_id: String,
}

#[derive(Debug, Deserialize)]
struct InspectState {
    workspace: InspectStateWorkspace,
}

#[derive(Debug, Deserialize)]
struct InspectStateWorkspace {
    path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FeedbackAction {
    ApproveSelect(String),
    ApproveYes,
    MissingApproveChoice,
    MissingRevisionText,
    Revise(String),
}

pub fn pending_feedback_store() -> PendingFeedbackStore {
    Arc::new(Mutex::new(HashMap::new()))
}

pub fn spawn_monitor(
    config: AlexandriaConfig,
    handle: Arc<ClientHandle>,
    channels: Vec<String>,
    pending: PendingFeedbackStore,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        if channels.is_empty() {
            tracing::warn!("alexandria monitor disabled: no Freeq channels configured");
            return;
        }
        let channel = channels[0].clone();

        if let Err(error) = register_room_bot_subscription(&config).await {
            tracing::warn!(error = ?error, "failed to register Alexandria room-bot subscription");
        }
        if let Err(error) = reconcile_startup_pending(&config, &handle, &channel, &pending).await {
            tracing::warn!(error = ?error, "failed to reconcile Alexandria pending play feedback");
        }

        loop {
            if let Err(error) =
                run_ledger_poll_loop(&config, &handle, &channel, pending.clone()).await
            {
                tracing::warn!(error = ?error, "Alexandria ledger poller stopped");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    })
}

pub async fn handle_addressed_feedback(
    config: AlexandriaConfig,
    handle: Arc<ClientHandle>,
    pending: PendingFeedbackStore,
    channel: &str,
    asker: &str,
    text: &str,
) -> bool {
    let pending_feedback = {
        let guard = pending.lock().await;
        match guard.get(&channel_key(channel)) {
            None => return false,
            Some(entries) if entries.is_empty() => return false,
            Some(entries) if entries.len() > 1 => {
                let _ = handle
                    .privmsg(
                        channel,
                        &format!(
                            "{asker}: I have multiple play feedback gates open. Please mention the run or question id so I do not cross-wire the answer."
                        ),
                    )
                    .await;
                return true;
            }
            Some(entries) => entries[0].clone(),
        }
    };

    let Some(action) = interpret_feedback_reply(text, &pending_feedback.choices) else {
        return false;
    };

    match action {
        FeedbackAction::MissingApproveChoice => {
            let _ = handle
                .privmsg(
                    channel,
                    &format!(
                        "{asker}: I could not find an approve choice on that play gate. Try `Raven, revise: <feedback>` instead."
                    ),
                )
                .await;
            true
        }
        FeedbackAction::MissingRevisionText => {
            let _ = handle
                .privmsg(
                    channel,
                    &format!(
                        "{asker}: give me the revision as `Raven, revise: <feedback>` and I will send it to the play."
                    ),
                )
                .await;
            true
        }
        FeedbackAction::ApproveSelect(choice) => {
            let result = submit_feedback_answer(
                &config,
                &pending_feedback,
                AnswerSubmission::Select(choice),
            )
            .await;
            handle_submission_result(result, &handle, &pending, channel, asker, pending_feedback)
                .await;
            true
        }
        FeedbackAction::ApproveYes => {
            let result =
                submit_feedback_answer(&config, &pending_feedback, AnswerSubmission::Yes).await;
            handle_submission_result(result, &handle, &pending, channel, asker, pending_feedback)
                .await;
            true
        }
        FeedbackAction::Revise(feedback) => {
            let result = submit_feedback_answer(
                &config,
                &pending_feedback,
                AnswerSubmission::Text(feedback),
            )
            .await;
            handle_submission_result(result, &handle, &pending, channel, asker, pending_feedback)
                .await;
            true
        }
    }
}

async fn handle_submission_result(
    result: Result<()>,
    handle: &ClientHandle,
    pending: &PendingFeedbackStore,
    channel: &str,
    asker: &str,
    feedback: PendingFeedback,
) {
    match result {
        Ok(()) => {
            remove_pending(pending, &feedback.fabro_run_id, &feedback.question_id).await;
            let _ = handle
                .privmsg(
                    channel,
                    &format!("{asker}: sent your feedback back to the play."),
                )
                .await;
        }
        Err(error) => {
            let _ = handle
                .privmsg(
                    channel,
                    &format!("{asker}: I could not send that feedback to the play ({error})."),
                )
                .await;
        }
    }
}

async fn register_room_bot_subscription(config: &AlexandriaConfig) -> Result<()> {
    let subscription_id = format!("{}:frame-the-problem", config.connection_id);
    let output = Command::new(&config.ax_bin)
        .current_dir(&config.workdir)
        .args([
            "inspect",
            "subscriptions",
            "register",
            "--subscription",
            &subscription_id,
            "--connection",
            &config.connection_id,
            "--host",
            "freeq-raven",
            "--type",
            "play.human_input_requested",
            "--type",
            "play.human_input_resolved",
            "--type",
            "play.completed",
            "--type",
            "play.failed",
            "--json",
        ])
        .output()
        .await
        .with_context(|| format!("running {} inspect subscriptions register", config.ax_bin))?;

    if !output.status.success() {
        return Err(anyhow!(
            "subscription registration failed: {}",
            command_error(&output)
        ));
    }

    Ok(())
}

async fn run_ledger_poll_loop(
    config: &AlexandriaConfig,
    handle: &Arc<ClientHandle>,
    channel: &str,
    pending: PendingFeedbackStore,
) -> Result<()> {
    let workspace = resolve_workspace_path(config).await?;
    let ledger_path = workspace.join("ledger").join("events.jsonl");
    let mut seen = read_state_events_from_ledger(&ledger_path).await?.len();

    loop {
        tokio::time::sleep(Duration::from_millis(config.poll_interval_ms)).await;
        let events = read_state_events_from_ledger(&ledger_path).await?;
        if events.len() < seen {
            seen = 0;
        }
        for event in events.iter().skip(seen) {
            if let Err(error) = handle_state_event(config, handle, channel, &pending, event).await {
                tracing::warn!(error = ?error, event_type = %event.event_type, "failed to handle Alexandria ledger event");
            }
        }
        seen = events.len();
    }
}

async fn handle_state_event(
    config: &AlexandriaConfig,
    handle: &ClientHandle,
    channel: &str,
    pending: &PendingFeedbackStore,
    event: &StateEvent,
) -> Result<()> {
    match event.event_type.as_str() {
        "play.human_input_requested" => {
            let Some(feedback) = pending_from_event(event)? else {
                return Ok(());
            };
            if add_pending(pending, channel, feedback.clone()).await {
                post_review_notice(config, handle, channel, &feedback).await;
            }
        }
        "play.human_input_resolved" => {
            if let Some((fabro_run_id, question_id)) = question_key_from_payload(&event.payload) {
                remove_pending(pending, &fabro_run_id, &question_id).await;
            }
        }
        "play.completed" => {
            let payload: TerminalPayload = serde_json::from_value(event.payload.clone())
                .context("decoding play.completed payload")?;
            if payload.play_id == "frame-the-problem" {
                if let Some(fabro_run_id) = payload.fabro_run_id.as_deref() {
                    remove_pending_for_run(pending, fabro_run_id).await;
                }
                post_completion_notice(config, handle, channel).await;
            }
        }
        "play.failed" => {
            let payload: TerminalPayload = serde_json::from_value(event.payload.clone())
                .context("decoding play.failed payload")?;
            if payload.play_id == "frame-the-problem" {
                if let Some(fabro_run_id) = payload.fabro_run_id.as_deref() {
                    remove_pending_for_run(pending, fabro_run_id).await;
                }
                let error = payload
                    .error
                    .as_deref()
                    .unwrap_or("the play runtime reported failure");
                let _ = handle
                    .privmsg(channel, &format!("Frame the Problem failed: {error}"))
                    .await;
            }
        }
        _ => {}
    }
    Ok(())
}

async fn reconcile_startup_pending(
    config: &AlexandriaConfig,
    handle: &ClientHandle,
    channel: &str,
    pending: &PendingFeedbackStore,
) -> Result<()> {
    let workspace = resolve_workspace_path(config).await?;
    let ledger_path = workspace.join("ledger").join("events.jsonl");
    let events = read_state_events_from_ledger(&ledger_path).await?;
    let mut open: HashMap<String, PendingFeedback> = HashMap::new();

    for event in events {
        match event.event_type.as_str() {
            "play.human_input_requested" => {
                if let Some(feedback) = pending_from_event(&event)? {
                    open.insert(feedback_key(&feedback), feedback);
                }
            }
            "play.human_input_resolved" => {
                if let Some((fabro_run_id, question_id)) = question_key_from_payload(&event.payload)
                {
                    open.remove(&format!("{fabro_run_id}:{question_id}"));
                }
            }
            "play.completed" | "play.failed" => {
                if let Some(fabro_run_id) = event
                    .payload
                    .get("fabroRunId")
                    .and_then(|value| value.as_str())
                {
                    open.retain(|_, feedback| feedback.fabro_run_id != fabro_run_id);
                }
            }
            _ => {}
        }
    }

    for feedback in open.into_values() {
        if add_pending(pending, channel, feedback.clone()).await {
            post_review_notice(config, handle, channel, &feedback).await;
        }
    }

    Ok(())
}

async fn read_state_events_from_ledger(path: &Path) -> Result<Vec<StateEvent>> {
    let ledger = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("reading {}", path.display()))?;
    Ok(ledger
        .lines()
        .filter_map(|line| match serde_json::from_str::<StateEvent>(line) {
            Ok(event) => Some(event),
            Err(error) => {
                tracing::debug!(error = ?error, "skipping malformed Alexandria ledger line");
                None
            }
        })
        .collect())
}

fn pending_from_event(event: &StateEvent) -> Result<Option<PendingFeedback>> {
    let payload: HumanInputPayload = serde_json::from_value(event.payload.clone())
        .with_context(|| format!("decoding {} payload {}", event.event_type, event.id))?;
    if payload.play_id != "frame-the-problem" {
        return Ok(None);
    }
    Ok(Some(PendingFeedback {
        choices: payload.choices,
        draft_artifact_path: payload.draft_artifact_path,
        fabro_run_id: payload.fabro_run_id,
        play_id: payload.play_id,
        play_run_id: payload.play_run_id,
        prompt: payload.prompt,
        question_id: payload.question_id,
    }))
}

fn question_key_from_payload(payload: &serde_json::Value) -> Option<(String, String)> {
    let fabro_run_id = payload.get("fabroRunId")?.as_str()?.to_string();
    let question_id = payload.get("questionId")?.as_str()?.to_string();
    Some((fabro_run_id, question_id))
}

async fn add_pending(
    pending: &PendingFeedbackStore,
    channel: &str,
    feedback: PendingFeedback,
) -> bool {
    let mut guard = pending.lock().await;
    let entries = guard.entry(channel_key(channel)).or_default();
    if entries.iter().any(|entry| {
        entry.fabro_run_id == feedback.fabro_run_id && entry.question_id == feedback.question_id
    }) {
        return false;
    }
    entries.push(feedback);
    true
}

async fn remove_pending(pending: &PendingFeedbackStore, fabro_run_id: &str, question_id: &str) {
    let mut guard = pending.lock().await;
    for entries in guard.values_mut() {
        entries
            .retain(|entry| entry.fabro_run_id != fabro_run_id || entry.question_id != question_id);
    }
}

async fn remove_pending_for_run(pending: &PendingFeedbackStore, fabro_run_id: &str) {
    let mut guard = pending.lock().await;
    for entries in guard.values_mut() {
        entries.retain(|entry| entry.fabro_run_id != fabro_run_id);
    }
}

fn feedback_key(feedback: &PendingFeedback) -> String {
    format!("{}:{}", feedback.fabro_run_id, feedback.question_id)
}

fn channel_key(channel: &str) -> String {
    channel.to_ascii_lowercase()
}

async fn post_review_notice(
    config: &AlexandriaConfig,
    handle: &ClientHandle,
    channel: &str,
    feedback: &PendingFeedback,
) {
    let director = read_runtime_file(config, "for-the-director.md").await;
    let draft = read_draft(config, feedback).await;
    let mut message = String::new();
    message.push_str("Frame the Problem is ready for your feedback.\n");
    message.push_str(&format!("Question: {}\n", feedback.prompt));
    if let Some(director) = director {
        message.push_str("\nFor you to react to:\n");
        message.push_str(&excerpt_chars(&director, 1400));
        message.push('\n');
    }
    if let Some(draft) = draft {
        message.push_str("\nCurrent draft:\n");
        message.push_str(&excerpt_chars(&draft, 1800));
        message.push('\n');
    }
    message.push_str(
        "\nReply `Raven, approve` to finish, or `Raven, revise: <feedback>` and I will send it back to the play.",
    );
    post_long(handle, channel, &message).await;
}

async fn post_completion_notice(config: &AlexandriaConfig, handle: &ClientHandle, channel: &str) {
    let draft = read_runtime_file(config, "problem-framing.md").await;
    let mut message = String::new();
    message.push_str("Frame the Problem finished.\n");
    if let Some(draft) = draft {
        message.push_str("\nFinal framing:\n");
        message.push_str(&excerpt_chars(&draft, 3600));
        message.push('\n');
    } else {
        message.push_str(
            "I could not read `runtime/problem-framing.md`, but the play reported completion.\n",
        );
    }
    message.push_str("\nReply `Raven, ratify` if this framing is the one you want to keep, or `Raven, loop: <reaction>` if you want another pass.");
    post_long(handle, channel, &message).await;
}

async fn resolve_workspace_path(config: &AlexandriaConfig) -> Result<PathBuf> {
    let output = Command::new(&config.ax_bin)
        .current_dir(&config.workdir)
        .args(["inspect", "state", "--json"])
        .output()
        .await
        .with_context(|| format!("running {} inspect state --json", config.ax_bin))?;
    if !output.status.success() {
        return Err(anyhow!("inspect state failed: {}", command_error(&output)));
    }
    let state: InspectState =
        serde_json::from_slice(&output.stdout).context("decoding ax inspect state JSON")?;
    Ok(PathBuf::from(state.workspace.path))
}

async fn read_runtime_file(config: &AlexandriaConfig, name: &str) -> Option<String> {
    let workspace = match resolve_workspace_path(config).await {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(error = ?error, "could not resolve Alexandria workspace path");
            return None;
        }
    };
    read_file_if_present(&workspace.join("runtime").join(name)).await
}

async fn read_draft(config: &AlexandriaConfig, feedback: &PendingFeedback) -> Option<String> {
    if let Some(path) = feedback.draft_artifact_path.as_deref() {
        if let Some(content) = read_artifact_path(config, path).await {
            return Some(content);
        }
    }
    read_runtime_file(config, "problem-framing.md").await
}

async fn read_artifact_path(config: &AlexandriaConfig, path: &str) -> Option<String> {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        return read_file_if_present(&candidate).await;
    }
    if let Some(content) = read_file_if_present(&config.workdir.join(path)).await {
        return Some(content);
    }
    let workspace = resolve_workspace_path(config).await.ok()?;
    read_file_if_present(&workspace.join(path)).await
}

async fn read_file_if_present(path: &Path) -> Option<String> {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => Some(content),
        Err(error) => {
            tracing::debug!(path = %path.display(), error = ?error, "Alexandria artifact unavailable");
            None
        }
    }
}

fn interpret_feedback_reply(text: &str, choices: &[String]) -> Option<FeedbackAction> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lower = trimmed.to_ascii_lowercase();

    if is_approve_reply(&lower) {
        return match approve_choice(choices) {
            Some(choice) => Some(FeedbackAction::ApproveSelect(choice)),
            None if choices.is_empty() => Some(FeedbackAction::ApproveYes),
            None => Some(FeedbackAction::MissingApproveChoice),
        };
    }

    for prefix in [
        "revise:",
        "revision:",
        "feedback:",
        "change:",
        "changes:",
        "loop:",
    ] {
        if lower.starts_with(prefix) {
            let feedback = trimmed[prefix.len()..].trim();
            return Some(if feedback.is_empty() {
                FeedbackAction::MissingRevisionText
            } else {
                FeedbackAction::Revise(feedback.to_string())
            });
        }
    }

    if matches!(
        lower.as_str(),
        "revise" | "revision" | "feedback" | "changes" | "loop"
    ) {
        return Some(FeedbackAction::MissingRevisionText);
    }

    None
}

fn is_approve_reply(lower: &str) -> bool {
    lower == "approve"
        || lower == "approved"
        || lower == "ship it"
        || lower == "looks good"
        || lower.starts_with("approve ")
        || lower.starts_with("approved ")
}

fn approve_choice(choices: &[String]) -> Option<String> {
    choices
        .iter()
        .find(|choice| {
            let lower = choice.to_ascii_lowercase();
            lower.contains("approve")
                || lower == "a"
                || lower == "yes"
                || lower == "y"
                || lower.contains("finish")
        })
        .cloned()
}

enum AnswerSubmission {
    Select(String),
    Text(String),
    Yes,
}

async fn submit_feedback_answer(
    config: &AlexandriaConfig,
    feedback: &PendingFeedback,
    submission: AnswerSubmission,
) -> Result<()> {
    let mut command = Command::new(&config.ax_bin);
    command.current_dir(&config.workdir).args([
        "raven",
        "answer",
        "--run",
        &feedback.fabro_run_id,
        "--question",
        &feedback.question_id,
    ]);
    match submission {
        AnswerSubmission::Select(choice) => {
            command.args(["--select", &choice]);
        }
        AnswerSubmission::Text(text) => {
            command.args(["--text", &text]);
        }
        AnswerSubmission::Yes => {
            command.arg("--yes");
        }
    }
    command.arg("--json");

    let output = command
        .output()
        .await
        .with_context(|| format!("running {} raven answer", config.ax_bin))?;
    if !output.status.success() {
        return Err(anyhow!(
            "ax raven answer failed: {}",
            command_error(&output)
        ));
    }
    Ok(())
}

fn command_error(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stdout.is_empty() {
        return stdout;
    }
    output.status.to_string()
}

fn excerpt_chars(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    let mut out = String::new();
    let mut count = 0;
    for ch in trimmed.chars() {
        if count >= max_chars {
            out.push_str("\n...");
            return out;
        }
        out.push(ch);
        count += 1;
    }
    out
}

async fn post_long(handle: &ClientHandle, channel: &str, text: &str) {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        for chunk in split_privmsg_chunks(trimmed, 380) {
            let _ = handle.privmsg(channel, &chunk).await;
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }
}

fn split_privmsg_chunks(line: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut count = 0;
    for ch in line.chars() {
        if count >= max_chars {
            chunks.push(current);
            current = String::new();
            count = 0;
        }
        current.push(ch);
        count += 1;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approve_reply_selects_approve_choice() {
        let choices = vec!["approve".to_string(), "revise".to_string()];
        expect_action(
            interpret_feedback_reply("approve", &choices),
            FeedbackAction::ApproveSelect("approve".to_string()),
        );
    }

    #[test]
    fn revise_reply_extracts_feedback_text() {
        expect_action(
            interpret_feedback_reply("revise: make the evidence bar sharper", &[]),
            FeedbackAction::Revise("make the evidence bar sharper".to_string()),
        );
    }

    #[test]
    fn unrelated_reply_falls_through_to_normal_qa() {
        assert!(interpret_feedback_reply("what do you think?", &[]).is_none());
    }

    fn expect_action(actual: Option<FeedbackAction>, expected: FeedbackAction) {
        assert_eq!(actual, Some(expected));
    }
}
