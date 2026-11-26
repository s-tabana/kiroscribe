//! 会議の相棒の設定の欄: 探すフォルダの追加・外す・「中身を送らない」と、資料の索引（作り直し・状態の知らせ）。
//!
//! 索引を作るのは、相棒がオンのときだけ（起動時・相棒をオンにして保存したとき・フォルダを足したとき）。
//! オフにして保存すると、索引の作成と見張りを止めて、索引の中身を消す。探すフォルダの一覧は残す。

use std::path::{Path, PathBuf};

use kiroscribe_core::search::{overlap, IndexStatus, Indexer, Overlap, SearchFolder, INDEX_FILE};
use kiroscribe_core::settings::Settings;
use tauri::{Emitter, Manager, State};

use crate::error::{CmdResult, CommandError};
use crate::runtime::AppHandle;
use crate::state::AppState;

/// 索引の状態が変わったときに画面へ送るイベント。
pub const EVENT_SEARCH_INDEX_STATUS: &str = "search-index-status";

pub struct CompanionState {
    pub indexer: Indexer,
}

/// 索引の作業用のスレッドを起こし、保存した設定に合わせて索引を作り始める（オフなら中身を消す）。
pub fn setup(app: &AppHandle, data_dir: &Path, settings: &Settings, folders: Vec<SearchFolder>) {
    let handle = app.clone();
    let indexer = Indexer::spawn(data_dir.join(INDEX_FILE), move |status| {
        let _ = handle.emit(EVENT_SEARCH_INDEX_STATUS, status);
    });
    if settings.companion {
        indexer.enable(folders);
    } else {
        indexer.set_folders(folders);
        indexer.disable();
    }
    app.manage(CompanionState { indexer });
}

/// 設定を保存したときに呼ぶ。相棒のオンとオフが変わったら、索引を作り始めるか、止めて消す。
pub fn settings_changed(app: &AppHandle, before: Option<&Settings>, saved: &Settings) {
    let Some(companion) = app.try_state::<CompanionState>() else {
        return;
    };
    if before.map(|b| b.companion) == Some(saved.companion) {
        return;
    }
    if saved.companion {
        let folders = app.state::<AppState>().db().load_search_folders().unwrap_or_default();
        companion.indexer.enable(folders);
    } else {
        companion.indexer.disable();
    }
}

fn save_folders(state: &AppState, companion: &CompanionState, folders: Vec<SearchFolder>) -> CmdResult<IndexStatus> {
    state.db().save_search_folders(&folders)?;
    companion.indexer.set_folders(folders);
    Ok(companion.indexer.status())
}

/// フォルダを選ぶダイアログを出す（画面にはファイルを選ぶ権限を渡さないので、本体が出す）。取り消したら None。
fn pick_folder(app: &AppHandle) -> Option<PathBuf> {
    #[cfg(feature = "e2e")]
    {
        let _ = app;
        crate::e2e::pick_folder().map(PathBuf::from)
    }
    #[cfg(not(feature = "e2e"))]
    {
        use tauri_plugin_dialog::DialogExt;
        app.dialog().file().set_title("探すフォルダを追加").blocking_pick_folder()?.into_path().ok()
    }
}

#[tauri::command]
pub fn get_search_index_status(companion: State<'_, CompanionState>) -> CmdResult<IndexStatus> {
    Ok(companion.indexer.status())
}

/// 「フォルダを追加…」: 選んだフォルダを一覧に足して、その場で保存する。取り消したら null。
#[tauri::command]
pub async fn add_search_folder(
    app: AppHandle,
    state: State<'_, AppState>,
    companion: State<'_, CompanionState>,
) -> CmdResult<Option<IndexStatus>> {
    let picker = app.clone();
    let picked = tauri::async_runtime::spawn_blocking(move || pick_folder(&picker))
        .await
        .map_err(|err| CommandError(format!("フォルダを選べませんでした: {err}")))?;
    let Some(path) = picked else {
        return Ok(None);
    };
    if !path.is_dir() {
        return Err(CommandError("フォルダを選んでください".into()));
    }
    let path = path.to_string_lossy().to_string();
    let mut folders = state.db().load_search_folders()?;
    match overlap(&folders, &path) {
        // 一覧にあるフォルダなら、何も変えない（調べ直さない）
        Some(Overlap::Same(_)) => return Ok(Some(companion.indexer.status())),
        // 同じファイルを 2 つのフォルダで数えないよう、重なるフォルダは足さない
        Some(Overlap::Nested(existing)) => {
            return Err(CommandError(format!(
                "追加したフォルダ（{}）と中身が重なるため、追加できません。中のフォルダや、それを含むフォルダは、どちらか一方だけを追加してください",
                existing.path
            )))
        }
        None => {}
    }
    folders.push(SearchFolder { path, no_content: false });
    save_folders(&state, &companion, folders).map(Some)
}

/// 「外す」: 一覧から外して、その場で保存する（そのフォルダのファイルは索引から消える）。
#[tauri::command]
pub fn remove_search_folder(
    state: State<'_, AppState>,
    companion: State<'_, CompanionState>,
    path: String,
) -> CmdResult<IndexStatus> {
    let mut folders = state.db().load_search_folders()?;
    folders.retain(|folder| folder.path != path);
    save_folders(&state, &companion, folders)
}

/// 「中身を送らない」を切り替えて、その場で保存する。
#[tauri::command]
pub fn set_search_folder_no_content(
    state: State<'_, AppState>,
    companion: State<'_, CompanionState>,
    path: String,
    value: bool,
) -> CmdResult<IndexStatus> {
    let mut folders = state.db().load_search_folders()?;
    let Some(folder) = folders.iter_mut().find(|folder| folder.path == path) else {
        return Err(CommandError("このフォルダは一覧にありません".into()));
    };
    if folder.no_content != value {
        folder.no_content = value;
        state.db().save_search_folders(&folders)?;
        // 索引の中身は変わらないので、調べ直さない（作っている途中の処理も止めない）
        companion.indexer.set_folder_options(folders);
    }
    Ok(companion.indexer.status())
}

/// 「索引を作り直す」: 索引を空にしてから作り直す。相棒がオフのときは作り直さない。
#[tauri::command]
pub fn rebuild_search_index(
    state: State<'_, AppState>,
    companion: State<'_, CompanionState>,
) -> CmdResult<IndexStatus> {
    if !state.db().load_settings()?.companion || !companion.indexer.rebuild() {
        return Err(CommandError("会議の相棒がオフのときは、索引を作り直せません".into()));
    }
    Ok(companion.indexer.status())
}
