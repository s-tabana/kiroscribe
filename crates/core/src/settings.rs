//! アプリ設定（API キー以外）。SQLite の settings テーブルに JSON で保存する。

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::error::Result;
use crate::log_submit::{Intake, DEFAULT_LOG_INTAKE_TOKEN, DEFAULT_LOG_INTAKE_URL};

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
    /// 見つけた新しいバージョンを自動でインストールする（既定はオフで、知らせるだけ）。
    pub auto_update: bool,
    /// デバッグ用のログの回収の入口（`https://<ホスト>`）。`None` なら既定（[`DEFAULT_LOG_INTAKE_URL`]）。画面には出さない
    pub log_intake_url: Option<String>,
    /// 入口に添える提出用の値（`X-Log-Token`）。`None` なら既定（[`DEFAULT_LOG_INTAKE_TOKEN`]）。画面には出さない
    pub log_intake_token: Option<String>,
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
            auto_update: false,
            log_intake_url: None,
            log_intake_token: None,
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
        let optional = |value: Option<String>| value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        self.log_intake_url = optional(self.log_intake_url);
        self.log_intake_token = optional(self.log_intake_token);
        self
    }

    /// デバッグ用のログの送り先。入口と提出用の値のどちらかが、設定にも既定にもなければ `None`
    pub fn log_intake(&self) -> Option<Intake> {
        self.log_intake_or(DEFAULT_LOG_INTAKE_URL, DEFAULT_LOG_INTAKE_TOKEN)
    }

    /// [`Self::log_intake`] の既定の値を差し替えたもの
    pub fn log_intake_or(&self, default_url: Option<&str>, default_token: Option<&str>) -> Option<Intake> {
        let base = self.log_intake_url.as_deref().or(default_url)?;
        let token = self.log_intake_token.as_deref().or(default_token)?;
        Some(Intake::new(base, token))
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

    // [SET-001]
    #[test]
    fn defaults_when_nothing_saved() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(db.load_settings().unwrap(), Settings::default());
    }

    // [UPD-019][IPC-008]
    #[test]
    fn auto_update_is_off_by_default_and_can_be_turned_on() {
        assert!(!Settings::default().auto_update);

        let db = Database::open_in_memory().unwrap();
        assert!(!db.load_settings().unwrap().auto_update, "何も保存していないとき");
        let saved = db
            .save_settings(Settings {
                auto_update: true,
                ..Default::default()
            })
            .unwrap();
        assert!(saved.auto_update);
        assert!(db.load_settings().unwrap().auto_update);
        // 画面とは camelCase でやり取りする
        assert_eq!(serde_json::to_value(&saved).unwrap()["autoUpdate"], true);
        let from_frontend: Settings = serde_json::from_str(r#"{"autoUpdate":true}"#).unwrap();
        assert!(from_frontend.auto_update);

        // オフに戻せる
        let saved = db.save_settings(Settings { auto_update: false, ..saved }).unwrap();
        assert!(!saved.auto_update);
        assert!(!db.load_settings().unwrap().auto_update);
    }

    // [SET-003][SET-004]
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

    // [SET-003]
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

    // [SET-004][UPD-019]
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
        // 後から追加した項目も既定値になる（自動検出と自動起動はオン、自動更新はオフ）
        assert!(loaded.auto_record_meetings);
        assert!(loaded.launch_at_login);
        assert!(!loaded.auto_update);
    }

    // [SET-001][SET-019]
    #[test]
    fn log_intake_comes_from_settings_or_defaults() {
        let db = Database::open_in_memory().unwrap();
        let loaded = db.load_settings().unwrap();
        assert_eq!((loaded.log_intake_url.as_deref(), loaded.log_intake_token.as_deref()), (None, None));
        // 未設定なら既定の値を使う
        assert_eq!(
            loaded.log_intake(),
            Some(Intake::new(DEFAULT_LOG_INTAKE_URL.unwrap(), DEFAULT_LOG_INTAKE_TOKEN.unwrap()))
        );
        assert!(loaded.log_intake().unwrap().url.starts_with("https://"));
        // 画面とは camelCase でやり取りし、未設定は null
        let json = serde_json::to_value(&loaded).unwrap();
        assert_eq!(json["logIntakeUrl"], serde_json::Value::Null);
        assert_eq!(json["logIntakeToken"], serde_json::Value::Null);

        let saved = db
            .save_settings(Settings {
                log_intake_url: Some(" http://127.0.0.1:4010/ ".into()),
                log_intake_token: Some(" token ".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(saved.log_intake(), Some(Intake::new("http://127.0.0.1:4010", "token")), "設定の値を使う");
        assert_eq!(db.load_settings().unwrap(), saved);

        // 空なら未設定に戻す
        let blank = db
            .save_settings(Settings {
                log_intake_url: Some("  ".into()),
                log_intake_token: Some("".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!((blank.log_intake_url, blank.log_intake_token), (None, None));

        // 既定がないときは、片方だけでは送らない
        let url_only = Settings { log_intake_url: Some("https://logs.example.com".into()), ..Default::default() };
        assert_eq!(url_only.log_intake_or(None, None), None);
        let token_only = Settings { log_intake_token: Some("t".into()), ..Default::default() };
        assert_eq!(token_only.log_intake_or(None, None), None);
        assert_eq!(
            token_only.log_intake_or(Some("https://logs.example.com"), None),
            Some(Intake::new("https://logs.example.com", "t"))
        );
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
