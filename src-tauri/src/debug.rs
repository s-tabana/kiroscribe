//! デバッグ用のログ（`kiroscribe_core::debug_log`）を、アプリの各所から書く入口と、画面から呼ぶコマンド。
//!
//! ログは、アプリのデータの場所の `logs/` に書く。書き出し（「デバッグ用のログを保存…」）は
//! [`build_bundle`] でまとめる（後で「提出」から送るときも、同じ関数を使う）。

use std::path::Path;

use chrono::{DateTime, Local};
use kiroscribe_core::debug_bundle::{self, BundleInput, Environment, SafeSettings};
use kiroscribe_core::debug_log::{self, DebugLog, Level, Source, VERBOSE_MS};
use kiroscribe_core::secrets::api_key_status;
use kiroscribe_core::time::now_ms;
use kiroscribe_core::Database;
use serde::Serialize;
use serde_json::Value;
use tauri::{Manager, State};

use crate::error::{CmdResult, CommandError};
use crate::runtime::AppHandle;
use crate::state::AppState;

/// 「詳しい記録」の期限（UNIX ミリ秒）を保存するキー（settings テーブル）。
const VERBOSE_KEY: &str = "debugVerboseUntil";

/// アプリのデータの場所の `logs/` を開く。開けなくても、アプリは動かす（ログを書かないだけ）。
pub fn open(data_dir: &Path) -> Option<DebugLog> {
    match DebugLog::open(data_dir.join(debug_log::DIR_NAME)) {
        Ok(log) => Some(log),
        Err(err) => {
            eprintln!("デバッグ用のログのフォルダを作れませんでした: {err}");
            None
        }
    }
}

/// 保存してある「詳しい記録」の期限を読み込む（期限が過ぎていれば、オフのまま）。
pub fn restore_verbose(log: &DebugLog, db: &Database, now_ms: i64) {
    let until = db
        .get_value(VERBOSE_KEY)
        .ok()
        .flatten()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|until| *until > now_ms);
    log.set_verbose_until(until);
}

/// 出来事を書く（ログを用意していなければ、何もしない）。
pub fn write(app: &AppHandle, level: Level, event: &str, fields: Value) {
    if let Some(log) = app.try_state::<DebugLog>() {
        log.write(level, Source::App, event, fields);
    }
}

pub fn info(app: &AppHandle, event: &str, fields: Value) {
    write(app, Level::Info, event, fields);
}

pub fn error(app: &AppHandle, event: &str, fields: Value) {
    write(app, Level::Error, event, fields);
}

/// 「詳しい記録」がオンのときだけ書く出来事。
pub fn detail(app: &AppHandle, event: &str, fields: Value) {
    write(app, Level::Detail, event, fields);
}

/// 「詳しい記録」がオンか。
pub fn verbose(app: &AppHandle) -> bool {
    app.try_state::<DebugLog>()
        .is_some_and(|log| log.verbose_until(now_ms()).is_some())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugLogStatus {
    /// 「詳しい記録」の期限（UNIX ミリ秒）。オフなら `null`
    pub verbose_until: Option<i64>,
}

fn status(app: &AppHandle) -> DebugLogStatus {
    DebugLogStatus {
        verbose_until: app.try_state::<DebugLog>().and_then(|log| log.verbose_until(now_ms())),
    }
}

#[tauri::command]
pub fn get_debug_log_status(app: AppHandle) -> DebugLogStatus {
    status(&app)
}

/// 「詳しい記録」を切り替える。オンにすると、いまから 24 時間だけ細かい出来事も書く。
#[tauri::command]
pub fn set_debug_verbose(app: AppHandle, state: State<'_, AppState>, enabled: bool) -> CmdResult<DebugLogStatus> {
    let log = app
        .try_state::<DebugLog>()
        .ok_or_else(|| CommandError("デバッグ用のログを書けない状態です".into()))?;
    let now = now_ms();
    let until = enabled.then_some(now + VERBOSE_MS);
    {
        let db = state.db();
        match until {
            Some(until) => db.set_value(VERBOSE_KEY, &until.to_string())?,
            None => db.delete_value(VERBOSE_KEY)?,
        }
    }
    log.set_verbose_until(until);
    let event = if enabled { "log.verbose.started" } else { "log.verbose.stopped" };
    log.write(Level::Info, Source::App, event, serde_json::json!({ "until": until.map(rfc3339) }));
    Ok(status(&app))
}

/// 画面の出来事を書く。`level` は "info" か "error"。出来事の名前には、画面のものと分かるよう「ui.」を付ける。
#[tauri::command]
pub fn write_debug_log(app: AppHandle, level: String, event: String, fields: Option<Value>) {
    let Some(log) = app.try_state::<DebugLog>() else {
        return;
    };
    let level = if level == "error" { Level::Error } else { Level::Info };
    let event = if event.starts_with("ui.") { event } else { format!("ui.{event}") };
    log.write(level, Source::Ui, &event, fields.unwrap_or(Value::Null));
}

fn rfc3339(ms: i64) -> String {
    DateTime::from_timestamp_millis(ms)
        .map(|t| t.with_timezone(&Local).to_rfc3339())
        .unwrap_or_default()
}

/// 書き出す環境の情報。
fn environment(app: &AppHandle) -> Environment {
    let state = app.state::<AppState>();
    let settings = state.db().load_settings().ok();
    let keys = api_key_status(state.secrets.as_ref()).ok();
    let now = now_ms();
    Environment {
        app_version: app.package_info().version.to_string(),
        os: debug_bundle::os_description(),
        os_build: debug_bundle::windows_build(),
        arch: std::env::consts::ARCH.to_string(),
        created_at: rfc3339(now),
        verbose_until: app
            .try_state::<DebugLog>()
            .and_then(|log| log.verbose_until(now))
            .map(rfc3339),
        settings: settings.map(|settings| SafeSettings::new(&settings, keys.as_ref())),
    }
}

/// ログのファイル・会議の検出の記録・環境の情報を、1 つの zip にまとめる（ファイルには書かない）。
/// zip の中身と、入れたものの名前を返す。「デバッグ用のログを保存…」と、後で作る「提出」の両方から使う。
pub fn build_bundle(app: &AppHandle) -> std::io::Result<(Vec<u8>, Vec<String>)> {
    let environment = environment(app);
    let record = crate::meeting_watch::export_record(app);
    let log_files = app.try_state::<DebugLog>().map(|log| log.files()).unwrap_or_default();
    debug_bundle::build_bundle(&BundleInput {
        log_files: &log_files,
        environment: &environment,
        meeting_record: &record,
    })
}

/// デバッグ用のログを、保存ダイアログで選ばれた場所に zip で保存する。zip に入れたファイルの数を返す。
/// 誤って他の種類のファイルを上書きしないよう、拡張子は .zip だけを受け付ける。
#[tauri::command(async)]
pub fn save_debug_bundle(app: AppHandle, path: String) -> CmdResult<usize> {
    let path = std::path::PathBuf::from(path);
    let is_zip = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"));
    if !is_zip {
        return Err(CommandError("保存できるのは .zip ファイルだけです".into()));
    }
    info(&app, "log.bundle.saving", serde_json::json!({}));
    let (bytes, names) =
        build_bundle(&app).map_err(|e| CommandError(format!("デバッグ用のログをまとめられませんでした: {e}")))?;
    std::fs::write(&path, bytes).map_err(|e| CommandError(format!("ファイルを保存できませんでした: {e}")))?;
    Ok(names.len())
}
