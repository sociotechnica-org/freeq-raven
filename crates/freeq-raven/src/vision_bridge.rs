use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use anyhow::{Context, Result};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use freeq_av::{VideoFrameStatus, VideoHandle};
use rand::Rng;
use rand::distributions::Alphanumeric;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::claude_agent::{ClaudeAgentVisionBridge, ClaudeAgentVisionParticipant};
use crate::vision;

#[derive(Clone)]
pub struct VisionBridgeHandle {
    endpoint: String,
    bearer_token: String,
    registry: VisionFrameRegistry,
    _task: Arc<BridgeTask>,
}

impl VisionBridgeHandle {
    pub async fn start() -> Result<Self> {
        Self::start_with_registry(VisionFrameRegistry::default()).await
    }

    async fn start_with_registry(registry: VisionFrameRegistry) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .context("binding Raven vision bridge")?;
        let addr = listener
            .local_addr()
            .context("reading Raven vision bridge address")?;
        let token: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(48)
            .map(char::from)
            .collect();
        let state = Arc::new(ServerState {
            token: token.clone(),
            registry: registry.clone(),
        });
        let app = Router::new()
            .route("/latest-frame", post(latest_frame_handler))
            .with_state(state);
        let task = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, app).await {
                tracing::warn!(error = ?error, "Raven vision bridge stopped");
            }
        });

        Ok(Self {
            endpoint: format!("http://{addr}"),
            bearer_token: token,
            registry,
            _task: Arc::new(BridgeTask { task }),
        })
    }

    pub fn activate_channel(&self, channel: &str) {
        self.registry.activate_channel(channel);
    }

    pub fn clear_channel(&self, channel: &str) {
        self.registry.clear_channel(channel);
    }

    pub fn register_participant(&self, channel: &str, participant: &str, video: VideoHandle) {
        self.registry
            .register_participant(channel, participant, video);
    }

    pub fn remove_participant(&self, channel: &str, participant: &str) {
        self.registry.remove_participant(channel, participant);
    }

    pub fn sidecar_descriptor(&self, channel: &str, asker: &str) -> ClaudeAgentVisionBridge {
        ClaudeAgentVisionBridge {
            endpoint: self.endpoint.clone(),
            bearer_token: self.bearer_token.clone(),
            channel: channel.to_string(),
            asker: asker.to_string(),
            participants: self.registry.participants(channel),
        }
    }
}

struct BridgeTask {
    task: JoinHandle<()>,
}

impl Drop for BridgeTask {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone, Default)]
pub struct VisionFrameRegistry {
    inner: Arc<RwLock<HashMap<String, ChannelFrames>>>,
}

#[derive(Default)]
struct ChannelFrames {
    participants: HashMap<String, ParticipantFrameSource>,
}

#[derive(Clone)]
struct ParticipantFrameSource {
    display_name: String,
    source: FrameSource,
}

#[derive(Clone)]
enum FrameSource {
    Live(VideoHandle),
    #[cfg(test)]
    Fixed(VisionBridgeResponse),
}

impl VisionFrameRegistry {
    pub fn activate_channel(&self, channel: &str) {
        if let Ok(mut guard) = self.inner.write() {
            guard.entry(normalize(channel)).or_default();
        }
    }

    pub fn clear_channel(&self, channel: &str) {
        if let Ok(mut guard) = self.inner.write() {
            guard.remove(&normalize(channel));
        }
    }

    pub fn register_participant(&self, channel: &str, participant: &str, video: VideoHandle) {
        if let Ok(mut guard) = self.inner.write() {
            let channel = guard.entry(normalize(channel)).or_default();
            channel.participants.insert(
                normalize(participant),
                ParticipantFrameSource {
                    display_name: participant.to_string(),
                    source: FrameSource::Live(video),
                },
            );
        }
    }

    pub fn remove_participant(&self, channel: &str, participant: &str) {
        if let Ok(mut guard) = self.inner.write() {
            if let Some(channel) = guard.get_mut(&normalize(channel)) {
                channel.participants.remove(&normalize(participant));
            }
        }
    }

