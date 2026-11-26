//! Kiroscribe のコアロジック。Tauri に依存しないため単体でテストできる。

pub mod audio;
pub mod autostart;
pub mod chat;
pub mod claude;
pub mod db;
#[cfg(feature = "e2e")]
pub mod e2e;
pub mod error;
pub mod export;
pub mod help;
pub mod http;
pub mod legacy;
pub mod meeting_record;
pub mod meetings;
pub mod notes;
pub mod prompts;
pub mod recorder;
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
