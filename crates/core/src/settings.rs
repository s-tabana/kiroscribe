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
    /// 会議の検出の対象に足すブラウザ（実行ファイルの名前、小文字）。組み込みの一覧（MTG-002）と同じに扱う。
    pub extra_browsers: Vec<String>,
    /// Windows の起動時に起動し、タスクトレイに常駐する。
    pub launch_at_login: bool,
    /// 見つけた新しいバージョンを自動でインストールする（既定はオフで、知らせるだけ）。
    pub auto_update: bool,
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
            extra_browsers: Vec::new(),
            launch_at_login: true,
            auto_update: false,
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
        self.extra_browsers = normalize_browsers(&self.extra_browsers);
        self
    }
}

/// 判定に足すブラウザの数と、名前の長さの上限。
pub const MAX_EXTRA_BROWSERS: usize = 20;
pub const MAX_BROWSER_NAME_CHARS: usize = 64;

/// 判定に足すブラウザの名前を整える（SET-021）: 前後の空白とフォルダの部分を除き、小文字にし、
/// 末尾が .exe でなければ .exe を付ける。空の名前・長すぎる名前・ブラウザとして扱わないプロセス
/// （Kiroscribe 自身と MTG-003 で対象にしないもの。[`crate::meetings::NOT_BROWSERS`]）・重複は除き、20 個までにする。
pub fn normalize_browsers(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        let name = name.trim().trim_matches('"');
        let name = name.rsplit(['\\', '/']).next().unwrap_or(name).trim().to_lowercase();
        if name.is_empty() || name == ".exe" {
            continue;
        }
        let name = if name.ends_with(".exe") { name } else { format!("{name}.exe") };
        if name.chars().count() > MAX_BROWSER_NAME_CHARS || crate::meetings::NOT_BROWSERS.contains(&name.as_str()) || out.contains(&name) {
            continue;
        }
        out.push(name);
        if out.len() == MAX_EXTRA_BROWSERS {
            break;
        }
    }
    out
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
        assert!(loaded.extra_browsers.is_empty(), "判定に足すブラウザはなし");
    }

    /// 判定に足すブラウザは、実行ファイルの名前（小文字、.exe 付き）にそろえて保存する。
    // [SET-021]
    #[test]
    fn extra_browsers_are_saved_as_exe_names() {
        let db = Database::open_in_memory().unwrap();
        let names = [
            " MyBrowser.EXE ",
            r#""C:\Program Files\Sample\sample.exe""#,
            "other",
            "mybrowser.exe",
            "",
            "Kiroscribe.exe",
            "D:/Tools/third.exe",
            r"C:\Program Files (x86)\Microsoft\EdgeWebView\Application\msedgewebview2.exe",
            "Update.exe",
            "ms-teamsupdate",
            "msteams_autostarter.exe",
            "ZCSCPTHOST.EXE",
        ];
        let saved = db
            .save_settings(Settings { extra_browsers: names.iter().map(|n| n.to_string()).collect(), ..Default::default() })
            .unwrap();
        assert_eq!(saved.extra_browsers, ["mybrowser.exe", "sample.exe", "other.exe", "third.exe"]);
        assert_eq!(db.load_settings().unwrap().extra_browsers, saved.extra_browsers);

        let many: Vec<String> = (0..30).map(|i| format!("b{i}.exe")).collect();
        assert_eq!(normalize_browsers(&many).len(), MAX_EXTRA_BROWSERS);
        assert!(normalize_browsers(&["a".repeat(80)]).is_empty(), "長すぎる名前は除く");
    }

    /// 送り先は設定に持たない。設定に送り先を書いても使わず、ほかの値はそのまま読める
    // [SET-004][SET-019][SEC-015]
    #[test]
    fn log_intake_is_not_a_setting() {
        let json = serde_json::to_value(Settings::default()).unwrap();
        assert!(json.get("logIntakeUrl").is_none() && json.get("logIntakeToken").is_none(), "{json}");

        let db = Database::open_in_memory().unwrap();
        db.set_value(
            SETTINGS_KEY,
            r#"{"myName":"田花","autoUpdate":true,"logIntakeUrl":"https://evil.example","logIntakeToken":"x"}"#,
        )
        .unwrap();
        let loaded = db.load_settings().unwrap();
        assert_eq!(loaded.my_name, "田花");
        assert!(loaded.auto_update);
        let saved = db.save_settings(loaded).unwrap();
        let stored = db.get_value(SETTINGS_KEY).unwrap().unwrap();
        assert!(!stored.contains("logIntake"), "{stored}");
        assert_eq!(db.load_settings().unwrap(), saved);
        // 送り先は本体に埋め込んだもの。E2E テスト用のビルドでは差し替わるため、埋め込んだ値から作って確かめる
        #[cfg(feature = "e2e")]
        let intake = crate::log_submit::Intake::production(
            crate::log_submit::LOG_INTAKE_URL,
            crate::log_submit::LOG_INTAKE_TOKEN,
        )
        .unwrap();
        #[cfg(not(feature = "e2e"))]
        let intake = crate::log_submit::intake().unwrap();
        assert_eq!(intake.url(), "https://log-intake.seita-tabana.workers.dev/v1/logs/kiroscribe");
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