    pub fn participants(&self, channel: &str) -> Vec<ClaudeAgentVisionParticipant> {
        let Ok(guard) = self.inner.read() else {
            return Vec::new();
        };
        let Some(channel) = guard.get(&normalize(channel)) else {
            return Vec::new();
        };
        let mut participants: Vec<_> = channel
            .participants
            .values()
            .map(|participant| {
                let (frame_available, frame_stale) = participant.availability();
                ClaudeAgentVisionParticipant {
                    name: participant.display_name.clone(),
                    frame_available,
                    frame_stale,
                }
            })
            .collect();
        participants.sort_by(|a, b| a.name.cmp(&b.name));
        participants
    }

    fn latest_frame(
        &self,
        channel: &str,
        participant: Option<&str>,
        asker: Option<&str>,
    ) -> VisionBridgeResponse {
        let target = participant
            .filter(|value| !value.trim().is_empty())
            .or_else(|| asker.filter(|value| !value.trim().is_empty()));
        let Ok(guard) = self.inner.read() else {
            return VisionBridgeResponse::failure(
                VisionBridgeReason::NoActiveCall,
                channel,
                target,
            );
        };
        let Some(channel_frames) = guard.get(&normalize(channel)) else {
            return VisionBridgeResponse::failure(
                VisionBridgeReason::NoActiveCall,
                channel,
                target,
            );
        };
        let Some(target) = target else {
            return VisionBridgeResponse::failure(
                VisionBridgeReason::UnknownParticipant,
                channel,
                None,
            );
        };
        let Some(source) = channel_frames.participants.get(&normalize(target)) else {
            return VisionBridgeResponse::failure(
                VisionBridgeReason::UnknownParticipant,
                channel,
                Some(target),
            );
        };
        source.response(channel)
    }
}

impl ParticipantFrameSource {
    fn availability(&self) -> (bool, bool) {
        match &self.source {
            FrameSource::Live(video) => match video.latest_status() {
                VideoFrameStatus::Fresh(_) => (true, false),
                VideoFrameStatus::Stale { .. } => (false, true),
                VideoFrameStatus::Missing => (false, false),
            },
            #[cfg(test)]
            FrameSource::Fixed(response) => (
                response.ok,
                response.reason == Some(VisionBridgeReason::StaleFrame),
            ),
        }
    }

    fn response(&self, channel: &str) -> VisionBridgeResponse {
        match &self.source {
            FrameSource::Live(video) => match video.latest_status() {
                VideoFrameStatus::Fresh(snapshot) => {
                    match vision::frame_to_jpeg_data_uri(&snapshot.frame) {
                        Ok(data_uri) => VisionBridgeResponse {
                            ok: true,
                            reason: None,
                            channel: Some(channel.to_string()),
                            participant: Some(self.display_name.clone()),
                            mime: Some("image/jpeg".to_string()),
                            data_uri: Some(data_uri),
                            dimensions: Some(snapshot.frame.dimensions),
                            captured_at: Some(system_time_rfc3339(snapshot.captured_at)),
                        },
                        Err(error) => {
                            tracing::warn!(
                                error = ?error,
                                participant = %self.display_name,
                                "failed to encode latest vision bridge frame"
                            );
                            VisionBridgeResponse::failure(
                                VisionBridgeReason::NoFrame,
                                channel,
                                Some(&self.display_name),
                            )
                        }
                    }
                }
                VideoFrameStatus::Stale { .. } => VisionBridgeResponse::failure(
                    VisionBridgeReason::StaleFrame,
                    channel,
                    Some(&self.display_name),
                ),
                VideoFrameStatus::Missing => VisionBridgeResponse::failure(
                    VisionBridgeReason::NoFrame,
                    channel,
                    Some(&self.display_name),
                ),
            },
            #[cfg(test)]
            FrameSource::Fixed(response) => response.clone(),
        }
    }
}

#[derive(Clone)]
struct ServerState {
    token: String,
    registry: VisionFrameRegistry,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LatestFrameRequest {
    channel: String,
    #[serde(default)]
    asker: Option<String>,
    #[serde(default)]
    participant: Option<String>,
}

async fn latest_frame_handler(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<LatestFrameRequest>,
) -> (StatusCode, Json<VisionBridgeResponse>) {
    if !authorized(&headers, &state.token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(VisionBridgeResponse::failure(
                VisionBridgeReason::Unauthorized,
                &req.channel,
                req.participant.as_deref().or(req.asker.as_deref()),
            )),
        );
    }
    (
        StatusCode::OK,
        Json(state.registry.latest_frame(
            &req.channel,
            req.participant.as_deref(),
            req.asker.as_deref(),
        )),
    )
}

