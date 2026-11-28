//! 会議の相棒の設定の欄: 探すフォルダの追加・外す・「中身を送らない」と、資料の索引（作り直し・状態の知らせ）。
//!
//! 索引を作るのは、相棒がオンのときだけ（起動時・相棒をオンにして保存したとき・フォルダを足したとき）。
//! オフにして保存すると、索引の作成と見張りを止めて、索引の中身を消す。探すフォルダの一覧は残す。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use kiroscribe_core::search::store::NoteDoc;
use kiroscribe_core::search::{overlap, IndexStatus, Indexer, NoteSource, Overlap, SearchFolder, INDEX_FILE};
use kiroscribe_core::settings::Settings;
use kiroscribe_core::{CoreError, Database};
use tauri::{Emitter, Manager, State};

use crate::error::{CmdResult, CommandError};
use crate::runtime::AppHandle;
use crate::state::AppState;

/// 索引の状態が変わったときに画面へ送るイベント。
pub const EVENT_SEARCH_INDEX_STATUS: &str = "search-index-status";

pub struct CompanionState {
    pub indexer: Indexer,
    /// 録音中（または仕上げ中）のノート。過去のノートの索引には、録音が終わってから入れる
    recording: Mutex<Option<String>>,
}

/// 過去のノートの索引を作るか（保存した設定で、相棒と「Kiroscribe の過去のノート」の両方がオン）。
fn notes_on(settings: &Settings) -> bool {
    settings.companion && settings.companion_notes
}

/// 索引に入れるノートの中身（題名・自分のメモ・議事録・文字起こし）。削除されていれば None。
/// ノートか文字起こしを読めなければエラー（空の中身で、索引の行を上書きしない）。
fn note_doc(db: &Database, note_id: &str) -> kiroscribe_core::Result<Option<NoteDoc>> {
    let note = match db.get_note(note_id) {
        Ok(note) => note,
        Err(CoreError::NoteNotFound(_)) => return Ok(None),
        Err(err) => return Err(err),
    };
    let transcript = db
        .list_segments(note_id)?
        .into_iter()
        .map(|segment| format!("{}: {}", segment.speaker.label(), segment.text))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Some(NoteDoc {
        note_id: note.id,
        title: note.title,
        created_at: note.created_at,
        memo: note.user_notes,
        minutes: note.enhanced_notes.unwrap_or_default(),
        transcript,
    }))
}

/// 過去のノートの原本。索引の作業用のスレッドが、読むためだけの別の接続から 1 件ずつ読む
/// （画面の操作と録音の保存を待たせない）。
struct NoteDatabase {
    app: AppHandle,
    /// ノートのデータベースのファイル
    db_path: PathBuf,
    /// 読むためだけの接続（開けなかったら、次に読むときに開き直す）
    reader: Mutex<Option<Database>>,
}

impl NoteDatabase {
    /// 読むためだけの接続で読む。開けないとき（データベースがファイルでないときなど）は、共有の接続を借りる。
    fn read<T>(&self, f: impl Fn(&Database) -> kiroscribe_core::Result<T>) -> kiroscribe_core::Result<T> {
        let mut reader = self.reader.lock().unwrap_or_else(|e| e.into_inner());
        if reader.is_none() {
            *reader = Database::open_read_only(&self.db_path).ok();
        }
        if let Some(db) = reader.as_ref() {
            let result = f(db);
            // 読めなかったときは、次に読むときに開き直す
            if result.is_err() {
                *reader = None;
            }
            return result;
        }
        drop(reader);
        let state = self.app.try_state::<AppState>().ok_or_else(|| CoreError::Other("ノートのデータベースを開けません".into()))?;
        let db = state.db();
        f(&db)
    }
}

impl NoteSource for NoteDatabase {
    fn note_ids(&self) -> kiroscribe_core::Result<Vec<String>> {
        self.read(|db| Ok(db.list_notes()?.into_iter().map(|note| note.id).collect()))
    }

    fn note(&self, note_id: &str) -> kiroscribe_core::Result<Option<NoteDoc>> {
        self.read(|db| note_doc(db, note_id))
    }
}

impl CompanionState {
    fn recording_note(&self) -> Option<String> {
        self.recording.lock().unwrap().clone()
    }
}

/// 過去のノートの索引を、保存した設定に合わせる（作るなら空にして入れ直し、作らないなら空にする）。
/// 入れ直しは、索引の作業用のスレッドが、原本から 1 件ずつ読んで行う。
fn sync_notes(app: &AppHandle, settings: &Settings) {
    let Some(companion) = app.try_state::<CompanionState>() else { return };
    companion.indexer.set_notes(notes_on(settings));
}

/// ノート・議事録・文字起こしが保存されたら呼ぶ。録音中のノートは、録音が終わってから入れる。
pub fn note_saved(app: &AppHandle, note_id: &str) {
    let Some(companion) = app.try_state::<CompanionState>() else { return };
    if companion.recording_note().as_deref() == Some(note_id) {
        return;
    }
    companion.indexer.note_saved(note_id);
}

/// ノートを削除したら呼ぶ（そのノートを、すぐに検索から外し、索引から消す）。
pub fn note_deleted(app: &AppHandle, note_id: &str) {
    if let Some(companion) = app.try_state::<CompanionState>() {
        companion.indexer.note_removed(note_id);
    }
}

/// 録音を始めたら、録音のロックの中で呼ぶ。録音を始めたノートは、すぐに検索から外す（索引にある前の中身も出さない）。
/// 止まったことは、止めたノートを指定する [`recording_stopped`] だけで反映する
/// （録音の状態の知らせは、止める処理と次の録音の開始の順と前後することがあるため使わない）。
pub fn recording_started(app: &AppHandle, note_id: &str) {
    let Some(companion) = app.try_state::<CompanionState>() else { return };
    *companion.recording.lock().unwrap() = Some(note_id.to_string());
    companion.indexer.set_recording_note(Some(note_id));
}

