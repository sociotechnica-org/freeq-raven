//! Library facade for `freeq-raven`.
//!
//! The binary at `src/main.rs` is the real entrypoint; we expose the
//! modules through `lib.rs` so adversarial unit tests can `cargo test
//! --lib` against them without dragging in `tokio::main`.

pub mod ambient;
pub mod character_profile;
pub mod claude_agent;
pub mod decisions;
pub mod diagram;
pub mod identity;
pub mod imagegen;
pub mod irc;
pub mod memory;
pub mod proactive;
pub mod qa;
pub mod social;
pub mod stt;
pub mod summary;
pub mod tts;
pub mod video;
pub mod video_particles;
pub mod vision;
pub mod whiteboard;
