//! ヘルプ画面（使い方・リリースノート）のコマンド。取得とフォールバックのロジックは kiroscribe-core にある。
//!
//! ウェブビューからは外部へ接続させず（CSP は変えない）、Rust 側で GitHub から取得する。

use kiroscribe_core::help::{self, Guide, HelpClient, ReleaseNotes};
use kiroscribe_core::time::now_ms;
use tauri::{AppHandle, State};

use crate::error::{CmdResult, CommandError};
use crate::state::AppState;

fn app_version(app: &AppHandle) -> String {
    app.package_info().version.to_string()
}

/// 使い方。取得できなければ、前回取得した内容かアプリ同梱の内容を返す（エラーにはしない）。
#[tauri::command]
pub async fn get_guide(app: AppHandle, state: State<'_, AppState>) -> CmdResult<Guide> {
    let fetched = HelpClient::new().fetch_guide().await;
    if let Err(err) = &fetched {
        eprintln!("使い方を取得できませんでした: {err}");
    }
    Ok(help::resolve_guide(&state.db(), &app_version(&app), fetched, now_ms()))
}

/// リリースノート。取得できなければ、前回取得した内容かアプリ同梱の CHANGELOG を返す。
#[tauri::command]
pub async fn get_release_notes(app: AppHandle, state: State<'_, AppState>) -> CmdResult<ReleaseNotes> {
    let fetched = HelpClient::new().fetch_releases().await;
    if let Err(err) = &fetched {
        eprintln!("リリースノートを取得できませんでした: {err}");
    }
    Ok(help::resolve_releases(&state.db(), &app_version(&app), fetched, now_ms()))
}

/// ヘルプの中のリンクを、既定のブラウザで開く。アプリのウィンドウは遷移させない。
#[tauri::command(async)]
pub fn open_external(url: String) -> CmdResult<()> {
    let url = help::allowed_external_url(&url).ok_or_else(|| {
        CommandError("このリンクは開けません（https:// で始まるリンクだけを開けます）".into())
    })?;
    tauri_plugin_opener::open_url(url, None::<&str>)
        .map_err(|err| CommandError(format!("ブラウザを開けませんでした: {err}")))
}
