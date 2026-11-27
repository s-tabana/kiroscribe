//! デバッグ用のログの書き出し: ログのファイル・会議の検出の記録・環境の情報を、1 つの zip にまとめる。
//!
//! 設定画面の「デバッグ用のログを保存…」から呼ぶ。後で「提出」から送るときも、[`build_bundle`] の結果をそのまま使う。
//!
//! zip の中身:
//! - `README.txt` … 中身の説明
//! - `environment.json` … アプリと OS のバージョン、秘密でない設定（[`Environment`]）
//! - `meeting-detection.json` … 会議の検出の記録（[`crate::meeting_record::MeetingRecord`]）
//! - `logs/kiroscribe-<年月日>.log` … デバッグ用のログ（[`crate::debug_log`]）

use std::io::{Cursor, Write};
use std::path::Path;

use serde::Serialize;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::meeting_record::MeetingRecord;
use crate::secrets::ApiKeyStatus;
use crate::settings::Settings;

/// zip の中の名前。
pub const README: &str = "README.txt";
pub const ENVIRONMENT: &str = "environment.json";
pub const MEETING_RECORD: &str = "meeting-detection.json";
pub const LOG_DIR: &str = "logs";

/// 書き出す設定（秘密でないものだけ）。API キーは、設定済みかどうかだけ。
/// 自分の呼び名とマイクの ID（デバイスの名前を含む）は入れず、決めてあるかどうかだけを入れる。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SafeSettings {
    pub language: String,
    pub transcription_model: String,
    pub claude_model: String,
    /// マイクを選んであるか（`false` なら既定のマイク）
    pub input_device_selected: bool,
    pub capture_system_audio: bool,
    /// 議事録での自分の呼び名を決めてあるか
    pub my_name_set: bool,
    pub auto_record_meetings: bool,
    pub launch_at_login: bool,
    pub auto_update: bool,
    pub openai_key_configured: bool,
    pub anthropic_key_configured: bool,
}

impl SafeSettings {
    pub fn new(settings: &Settings, keys: Option<&ApiKeyStatus>) -> Self {
        Self {
            language: settings.language.clone(),
            transcription_model: settings.transcription_model.clone(),
            claude_model: settings.claude_model.clone(),
            input_device_selected: settings.input_device_id.is_some(),
            capture_system_audio: settings.capture_system_audio,
            my_name_set: !settings.my_name.is_empty(),
            auto_record_meetings: settings.auto_record_meetings,
            launch_at_login: settings.launch_at_login,
            auto_update: settings.auto_update,
            openai_key_configured: keys.is_some_and(|k| k.openai),
            anthropic_key_configured: keys.is_some_and(|k| k.anthropic),
        }
    }
}

/// 環境の情報（`environment.json`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    pub app_version: String,
    /// OS の名前とバージョン（[`os_description`]）
    pub os: String,
    /// Windows のビルド番号（[`windows_build`]。Windows 以外では `None`）
    pub os_build: Option<String>,
    /// CPU の種類（x86_64 など）
    pub arch: String,
    /// 書き出した時刻（RFC 3339）
    pub created_at: String,
    /// 「詳しい記録」の期限（RFC 3339）。オフなら `None`
    pub verbose_until: Option<String>,
    /// 設定（読めなかったら `None`）
    pub settings: Option<SafeSettings>,
}

/// まとめる材料。
pub struct BundleInput<'a> {
    /// ログのファイル（[`crate::debug_log::DebugLog::files`]）
    pub log_files: &'a [std::path::PathBuf],
    pub environment: &'a Environment,
    pub meeting_record: &'a MeetingRecord,
}

fn readme(environment: &Environment) -> String {
    format!(
        "Kiroscribe のデバッグ用のログ（{created}、Kiroscribe {version}）\n\
         \n\
         不具合の原因を調べるための記録です。中身は次のとおりです。\n\
         \n\
         - {ENVIRONMENT}: アプリと OS のバージョン、設定（API キーは設定済みかどうかだけ）\n\
         - {MEETING_RECORD}: 会議の検出の記録（マイクを使ったアプリの名前と、マイクを使っているブラウザのタブの名前）\n\
         - {LOG_DIR}/: アプリの動き（起動と終了、更新の確認、会議の検出の判断、録音、文字起こしと AI の処理の成否と時間、エラー）\n\
         \n\
         API キー、録音した音声、文字起こし・メモ・議事録・AI チャットの本文は入っていません。\n\
         タブの名前には、会議でないページやメールの件名も入ることがあります。\n\
         人に渡す前に、見せたくない内容がないか確かめてください。\n",
        created = environment.created_at,
        version = environment.app_version,
    )
}

