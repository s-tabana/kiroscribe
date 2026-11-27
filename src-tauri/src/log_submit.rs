//! 「デバッグ用のログを提出」のコマンド。ログを zip にまとめ（[`debug::build_bundle`]）、
//! ログの回収の入口に送る（`kiroscribe_core::log_submit`）。

use std::sync::Arc;

use kiroscribe_core::log_submit::{self, Progress, Submission};
use serde::Serialize;
use serde_json::json;
use tauri::{Emitter, State};

use crate::debug;
use crate::error::{CmdResult, CommandError};
use crate::runtime::AppHandle;
use crate::state::AppState;

/// 送った分を画面に知らせるイベント
pub const EVENT_LOG_SUBMIT_PROGRESS: &str = "log-submit-progress";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogSubmitProgress {
    pub sent: u64,
    pub total: u64,
}

/// デバッグ用のログを zip にまとめて送り、入口が返した提出の番号を返す。
/// 送り先は本体に埋め込んだ入口だけで、画面からは変えられない。送り先を使えなければ、まとめずに理由を返す。
#[tauri::command]
pub async fn submit_debug_logs(app: AppHandle, state: State<'_, AppState>) -> CmdResult<Submission> {
    let intake = log_submit::intake()?;
    let client_id = log_submit::client_id(&state.db())?;

    let bundler = app.clone();
    let (zip, _) = tauri::async_runtime::spawn_blocking(move || debug::build_bundle(&bundler))
        .await
        .map_err(|e| CommandError(format!("デバッグ用のログをまとめられませんでした: {e}")))?
        .map_err(|e| CommandError(format!("デバッグ用のログをまとめられませんでした: {e}")))?;
    let size = zip.len();
    debug::info(&app, "log.submit.start", json!({ "bytes": size }));

    let emitter = app.clone();
    let progress: Progress = Arc::new(move |sent, total| {
        let _ = emitter.emit(EVENT_LOG_SUBMIT_PROGRESS, LogSubmitProgress { sent, total });
    });
    let version = app.package_info().version.to_string();
    let result = log_submit::submit_logs(
        &log_submit::client(),
        &intake,
        &version,
        &client_id,
        zip,
        progress,
    )
    .await;
    match &result {
        Ok(submission) => debug::info(&app, "log.submit.done", json!({ "id": submission.id, "bytes": size })),
        Err(err) => debug::error(&app, "log.submit.failed", json!({ "message": err.to_string() })),
    }
    Ok(result?)
}
