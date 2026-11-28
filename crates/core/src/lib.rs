//! Kiroscribe のコアロジック。Tauri に依存しないため単体でテストできる。

pub mod audio;
pub mod audio_sessions;
pub mod autostart;
pub mod chat;
pub mod claude;
pub mod db;
pub mod debug_bundle;
pub mod debug_log;
#[cfg(feature = "e2e")]
pub mod e2e;
pub mod error;
pub mod export;
pub mod help;
pub mod http;
pub mod legacy;
pub mod log_submit;
pub mod meeting_facts;
pub mod meeting_judge;
pub mod meeting_record;
pub mod meeting_record_v1;
pub mod meeting_rules;
pub mod meetings;
pub mod notes;
pub mod prompts;
pub mod recorder;
pub mod recording_check;
pub mod retry;
pub mod secrets;
pub mod session;
pub mod settings;
pub mod sse;
pub mod time;
pub mod transcribe;
pub mod transcript;

pub use db::Database;
pub use error::{CoreError, Result};