/// zip の中身を作る（ファイルには書かない）。zip に入れた名前の一覧も返す。
pub fn build_bundle(input: &BundleInput<'_>) -> std::io::Result<(Vec<u8>, Vec<String>)> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut names = Vec::new();
    let mut add = |zip: &mut ZipWriter<Cursor<Vec<u8>>>, name: String, bytes: &[u8]| -> std::io::Result<()> {
        zip.start_file(name.as_str(), options).map_err(std::io::Error::other)?;
        zip.write_all(bytes)?;
        names.push(name);
        Ok(())
    };
    add(&mut zip, README.into(), readme(input.environment).as_bytes())?;
    add(&mut zip, ENVIRONMENT.into(), &serde_json::to_vec_pretty(input.environment)?)?;
    add(&mut zip, MEETING_RECORD.into(), &serde_json::to_vec_pretty(input.meeting_record)?)?;
    for path in input.log_files {
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        // 書いている途中のファイルでも、読めた分だけ入れる（読めなければ飛ばす）
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        add(&mut zip, format!("{LOG_DIR}/{name}"), &bytes)?;
    }
    let cursor = zip.finish().map_err(std::io::Error::other)?;
    Ok((cursor.into_inner(), names))
}

/// zip を `out` に保存する。zip に入れた名前の一覧を返す。
pub fn write_bundle(out: &Path, input: &BundleInput<'_>) -> std::io::Result<Vec<String>> {
    let (bytes, names) = build_bundle(input)?;
    std::fs::write(out, bytes)?;
    Ok(names)
}

/// OS の名前とバージョン（例: 「Windows 11 Pro 24H2 (build 26100.2314)」「Ubuntu 24.04.1 LTS」）。
pub fn os_description() -> String {
    #[cfg(windows)]
    {
        windows_version().unwrap_or_else(|| "Windows".to_string())
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|line| line.strip_prefix("PRETTY_NAME="))
                    .map(|name| name.trim_matches('"').to_string())
            })
            .unwrap_or_else(|| "Linux".to_string())
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        std::env::consts::OS.to_string()
    }
}

