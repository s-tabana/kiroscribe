//! アプリ設定（API キー以外）。SQLite の settings テーブルに JSON で保存する。

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::error::Result;

pub const DEFAULT_TRANSCRIPTION_MODEL: &str = "gpt-4o-transcribe";
pub const DEFAULT_CLAUDE_MODEL: &str = "claude-opus-5-5";
pub const DEFAULT_LANGUAGE: &str = "ja";

const SETTINGS_KEY: &str = "app";

/// ユーザ設定。項目を増やしても古い保存データを読めるよう、欠けた項目は既定値で補う。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// 文字起こしの言語（ISO-639-1）。
    pub language: String,
    /// OpenAI の文字起こしモデル。
    pub transcription_model: String,
    /// 議事録生成・チャットに使う Claude のモデル。
    pub claude_model: String,
    /// 録音に使うマイクのデバイス ID。`None` なら OS の既定デバイス。
    pub input_device_id: Option<String>,
    /// システム音声（相手の声）も取り込むか。
    pub capture_system_audio: bool,
    /// 議事録の中で自分を指す名前（空なら「自分」）。
    pub my_name: String,
    /// 会議（Google Meet・Slack ハドルなど）を検出したら自動で録音する。
    pub auto_record_meetings: bool,
    /// Windows の起動時に起動し、タスクトレイに常駐する。
    pub launch_at_login: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: DEFAULT_LANGUAGE.to_string(),
            transcription_model: DEFAULT_TRANSCRIPTION_MODEL.to_string(),
            claude_model: DEFAULT_CLAUDE_MODEL.to_string(),
            input_device_id: None,
            capture_system_audio: true,
            my_name: String::new(),
            auto_record_meetings: true,
            launch_at_login: true,
        }
    }
}

impl Settings {
    /// 前後の空白を除き、空の必須項目を既定値に戻す。
    pub fn normalized(mut self) -> Self {
        let defaults = Settings::default();
        let fix = |value: &mut String, default: String| {
            let trimmed = value.trim().to_string();
            *value = if trimmed.is_empty() { default } else { trimmed };
        };
        fix(&mut self.language, defaults.language);
        fix(&mut self.transcription_model, defaults.transcription_model);
        fix(&mut self.claude_model, defaults.claude_model);
        self.my_name = self.my_name.trim().to_string();
        self.input_device_id = self
            .input_device_id
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty());
        self
    }
}

impl Database {
    pub fn load_settings(&self) -> Result<Settings> {
        let raw: Option<String> = self
            .conn()
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                [SETTINGS_KEY],
                |row| row.get(0),
            )
            .optional()?;
        Ok(match raw {
            Some(json) => serde_json::from_str::<Settings>(&json)?.normalized(),
            None => Settings::default(),
        })
    }

    pub fn save_settings(&self, settings: Settings) -> Result<Settings> {
        let settings = settings.normalized();
        let json = serde_json::to_string(&settings)?;
        self.conn().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SETTINGS_KEY, json],
        )?;
        Ok(settings)
    }

    /// settings テーブルの任意のキーの値（ヘルプのキャッシュなど、設定以外の小さなデータ用）。
    pub fn get_value(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn()
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| row.get(0))
            .optional()?)
    }

    pub fn set_value(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn delete_value(&self, key: &str) -> Result<()> {
        self.conn().execute("DELETE FROM settings WHERE key = ?1", [key])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_nothing_saved() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.load_settings().unwrap(), Settings::default());
    }

    #[test]
    fn save_and_load_roundtrip() {
        let db = Database::open_in_memory().unwrap();
        let settings = Settings {
            claude_model: "claude-sonnet-5-5".into(),
            input_device_id: Some("mic-1".into()),
            capture_system_audio: false,
            my_name: " 田花 ".into(),
            ..Default::default()
        };
        let saved = db.save_settings(settings).unwrap();
        assert_eq!(saved.my_name, "田花");
        assert_eq!(db.load_settings().unwrap(), saved);
    }

    #[test]
    fn blank_values_fall_back_to_defaults() {
        let db = Database::open_in_memory().unwrap();
        let saved = db
            .save_settings(Settings {
                transcription_model: "  ".into(),
                input_device_id: Some("".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(saved.transcription_model, DEFAULT_TRANSCRIPTION_MODEL);
        assert_eq!(saved.input_device_id, None);
    }

    #[test]
    fn missing_fields_in_stored_json_use_defaults() {
        let db = Database::open_in_memory().unwrap();
        db.conn()
            .execute(
                "INSERT INTO settings (key, value) VALUES ('app', '{\"language\":\"en\"}')",
                [],
            )
            .unwrap();
        let loaded = db.load_settings().unwrap();
        assert_eq!(loaded.language, "en");
        assert_eq!(loaded.claude_model, DEFAULT_CLAUDE_MODEL);
        assert!(loaded.capture_system_audio);
        // 後から追加した項目も既定値（オン）になる
        assert!(loaded.auto_record_meetings);
        assert!(loaded.launch_at_login);
    }

    #[test]
    fn extra_values_live_beside_settings() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.get_value("help.guide").unwrap(), None);
        db.set_value("help.guide", "v1").unwrap();
        db.set_value("help.guide", "v2").unwrap();
        assert_eq!(db.get_value("help.guide").unwrap().as_deref(), Some("v2"));
        // 設定そのものには影響しない
        assert_eq!(db.load_settings().unwrap(), Settings::default());
        db.delete_value("help.guide").unwrap();
        assert_eq!(db.get_value("help.guide").unwrap(), None);
    }
}
