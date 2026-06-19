//! Shared helpers for the freeq-raven integration tests.
//!
//! Included via `mod common;` from each integration test file. Not every
//! helper is used by every test binary, so dead-code is allowed here.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use freeq_raven::claude_agent::ClaudeAgentConfig;
use freeq_raven::identity::{self, Identity};

/// Mint a throwaway did:key identity in a private tempdir so tests never
/// touch `$HOME/.freeq`. Returns the identity plus the tempdir guard
/// (kept alive by the caller for the test's lifetime).
pub fn mint_identity(name: &str) -> (Identity, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ident = identity::load_or_create_in(name, tmp.path()).expect("mint identity");
    (ident, tmp)
}

/// The repo root, two levels above this crate's `CARGO_MANIFEST_DIR`.
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crate lives under repo/crates/freeq-raven")
        .to_path_buf()
}

/// Single-quote a string for safe embedding in a `/bin/sh -c` command.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// A `ClaudeAgentConfig` that runs the sidecar in deterministic mock mode,
/// persisting its session map to `state_path` so per-channel session
/// continuity can be asserted across turns.
pub fn mock_claude_agent_config(state_path: &Path) -> ClaudeAgentConfig {
    let root = repo_root();
    ClaudeAgentConfig {
        command: format!(
            "RAVEN_CLAUDE_AGENT_MOCK=1 RAVEN_CLAUDE_AGENT_MOCK_STATE={} node {}",
            shell_quote(&state_path.display().to_string()),
            shell_quote(
                &root
                    .join("scripts/claude-agent-sidecar.mjs")
                    .display()
                    .to_string()
            )
        ),
        workdir: Some(root.clone()),
        alexandria_plugin_path: Some(root.join(".claude/plugins/alexandria")),
        model: None,
        permission_mode: "dontAsk".to_string(),
        max_turns: 4,
        timeout: Duration::from_secs(30),
    }
}
