//! フロントエンドから `invoke` で呼ばれるコマンド。ロジックは kiroscribe-core に置き、ここは薄く保つ。

use kiroscribe_core::export::{note_to_markdown, suggested_file_name, ExportOptions};
use kiroscribe_core::notes::{Note, NotePatch, NoteSummary};
use kiroscribe_core::secrets::{self, ApiKeyStatus, Provider};
use kiroscribe_core::settings::Settings;
use tauri::State;

use crate::error::{CmdResult, CommandError};
use crate::recording::RecorderState;
use crate::runtime::AppHandle;
use crate::state::AppState;

// ---- ノート ----

#[tauri::command]
pub fn list_notes(state: State<'_, AppState>) -> CmdResult<Vec<NoteSummary>> {
    Ok(state.db().list_notes()?)
}

/// 検索の文字を、タイトル・自分のメモ・議事録・文字起こしのどれかに含むノートの ID を、一覧と同じ並びで返す。
/// 画面は、その場で分かるタイトルとプレビューでの絞り込みに、この結果を足して表示する。
#[tauri::command]
pub fn search_notes(state: State<'_, AppState>, query: String) -> CmdResult<Vec<String>> {
    Ok(state.db().search_notes(&query)?)
}

#[tauri::command]
pub fn create_note(state: State<'_, AppState>) -> CmdResult<Note> {
    Ok(state.db().create_note()?)
}

#[tauri::command]
pub fn get_note(state: State<'_, AppState>, id: String) -> CmdResult<Note> {
    Ok(state.db().get_note(&id)?)
}

#[tauri::command]
pub fn update_note(state: State<'_, AppState>, id: String, patch: NotePatch) -> CmdResult<Note> {
    Ok(state.db().update_note(&id, &patch)?)
}

#[tauri::command]
pub async fn delete_note(
    state: State<'_, AppState>,
    recorder: State<'_, RecorderState>,
    id: String,
) -> CmdResult<()> {
    if recorder.busy_note_id().await.as_deref() == Some(id.as_str()) {
        return Err(CommandError(
            "録音中のノートは削除できません。先に録音を停止してください".into(),
        ));
    }
    Ok(state.db().delete_note(&id)?)
}

// ---- 設定 ----

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> CmdResult<Settings> {
    Ok(state.db().load_settings()?)
}

#[tauri::command]
pub fn save_settings(
    app: AppHandle,
    state: State<'_, AppState>,
    settings: Settings,
) -> CmdResult<Settings> {
    let (was_auto_recording, saved) = {
        let db = state.db();
        let before = db.load_settings().map(|before| before.auto_record_meetings).unwrap_or(false);
        (before, db.save_settings(settings)?)
    };
    crate::background::apply_settings(&app, &saved);
    crate::update::apply_settings(&app, &saved);
    crate::meeting_watch::settings_changed(&app, was_auto_recording, &saved);
    Ok(saved)
}

/// キーが設定済みかどうかだけを返す。値はフロントに返さない。
#[tauri::command(async)]
pub fn get_api_key_status(state: State<'_, AppState>) -> CmdResult<ApiKeyStatus> {
    Ok(secrets::api_key_status(state.secrets.as_ref())?)
}

/// キーを保存する（空文字なら削除）。保存後の状態を返す。
#[tauri::command(async)]
pub fn set_api_key(
    state: State<'_, AppState>,
    provider: Provider,
    value: String,
) -> CmdResult<ApiKeyStatus> {
    secrets::set_api_key(state.secrets.as_ref(), provider, &value)?;
    Ok(secrets::api_key_status(state.secrets.as_ref())?)
}

// ---- 書き出し ----

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportResult {
    pub markdown: String,
    /// 保存ダイアログの既定のファイル名
    pub file_name: String,
}

fn build_export(state: &AppState, note_id: &str, options: ExportOptions) -> CmdResult<ExportResult> {
    let db = state.db();
    let note = db.get_note(note_id)?;
    let segments = db.list_segments(note_id)?;
    let settings = db.load_settings()?;
    Ok(ExportResult {
        markdown: note_to_markdown(&note, &segments, options, &settings.my_name),
        file_name: suggested_file_name(&note),
    })
}

#[tauri::command]
pub fn export_markdown(
    state: State<'_, AppState>,
    note_id: String,
    options: ExportOptions,
) -> CmdResult<ExportResult> {
    build_export(&state, &note_id, options)
}

/// 保存ダイアログで選ばれたパスに書き出す。誤って他の種類のファイルを上書きしないよう拡張子を制限する。
#[tauri::command(async)]
pub fn save_markdown(
    state: State<'_, AppState>,
    note_id: String,
    options: ExportOptions,
    path: String,
) -> CmdResult<()> {
    let path = std::path::PathBuf::from(path);
    let allowed = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "md" | "markdown" | "txt"))
        .unwrap_or(false);
    if !allowed {
        return Err(CommandError(
            "保存できるのは .md / .markdown / .txt ファイルだけです".into(),
        ));
    }
    let export = build_export(&state, &note_id, options)?;
    std::fs::write(&path, export.markdown)
        .map_err(|e| CommandError(format!("ファイルを保存できませんでした: {e}")))?;
    Ok(())
}
