//! Explicit live smoke tests against irc.freeq.at.
//!
//! These are ignored by default because they hit the public service. Run with:
//!
//!   cargo test -p freeq-raven --test live_irc_freeq_at_test \
//!     live_irc_freeq_at_addressed_chat_uses_claude_agent_session -- --ignored --nocapture

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use freeq_raven::claude_agent::ClaudeAgentConfig;
use freeq_raven::irc::{AuthIdentity, RunConfig, run};
use freeq_raven::stt::SttEngine;
use freeq_sdk::client::{self, ClientHandle, ConnectConfig};
use freeq_sdk::event::Event;
use tokio::sync::mpsc::Receiver;

mod common;
use common::{claude_agent_sidecar_config, mint_identity};

const LIVE_SERVER: &str = "wss://irc.freeq.at/irc";
const LIVE_CHANNEL: &str = "#alexandria-test";
const SETTLE: Duration = Duration::from_secs(30);

struct Witness {
    handle: ClientHandle,
    events: Receiver<Event>,
}

impl Witness {
    async fn join(server: &str, nick: &str, channel: &str) -> Result<Self> {
        let (server_addr, websocket_url, tls) = connect_target(server)?;
        let config = ConnectConfig {
            server_addr,
            nick: nick.to_string(),
            user: nick.to_string(),
            realname: "freeq-raven-live-witness".to_string(),
            tls,
            tls_insecure: false,
            web_token: None,
            websocket_url,
        };
        let (handle, mut events) = client::connect(config, None);

        wait_for(&mut events, SETTLE, |ev| match ev {
            Event::Registered { nick } => Some(nick.clone()),
            _ => None,
        })
        .await
        .expect("witness did not register");

        handle.join(channel).await?;
        wait_for(&mut events, SETTLE, |ev| match ev {
            Event::Joined {
                channel: c,
                nick: n,
                ..
            } if c.eq_ignore_ascii_case(channel) && n.eq_ignore_ascii_case(nick) => Some(()),
            _ => None,
        })
        .await
        .expect("witness did not join live channel");

        Ok(Self { handle, events })
    }

    async fn wait_for<T>(
        &mut self,
        dur: Duration,
        f: impl FnMut(&Event) -> Option<T>,
    ) -> Option<T> {
        wait_for(&mut self.events, dur, f).await
    }
}

async fn wait_for<T>(
    events: &mut Receiver<Event>,
    dur: Duration,
    mut f: impl FnMut(&Event) -> Option<T>,
) -> Option<T> {
    let deadline = Instant::now() + dur;
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::ZERO);
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, events.recv()).await {
            Ok(Some(ev)) => {
                if let Some(v) = f(&ev) {
                    return Some(v);
                }
            }
            Ok(None) | Err(_) => return None,
        }
    }
}

fn connect_target(server: &str) -> Result<(String, Option<String>, bool)> {
    if server.starts_with("ws://")
        || server.starts_with("wss://")
        || server.starts_with("http://")
        || server.starts_with("https://")
    {
        let url: url::Url = server.parse()?;
        let host = url.host_str().unwrap_or("localhost");
        let port = url.port_or_known_default().unwrap_or(443);
        Ok((
            format!("{host}:{port}"),
            Some(server.to_string()),
            server.starts_with("wss://") || server.starts_with("https://"),
        ))
    } else {
        Ok((server.to_string(), None, false))
    }
}