fn authorized(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|value| value == token)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VisionBridgeResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<VisionBridgeReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub participant: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    #[serde(rename = "dataUri", skip_serializing_if = "Option::is_none")]
    pub data_uri: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<[u32; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<String>,
}

impl VisionBridgeResponse {
    fn failure(reason: VisionBridgeReason, channel: &str, participant: Option<&str>) -> Self {
        Self {
            ok: false,
            reason: Some(reason),
            channel: Some(channel.to_string()),
            participant: participant.map(str::to_string),
            mime: None,
            data_uri: None,
            dimensions: None,
            captured_at: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VisionBridgeReason {
    NoActiveCall,
    NoFrame,
    UnknownParticipant,
    StaleFrame,
    Unauthorized,
}

fn normalize(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

fn system_time_rfc3339(value: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(value).to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_response(reason: VisionBridgeReason) -> VisionBridgeResponse {
        VisionBridgeResponse::failure(reason, "#alexandria", Some("alice"))
    }

    #[test]
    fn latest_frame_reports_no_active_call() {
        let registry = VisionFrameRegistry::default();
        let response = registry.latest_frame("#alexandria", Some("alice"), None);
        assert!(!response.ok);
        assert_eq!(response.reason, Some(VisionBridgeReason::NoActiveCall));
    }

    #[test]
    fn latest_frame_reports_unknown_participant() {
        let registry = VisionFrameRegistry::default();
        registry.activate_channel("#alexandria");
        let response = registry.latest_frame("#alexandria", Some("alice"), None);
        assert!(!response.ok);
        assert_eq!(
            response.reason,
            Some(VisionBridgeReason::UnknownParticipant)
        );
    }

    #[test]
    fn latest_frame_reports_no_frame_for_known_participant() {
        let registry = VisionFrameRegistry::default();
        registry.register_participant("#alexandria", "alice", VideoHandle::default());
        let response = registry.latest_frame("#alexandria", Some("alice"), None);
        assert!(!response.ok);
        assert_eq!(response.reason, Some(VisionBridgeReason::NoFrame));
    }

    #[test]
    fn latest_frame_reports_stale_frame_without_panicking() {
        let registry = VisionFrameRegistry::default();
        registry.activate_channel("#alexandria");
        {
            let mut guard = registry.inner.write().expect("registry lock");
            let channel = guard.get_mut("#alexandria").expect("channel active");
            channel.participants.insert(
                "alice".to_string(),
                ParticipantFrameSource {
                    display_name: "alice".to_string(),
                    source: FrameSource::Fixed(fixed_response(VisionBridgeReason::StaleFrame)),
                },
            );
        }
        let response = registry.latest_frame("#alexandria", Some("alice"), None);
        assert!(!response.ok);
        assert_eq!(response.reason, Some(VisionBridgeReason::StaleFrame));
    }

    #[tokio::test]
    async fn http_bridge_requires_bearer_token() -> Result<()> {
        let bridge = VisionBridgeHandle::start().await?;
        let client = reqwest::Client::new();
        let response: VisionBridgeResponse = client
            .post(format!("{}/latest-frame", bridge.endpoint))
            .json(&serde_json::json!({
                "channel": "#alexandria",
                "asker": "alice",
            }))
            .send()
            .await?
            .json()
            .await?;
        assert_eq!(response.reason, Some(VisionBridgeReason::Unauthorized));
        Ok(())
    }

    #[tokio::test]
    async fn http_bridge_returns_structured_no_frame() -> Result<()> {
        let bridge = VisionBridgeHandle::start().await?;
        bridge.register_participant("#alexandria", "alice", VideoHandle::default());
        let client = reqwest::Client::new();
        let response: VisionBridgeResponse = client
            .post(format!("{}/latest-frame", bridge.endpoint))
            .bearer_auth(&bridge.bearer_token)
            .json(&serde_json::json!({
                "channel": "#alexandria",
                "asker": "alice",
            }))
            .send()
            .await?
            .json()
            .await?;
        assert_eq!(response.reason, Some(VisionBridgeReason::NoFrame));
        Ok(())
    }
}