/// 録音を止め終えたら（残りの文字起こしを保存し終えたら）、止めたノート `note_id` で呼ぶ。
/// 仕上げ中のあいだ（次の録音を始められないあいだ）に呼ぶ。止めたノートを、索引に入れ直す。
pub fn recording_stopped(app: &AppHandle, note_id: &str) {
    let Some(companion) = app.try_state::<CompanionState>() else { return };
    {
        let mut recording = companion.recording.lock().unwrap();
        if recording.as_deref() == Some(note_id) {
            *recording = None;
            companion.indexer.set_recording_note(None);
        }
    }
    note_saved(app, note_id);
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
    // 過去のノートは、作業用のスレッドが、ノートのデータベースから 1 件ずつ読む
    indexer.set_note_source(Arc::new(NoteDatabase {
        app: app.clone(),
        db_path: data_dir.join(crate::startup::DB_FILE),
        reader: Mutex::new(None),
    }));
    let companion = CompanionState { indexer, recording: Mutex::new(None) };
    app.manage(companion);
    sync_notes(app, settings);
}

/// アプリを終えるときに呼ぶ。索引の作業用のスレッドを止めて、少しのあいだ終わるのを待つ
/// （作業用のスレッドは状態を知らせるためにアプリを持っているので、アプリが片付くだけでは止まらない）。
pub fn shutdown(app: &AppHandle) {
    if let Some(companion) = app.try_state::<CompanionState>() {
        companion.indexer.shutdown_and_wait(SHUTDOWN_WAIT);
    }
}

/// 終えるときに、索引の作業用のスレッドが終わるのを待つ長さ（読んでいる途中の資料を読み終えるまで）。
const SHUTDOWN_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// 設定を保存したときに呼ぶ。相棒のオンとオフが変わったら、索引を作り始めるか、止めて消す。
pub fn settings_changed(app: &AppHandle, before: Option<&Settings>, saved: &Settings) {
    let Some(companion) = app.try_state::<CompanionState>() else {
        return;
    };
    let state = app.state::<AppState>();
    let companion_changed = before.map(|b| b.companion) != Some(saved.companion);
    if companion_changed {
        if saved.companion {
            let folders = state.db().load_search_folders().unwrap_or_default();
            companion.indexer.enable(folders);
        } else {
            // 設定の画面は開いたままなので、消し始めたこと（「消している」）をすぐ知らせる。
            // 消し終えた知らせが先に届いていても、画面は番号の小さい古い状態で上書きしない
            let status = companion.indexer.disable();
            let _ = app.emit(EVENT_SEARCH_INDEX_STATUS, &status);
        }
    }
    if companion_changed || before.map(notes_on) != Some(notes_on(saved)) {
        sync_notes(app, saved);
    }
}

fn save_folders(state: &AppState, companion: &CompanionState, folders: Vec<SearchFolder>) -> CmdResult<IndexStatus> {
    state.db().save_search_folders(&folders)?;
    Ok(companion.indexer.set_folders(folders))
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
    // リンクの先はたどらないので、リンクのフォルダは足さない（IDX-027）
    if kiroscribe_core::search::walk::is_link(&path) {
        return Err(CommandError(
            "ショートカットやリンク（ジャンクション・シンボリックリンク）のフォルダは追加できません。リンク先のフォルダを選んでください".into(),
        ));
    }
    // 選んだフォルダをその場で開き、その実体を記録する（あとで別の場所のフォルダに替わっても、中身を読まない）
    let folder = SearchFolder::register(&path).map_err(|err| CommandError(format!("フォルダを追加できません: {err}")))?;
    let mut folders = state.db().load_search_folders()?;
    match overlap(&folders, &folder.path) {
        Some(Overlap::Same(existing)) => {
            // 一覧にあるフォルダで、足したときと同じ実体なら、何も変えない（調べ直さない）
            if existing.path == folder.path && existing.place == folder.place {
                return Ok(Some(companion.indexer.status()));
            }
            // 足したときの実体を記録していない・作り直したフォルダは、選び直したフォルダの実体を記録し直す
            let no_content = existing.no_content;
            let position = folders.iter().position(|row| std::ptr::eq(row, existing)).expect("一覧の中の行");
            folders[position] = SearchFolder { no_content, ..folder };
        }
        // 同じファイルを 2 つのフォルダで数えないよう、重なるフォルダは足さない
        Some(Overlap::Nested(existing)) => {
            return Err(CommandError(format!(
                "追加したフォルダ（{}）と中身が重なるため、追加できません。中のフォルダや、それを含むフォルダは、どちらか一方だけを追加してください",
                existing.path
            )))
        }
        None => folders.push(folder),
    }
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
        return Ok(companion.indexer.set_folder_options(folders));
    }
    Ok(companion.indexer.status())
}

/// 「索引を作り直す」: 索引を空にしてから作り直す。相棒がオフのときは作り直さない。
#[tauri::command]
pub fn rebuild_search_index(
    app: AppHandle,
    state: State<'_, AppState>,
    companion: State<'_, CompanionState>,
) -> CmdResult<IndexStatus> {
    let settings = state.db().load_settings()?;
    let rebuilt = if settings.companion { companion.indexer.rebuild() } else { None };
    let status = rebuilt.ok_or_else(|| CommandError("会議の相棒がオフのときは、索引を作り直せません".into()))?;
    // 過去のノートも、両方がオンなら入れ直す
    sync_notes(&app, &settings);
    Ok(status)
}