/// Windows のビルド番号（例: 「26100.2314」）。Windows 以外と、ビルド番号を読めないときは `None`。
pub fn windows_build() -> Option<String> {
    #[cfg(windows)]
    {
        let key = windows_current_version()?;
        let build: String = key.get_value("CurrentBuild").ok()?;
        let ubr: u32 = key.get_value("UBR").unwrap_or(0);
        build_number(&build, ubr)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// ビルド番号の形にする。ビルド番号が空なら `None`（「.0」のような値を書かない）。
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn build_number(build: &str, ubr: u32) -> Option<String> {
    let build = build.trim();
    (!build.is_empty()).then(|| format!("{build}.{ubr}"))
}

#[cfg(windows)]
fn windows_current_version() -> Option<winreg::RegKey> {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    winreg::RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion")
        .ok()
}

#[cfg(windows)]
fn windows_version() -> Option<String> {
    let key = windows_current_version()?;
    let product: String = key.get_value("ProductName").unwrap_or_else(|_| "Windows".into());
    let display: String = key.get_value("DisplayVersion").unwrap_or_default();
    let build: String = key.get_value("CurrentBuild").unwrap_or_default();
    let ubr: u32 = key.get_value("UBR").unwrap_or(0);
    // Windows 11 でも、ProductName は「Windows 10 …」のままのことがある（ビルド番号 22000 以上が 11）
    let product = if build.parse::<u32>().unwrap_or(0) >= 22_000 {
        product.replacen("Windows 10", "Windows 11", 1)
    } else {
        product
    };
    let display = if display.is_empty() { String::new() } else { format!(" {display}") };
    Some(format!("{product}{display} (build {build}.{ubr})"))
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    /// Windows のビルド番号を読めないとき（空のとき）は、空の文字列でなく「なし」にする。
    // [LOG-003]
    #[test]
    fn an_empty_windows_build_is_none() {
        assert_eq!(build_number("26100", 2314), Some("26100.2314".into()));
        assert_eq!(build_number("", 0), None);
        assert_eq!(build_number("  ", 5), None);
    }
    use crate::meeting_record::MeetingRecorder;

    fn settings() -> Settings {
        Settings {
            my_name: "山田".into(),
            input_device_id: Some("{0.0.1.00000000}.{マイク (Realtek)}".into()),
            ..Settings::default()
        }
    }

    /// zip には、説明・環境の情報・会議の検出の記録・ログのファイルを入れる。
    /// 設定は秘密でないものだけで、API キーは設定済みかどうか、呼び名とマイクは決めてあるかどうかだけ。
    // [LOG-011][MTG-026]
    #[test]
    fn the_bundle_holds_logs_record_and_environment() {
        let dir = tempfile::tempdir().unwrap();
        let log_a = dir.path().join("kiroscribe-20261126.log");
        let log_b = dir.path().join("kiroscribe-20261127.log");
        std::fs::write(&log_a, "{\"event\":\"app.started\"}\n").unwrap();
        std::fs::write(&log_b, "{\"event\":\"app.exit\"}\n").unwrap();
        let keys = ApiKeyStatus {
            openai: true,
            anthropic: false,
            storage: String::new(),
        };
        let environment = Environment {
            app_version: "0.4.1".into(),
            os: os_description(),
            os_build: windows_build(),
            arch: std::env::consts::ARCH.into(),
            created_at: "2026-11-27T10:00:00+09:00".into(),
            verbose_until: None,
            settings: Some(SafeSettings::new(&settings(), Some(&keys))),
        };
        let record = MeetingRecorder::default().export("0.4.1", None);
        let out = dir.path().join("debug.zip");
        let names = write_bundle(
            &out,
            &BundleInput {
                log_files: &[log_a, log_b, dir.path().join("消えたファイル.log")],
                environment: &environment,
                meeting_record: &record,
            },
        )
        .unwrap();
        assert_eq!(
            names,
            vec![
                "README.txt",
                "environment.json",
                "meeting-detection.json",
                "logs/kiroscribe-20261126.log",
                "logs/kiroscribe-20261127.log"
            ]
        );

        let mut zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        let mut read = |name: &str| {
            let mut text = String::new();
            zip.by_name(name).unwrap().read_to_string(&mut text).unwrap();
            text
        };
        let env: serde_json::Value = serde_json::from_str(&read(ENVIRONMENT)).unwrap();
        assert_eq!(env["appVersion"], "0.4.1");
        assert_eq!(env["settings"]["openaiKeyConfigured"], true);
        assert_eq!(env["settings"]["anthropicKeyConfigured"], false);
        assert_eq!(env["settings"]["myNameSet"], true);
        assert_eq!(env["settings"]["inputDeviceSelected"], true);
        let env_text = env.to_string();
        assert!(!env_text.contains("山田") && !env_text.contains("Realtek"), "{env_text}");
        let record: MeetingRecord = serde_json::from_str(&read(MEETING_RECORD)).unwrap();
        assert_eq!(record.format, crate::meeting_record::FORMAT, "回帰テストの材料と同じ形");
        assert_eq!(read("logs/kiroscribe-20261127.log"), "{\"event\":\"app.exit\"}\n");
        let readme = read(README);
        assert!(readme.contains("API キー") && readme.contains("確かめてください"), "{readme}");
    }

    #[test]
    fn os_description_is_not_empty() {
        assert!(!os_description().trim().is_empty());
        // Windows ではビルド番号（「26100.2314」の形）、それ以外ではなし
        match windows_build() {
            Some(build) => assert!(build.split('.').all(|part| part.parse::<u32>().is_ok()), "{build}"),
            None => assert!(!cfg!(windows)),
        }
    }
}