fn spawn_live_bot(
    server: &str,
    channel: &str,
    bot_name: &str,
    claude_agent: ClaudeAgentConfig,
) -> (
    tokio::task::JoinHandle<anyhow::Result<()>>,
    tempfile::TempDir,
) {
    let (ident, tmp) = mint_identity(bot_name);
    let cfg = RunConfig {
        server: server.to_string(),
        channels: vec![channel.to_string()],
        nick: bot_name.to_string(),
        auth: AuthIdentity::DidKey(ident),
        stt: Arc::new(SttEngine::noop()),
        window_secs: 10.0,
        summary_model: "claude-sonnet-4-5".to_string(),
        anthropic_key: None,
        summary_enabled: false,
        start_session_in: None,
        sfu_url_override: None,
        groq_api_key: None,
        groq_chat_model: "llama-3.3-70b-versatile".to_string(),
        answer_provider: "groq".to_string(),
        groq_answer_model: "groq/compound".to_string(),
        inception_api_key: None,
        inception_reasoning_effort: "instant".to_string(),
        claude_agent: Some(claude_agent),
        alexandria_wake_command: None,
        vision_model: "meta-llama/llama-4-scout-17b-16e-instruct".to_string(),
        elevenlabs_api_key: None,
        elevenlabs_voice_id: "aj0fZfXTBc7E3By4X8L2".to_string(),
        elevenlabs_model: "eleven_turbo_v2_5".to_string(),
        image_ai: None,
        proactive_enabled: false,
        ambient_enabled: false,
        render_backend: "svg".to_string(),
        ghostly_character: "raven".to_string(),
        character_system_prompt: None,
        peer_agents: Vec::new(),
    };
    (tokio::spawn(run(cfg)), tmp)
}

fn live_suffix() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_millis();
    let suffix = millis % 1_000_000;
    format!("{}{}", std::process::id(), suffix)
}

#[tokio::test]
#[ignore = "connects to the public irc.freeq.at service"]
async fn live_irc_freeq_at_addressed_chat_uses_claude_agent_session() -> Result<()> {
    let _api_key = std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .expect("ANTHROPIC_API_KEY is required for the live Claude Agent SDK test");
    let suffix = live_suffix();
    let requested_bot_nick = format!("ravenlive{suffix}");
    let witness_nick = format!("ravenwit{suffix}");
    let marker = format!("Night Library {suffix}");

    let mut witness = Witness::join(LIVE_SERVER, &witness_nick, LIVE_CHANNEL).await?;
    let (bot, _bot_tmp) = spawn_live_bot(
        LIVE_SERVER,
        LIVE_CHANNEL,
        &requested_bot_nick,
        claude_agent_sidecar_config(),
    );

    let actual_bot_nick = witness
        .wait_for(SETTLE, |ev| match ev {
            Event::Joined { nick, channel, .. }
                if channel.eq_ignore_ascii_case(LIVE_CHANNEL)
                    && nick.eq_ignore_ascii_case(&requested_bot_nick) =>
            {
                Some(nick.clone())
            }
            _ => None,
        })
        .await
        .expect("live witness never saw Raven join #alexandria-test");

    println!(
        "live smoke connected to {LIVE_SERVER} {LIVE_CHANNEL} as bot={actual_bot_nick} witness={witness_nick}"
    );

    // Raven ignores addressed messages during startup history replay.
    tokio::time::sleep(Duration::from_secs(16)).await;

    let first_turn = format!(
        "{actual_bot_nick}, remember this session marker: {marker}. Reply exactly: remembered {marker}"
    );
    println!("sending first live turn: {first_turn}");
    witness.handle.privmsg(LIVE_CHANNEL, &first_turn).await?;

    let first_reply = witness
        .wait_for(SETTLE, |ev| match ev {
            Event::Message {
                from, target, text, ..
            } if from.eq_ignore_ascii_case(&actual_bot_nick)
                && target.eq_ignore_ascii_case(LIVE_CHANNEL)
                && text.contains(&marker) =>
            {
                Some(text.clone())
            }
            _ => None,
        })
        .await
        .expect("Raven did not answer first live addressed turn");
    println!("first live reply: {first_reply}");
    assert!(first_reply.contains(&marker));

    let second_turn =
        format!("{actual_bot_nick}, what is the session marker? Reply with only the marker.");
    println!("sending second live turn: {second_turn}");
    witness.handle.privmsg(LIVE_CHANNEL, &second_turn).await?;

    let second_reply = witness
        .wait_for(SETTLE, |ev| match ev {
            Event::Message {
                from, target, text, ..
            } if from.eq_ignore_ascii_case(&actual_bot_nick)
                && target.eq_ignore_ascii_case(LIVE_CHANNEL)
                && text.contains(&marker) =>
            {
                Some(text.clone())
            }
            _ => None,
        })
        .await
        .expect("Raven did not answer second live addressed turn");
    println!("second live reply: {second_reply}");
    assert!(
        second_reply.contains(&marker),
        "second live reply did not preserve sidecar session memory: {second_reply}"
    );

    bot.abort();
    Ok(())
}
