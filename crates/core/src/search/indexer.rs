//! 資料の索引を裏で作り、フォルダの変更を見張って更新する。
//!
//! 索引への読み書きは、1 本の作業用のスレッドだけが行う。画面の操作と録音は止めない。
//! 相棒がオフのあいだは、索引を作らず、見張りもしない（オフにしたときは、索引の中身を消す）。

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::extract::{extract_within, Extracted};
use super::store::{erase_marker, is_search_suspended, remove_index_files, resume_search, stamp_of_file, suspend_search, Binding, Hit, NoteDoc, NoteHit, SearchIndex};
use super::walk::{is_online_only, is_outside, FileEntry, FileId, Limits, Place, Root};
use crate::time::now_ms;

/// 見張れないフォルダ（ネットワークのドライブなど）を見直す間隔。
pub const POLL_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// 見張っているフォルダそのものがあるかを確かめる間隔。Windows では、見張っているフォルダ自身を移したことは
/// 変更の知らせに含まれないため、ここで見つける。
pub const ROOT_CHECK_INTERVAL: Duration = Duration::from_secs(5);
/// 変更の知らせを受けてから、まとめて反映するまでの待ち時間（続けて届く知らせを 1 回にまとめる）。
pub const CHANGE_DEBOUNCE: Duration = Duration::from_secs(2);
/// 変更の知らせが途切れずに届き続けても、最初の知らせからこの時間がたったら反映する。
pub const MAX_CHANGE_DELAY: Duration = Duration::from_secs(10);
/// 索引のファイルを消せず、中身も空にできなかったときに、消し直す間隔。
pub const PURGE_RETRY: Duration = Duration::from_secs(60);

/// 見直しの間隔。
#[derive(Debug, Clone, Copy)]
pub struct Intervals {
    /// 見張れないフォルダ・見つからないフォルダを見直す間隔
    pub poll: Duration,
    /// 見張っているフォルダそのものがあるかを確かめる間隔
    pub root_check: Duration,
    /// 索引を消せなかったときに、消し直す間隔
    pub purge_retry: Duration,
}

impl Default for Intervals {
    fn default() -> Self {
        Self { poll: POLL_INTERVAL, root_check: ROOT_CHECK_INTERVAL, purge_retry: PURGE_RETRY }
    }
}

/// 探すフォルダ（保存する設定）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchFolder {
    pub path: String,
    /// 「中身を送らない」
    #[serde(default)]
    pub no_content: bool,
    /// 足したときに開いたフォルダの実体。これと同じフォルダ（同じ場所で作り直したものを含む）だけを索引に入れる。
    /// ない（実体を記録する前の設定・読めない値）ときは、利用者が選び直すまで、中身を読まない
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "place_or_none")]
    pub place: Option<Place>,
}

impl SearchFolder {
    /// 選んだフォルダを開き、その実体を記録した探すフォルダにする。
    /// 文字に直せない名前（損なわずに保存できない）・リンク・フォルダでないもの・実際の場所を確かめられないものは、エラー。
    pub fn register(path: &Path) -> std::io::Result<SearchFolder> {
        let text = path.to_str().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "文字に直せない名前を含むフォルダは追加できません")
        })?;
        let place = Root::open(path)?.place();
        if place.is_unverifiable() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "このフォルダは、実際の場所を確かめられないため追加できません",
            ));
        }
        Ok(SearchFolder { path: text.to_string(), no_content: false, place: Some(place) })
    }
}

/// 保存した実体を読む。読めない値は、記録していないものとして扱う（選び直すまで、中身を読まない）。
fn place_or_none<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Place>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IndexState {
    /// 相棒がオフで、索引を作っていない（索引の中身を消し終えている）
    Off,
    /// 相棒をオフにして、索引の中身を消している（消せないときは、消し終えるまで消し直す）
    Erasing,
    /// 索引を作っている・更新している
    Building,
    /// 索引ができている
    Ready,
}

/// 探すフォルダの一覧の 1 行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderStatus {
    pub path: String,
    pub no_content: bool,
    /// 索引に入っているファイルの件数
    pub files: usize,
    /// フォルダが見つからない（ドライブが外れているなど）
    pub missing: bool,
    /// ファイルが多く、一部だけを索引に入れている
    pub truncated: bool,
    /// 足したときのフォルダの実体を記録していないため、選び直すまで索引に入れない
    pub unconfirmed: bool,
}

/// 索引の状態（設定の欄に表示する）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexStatus {
    pub state: IndexState,
    /// 作っている途中なら、調べ終えたファイルの数と、見つけたファイルの数
    pub done: usize,
    pub total: usize,
    /// 索引に入っているファイルの件数
    pub files: usize,
    /// 最後に索引を更新し終えた時刻
    pub updated_at: Option<i64>,
    pub folders: Vec<FolderStatus>,
    /// 状態が変わるたびに増える番号（画面は、これが小さい古い状態で、新しい状態を上書きしない）
    pub revision: u64,
}

type StatusCallback = Arc<dyn Fn(&IndexStatus) + Send + Sync>;

enum Command {
    /// 相棒がオンになった（または起動した）。索引を作り始める
    Enable(Vec<SearchFolder>),
    /// 相棒がオフになった。止めて、索引の中身を消す
    Disable,
    /// 探すフォルダの一覧が変わった
    SetFolders(Vec<SearchFolder>),
    /// 索引を空にして作り直す
    Rebuild,
}

enum Message {
    /// 画面の操作から来た命令と、そのときの世代
    Command(u64, Command),
    /// フォルダの中で変更があった（見張りの知らせ）
    Changed(PathBuf),
    /// 見張りが変更を取りこぼした（知らせのあふれ）。すべてのフォルダを調べ直す
    Rescan,
    /// 見張りに失敗した（見張りが止まったことがある）。見張り直して、すべてのフォルダを調べ直す
    WatchFailed,
    /// 過去のノートの索引を空にし、その世代で、すべてのノートを原本から読んで入れ直す
    ResetNotes(u64),
    /// ノートが保存された（原本から読み直して入れる）
    NoteChanged(String),
    /// ノートが削除された
    RemoveNote(String),
    /// 作業用のスレッドを止める
    Shutdown,
}

struct Shared {
    status: IndexStatus,
    enabled: bool,
    /// 過去のノートの索引を作るか（相棒と「Kiroscribe の過去のノート」の両方がオン）
    notes: bool,
    /// 過去のノートの索引の世代（入れ直すたびに進める。古い世代の読み込みは反映しない）
    notes_generation: u64,
    /// 削除したノート（索引から消し終える前でも検索に出さず、あとから届いた古い中身も入れない）
    removed_notes: HashSet<String>,
    /// 録音中のノート（検索に出さない。索引には、録音が終わってから入れ直す）
    recording_note: Option<String>,
    /// ノートの索引を空にできず、古い行が残っているかもしれない（空にし直せるまで、ノートを探さない）
    notes_stale: bool,
    /// ノートの索引を空にし終えた世代。いまの世代と同じになるまで、ノートを探さない
    /// （前回の起動で消せなかった削除済みのノートなど、前の世代の行を出さない）
    notes_cleared: u64,
    /// 索引の行が原本より古いかもしれないノート（読む・書くのに失敗した。入れ直せるまで検索に出さない）
    unsynced_notes: HashSet<String>,
    /// 索引を開けず、行を消せていないフォルダ（消し終えるまで検索に出さない）
    hidden_folders: HashSet<PathBuf>,
    folders: Vec<SearchFolder>,
    /// 命令を出すたびに増やす。作っている途中の処理は、これが変わったら途中でやめ、古い状態を知らせない
    generation: u64,
}

impl Shared {
    fn set_status(&mut self, status: IndexStatus) {
        let revision = self.status.revision + 1;
        self.status = IndexStatus { revision, ..status };
    }
}

/// 検索用の接続（作業用のスレッドとは別の接続。索引を作っている途中でも、待たずに探せるように）。
#[derive(Default)]
struct Reader {
    index: Option<SearchIndex>,
    /// 索引を空にする・消す処理のうち、終わっていないものの数（0 になるまで、検索用の接続を開かない。
    /// 開いたままだと、Windows では索引のファイルを消せない）
    blocks: usize,
}

/// 過去のノートの原本（アプリのノートのデータベース）。作業用のスレッドが、ノートを 1 件ずつ読む。
pub trait NoteSource: Send + Sync {
    /// すべてのノートの ID。読めなければエラー（あとで読み直す）。
    fn note_ids(&self) -> crate::error::Result<Vec<String>>;
    /// ノート 1 件の中身。削除されていれば None。一部でも読めなければエラー（あとで読み直す）。
    fn note(&self, note_id: &str) -> crate::error::Result<Option<NoteDoc>>;
}

type SharedSource = Arc<Mutex<Option<Arc<dyn NoteSource>>>>;

struct Inner {
    tx: Sender<Message>,
    shared: Arc<Mutex<Shared>>,
    /// 状態の知らせ（`on_status`）と命令の順を守る。知らせの途中に出た命令は、知らせのあとに出たことになる
    order: Arc<Mutex<()>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    index_path: PathBuf,
    reader: Arc<Mutex<Reader>>,
    note_source: SharedSource,
}

impl Drop for Inner {
    /// 最後の窓口がなくなったら、作業用のスレッドを止めて、終わるまで待つ。
    fn drop(&mut self) {
        self.shared.lock().unwrap().generation += 1;
        let _ = self.tx.send(Message::Shutdown);
        if let Some(thread) = self.thread.lock().unwrap().take() {
            let _ = thread.join();
        }
    }
}

/// 資料の索引の作業用のスレッドへの窓口。
#[derive(Clone)]
pub struct Indexer {
    inner: Arc<Inner>,
}

impl Indexer {
    /// 作業用のスレッドを起こす。`on_status` は、状態が変わるたびに（作っている途中は間引いて）呼ぶ。
    /// `on_status` の中から、この窓口の命令（`enable` など）を呼ばない（状態を読む `status` は呼べる）。
    pub fn spawn(index_path: PathBuf, on_status: impl Fn(&IndexStatus) + Send + Sync + 'static) -> Self {
        Self::spawn_with(index_path, Limits::default(), on_status)
    }

    pub fn spawn_with(index_path: PathBuf, limits: Limits, on_status: impl Fn(&IndexStatus) + Send + Sync + 'static) -> Self {
        Self::spawn_with_intervals(index_path, limits, Intervals::default(), on_status)
    }

    pub fn spawn_with_intervals(
        index_path: PathBuf,
        limits: Limits,
        intervals: Intervals,
        on_status: impl Fn(&IndexStatus) + Send + Sync + 'static,
    ) -> Self {
        let (tx, rx) = channel();
        let shared = Arc::new(Mutex::new(Shared {
            status: IndexStatus {
                state: IndexState::Off,
                done: 0,
                total: 0,
                files: 0,
                updated_at: None,
                folders: Vec::new(),
                revision: 0,
            },
            enabled: false,
            notes: false,
            notes_generation: 0,
            removed_notes: HashSet::new(),
            recording_note: None,
            notes_stale: false,
            notes_cleared: 0,
            unsynced_notes: HashSet::new(),
            hidden_folders: HashSet::new(),
            folders: Vec::new(),
            generation: 0,
        }));
        let order = Arc::new(Mutex::new(()));
        let reader = Arc::new(Mutex::new(Reader::default()));
        let note_source: SharedSource = Arc::new(Mutex::new(None));
        let mut worker = Worker::new(index_path.clone(), limits, intervals, shared.clone(), order.clone(), Arc::new(on_status), tx.clone());
        worker.reader = reader.clone();
        worker.note_source = note_source.clone();
        let thread = std::thread::Builder::new()
            .name("search-index".into())
            .spawn(move || worker.run(rx))
            .expect("索引の作業用のスレッドを起こせません");
        Self {
            inner: Arc::new(Inner { tx, shared, order, thread: Mutex::new(Some(thread)), index_path, reader, note_source }),
        }
    }
}

impl Worker {
    fn new(
        index_path: PathBuf,
        limits: Limits,
        intervals: Intervals,
        shared: Arc<Mutex<Shared>>,
        order: Arc<Mutex<()>>,
        on_status: StatusCallback,
        tx: Sender<Message>,
    ) -> Self {
        Worker {
            limits,
            intervals,
            index: None,
            shared,
            order,
            on_status,
            tx,
            watcher: None,
            watched: HashMap::new(),
            polled: Vec::new(),
            last_poll: Instant::now(),
            last_root_check: Instant::now(),
            last_emit: Instant::now(),
            truncated: HashSet::new(),
            missing: HashSet::new(),
            erasure: Erasure::Unknown,
            last_purge: Instant::now(),
            changes: 0,
            incomplete: HashSet::new(),
            unverified: HashSet::new(),
            last_retry: Instant::now(),
            last_note_retry: Instant::now(),
            last_reopen: Instant::now(),
            reopen_pending: false,
            // 前回の起動で、索引を検索から外せないまま終わっていたら、消し終えるまで検索せずに消し直す
            suspended: is_search_suspended(&index_path),
            rescan: false,
            unplaced: HashSet::new(),
            reader: Arc::new(Mutex::new(Reader::default())),
            note_source: Arc::new(Mutex::new(None)),
            pending_removals: BTreeSet::new(),
            notes_clear_pending: false,
            note_queue: VecDeque::new(),
            queue_generation: 0,
            list_pending: None,
            note_retry: BTreeSet::new(),
            index_path,
        }
    }
}

impl Indexer {

    /// 共有の状態を変えて、命令を出す（同じ鍵の中で世代を進める。途中の処理が、古い状態を知らせないように）。
    /// 命令を出した時点の状態を返す。鍵を放したあとに読み直すと、作業用のスレッドの進み方で中身が変わるため。
    /// 命令を出さなかったときは None。
    fn command(&self, update: impl FnOnce(&mut Shared) -> Option<Command>) -> Option<IndexStatus> {
        let _order = self.inner.order.lock().unwrap();
        let mut shared = self.inner.shared.lock().unwrap();
        let command = update(&mut shared)?;
        shared.generation += 1;
        let _ = self.inner.tx.send(Message::Command(shared.generation, command));
        Some(shared.status.clone())
    }

    pub fn enable(&self, folders: Vec<SearchFolder>) {
        self.command(|shared| {
            shared.enabled = true;
            shared.folders = folders.clone();
            list_folders(shared);
            Some(Command::Enable(folders))
        });
    }

    /// 止めて、索引の中身を消す。消し終えるまでの状態は「消している」で、作業用のスレッドが消し終えてから「オフ」になる。
    /// 戻り値は、命令を出した直後の状態（「消している」）。
    pub fn disable(&self) -> IndexStatus {
        // 検索用の接続を閉じ、消し終えるまで開かせない（開いたままだと、Windows では索引のファイルを消せない）
        let mut reader = self.inner.reader.lock().unwrap();
        reader.index = None;
        reader.blocks += 1;
        self.command(|shared| {
            shared.enabled = false;
            let status = IndexStatus { state: IndexState::Erasing, done: 0, total: 0, updated_at: None, ..shared.status.clone() };
            shared.set_status(status);
            Some(Command::Disable)
        })
        .expect("オフにする命令は、必ず出す")
    }

    /// 探すフォルダの一覧を変える（足した・外した）。相棒がオンなら、索引に反映する。
    /// 戻り値は、変えた直後の状態（相棒がオンなら「作っている」）。
    pub fn set_folders(&self, folders: Vec<SearchFolder>) -> IndexStatus {
        self.command(|shared| {
            shared.folders = folders.clone();
            list_folders(shared);
            Some(Command::SetFolders(folders))
        })
        .expect("一覧を変える命令は、必ず出す")
    }

    /// 「中身を送らない」などの、索引の中身に関わらない設定だけを変える。
    /// 調べ直さず、作っている途中の処理も止めない。状態（作っている・できている）と更新した時刻も変えない。
    /// 戻り値は、変えた直後の状態。
    pub fn set_folder_options(&self, folders: Vec<SearchFolder>) -> IndexStatus {
        let _order = self.inner.order.lock().unwrap();
        let mut shared = self.inner.shared.lock().unwrap();
        let mut status = shared.status.clone();
        for row in &mut status.folders {
            if let Some(folder) = folders.iter().find(|folder| folder.path == row.path) {
                row.no_content = folder.no_content;
            }
        }
        for folder in &mut shared.folders {
            if let Some(changed) = folders.iter().find(|changed| changed.path == folder.path) {
                folder.no_content = changed.no_content;
            }
        }
        shared.set_status(status);
        shared.status.clone()
    }

    /// 索引を作り直す。戻り値は、命令を出した直後の状態（「作っている」）。相棒がオフなら何もしない（None）。
    pub fn rebuild(&self) -> Option<IndexStatus> {
        // 空にできずにファイルを消して作り直すときに備えて、検索用の接続を閉じ、空にし終えるまで開かせない
        let mut reader = self.inner.reader.lock().unwrap();
        let status = self.command(|shared| {
            if !shared.enabled {
                return None;
            }
            let status = IndexStatus { state: IndexState::Building, done: 0, total: 0, ..shared.status.clone() };
            shared.set_status(status);
            Some(Command::Rebuild)
        });
        if status.is_some() {
            reader.index = None;
            reader.blocks += 1;
        }
        status
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.shared.lock().unwrap().enabled
    }

    pub fn status(&self) -> IndexStatus {
        self.inner.shared.lock().unwrap().status.clone()
    }

    /// 過去のノートの索引を作るかを決め、ノートの索引を空にする。戻り値は、入れ直しの世代。
    /// 作るなら、作業用のスレッドが、原本（[`Indexer::set_note_source`]）からすべてのノートを 1 件ずつ読んで入れる。
    /// 同じ鍵の中で世代を進めるので、空にし終えるまで、前の世代の行は検索に出ない。
    pub fn set_notes(&self, on: bool) -> u64 {
        let mut shared = self.inner.shared.lock().unwrap();
        shared.notes = on;
        shared.notes_generation += 1;
        let generation = shared.notes_generation;
        // 世代を進めたのと同じ鍵の中で送る（世代の順と、命令の順をそろえる）
        let _ = self.inner.tx.send(Message::ResetNotes(generation));
        generation
    }

    /// 過去のノートの原本を決める（ノートの索引を作る前に呼ぶ）。
    pub fn set_note_source(&self, source: Arc<dyn NoteSource>) {
        *self.inner.note_source.lock().unwrap() = Some(source);
    }

    /// 過去のノートの索引を作っているか。
    pub fn notes_enabled(&self) -> bool {
        let shared = self.inner.shared.lock().unwrap();
        shared.enabled && shared.notes
    }

    /// 保存されたノートを、原本から読み直して索引に反映する（ノートの索引を作っていなければ何もしない）。
    pub fn note_saved(&self, note_id: &str) {
        if self.notes_enabled() {
            let _ = self.inner.tx.send(Message::NoteChanged(note_id.to_string()));
        }
    }

    /// 削除したノートを、すぐに検索から外し、索引から消す（消せなければ、消し直す間隔で消し直す）。
    pub fn note_removed(&self, note_id: &str) {
        self.inner.shared.lock().unwrap().removed_notes.insert(note_id.to_string());
        let _ = self.inner.tx.send(Message::RemoveNote(note_id.to_string()));
    }

    /// 録音中のノートを決める（録音中のあいだ、そのノートを検索に出さない）。
    pub fn set_recording_note(&self, note_id: Option<&str>) {
        self.inner.shared.lock().unwrap().recording_note = note_id.map(str::to_string);
    }

    /// 検索語で、指定したフォルダと過去のノートを探す（相棒がオフなら、何も見つからない）。
    /// `exclude_note` は除くノート（録音中のノート）。
    pub fn search(&self, terms: &[String], exclude_note: Option<&str>, limit: usize) -> (Vec<Hit>, Vec<NoteHit>) {
        self.search_by(terms, exclude_note, limit, |_| ())
    }

    /// [`Indexer::search`] と同じく探し、過去のノートは `note_key` の大きい順に並べてから `limit` 件にする。
    /// 録音中のノートと、削除したノートは、索引から消し終える前でも返さない。
    pub fn search_by<K: Ord>(
        &self,
        terms: &[String],
        exclude_note: Option<&str>,
        limit: usize,
        note_key: impl Fn(&NoteHit) -> K,
    ) -> (Vec<Hit>, Vec<NoteHit>) {
        // 接続の鍵を持ってからオンかを読む（オフにする処理が接続を閉じたあとに、開き直さないように）
        let mut reader = self.inner.reader.lock().unwrap();
        let (enabled, notes, exclude, scope) = {
            let shared = self.inner.shared.lock().unwrap();
            let mut exclude: Vec<String> = shared.removed_notes.iter().cloned().collect();
            exclude.extend(shared.unsynced_notes.iter().cloned());
            exclude.extend(shared.recording_note.iter().cloned());
            exclude.extend(exclude_note.map(str::to_string));
            // ノートは、いまの世代の索引を空にし終えてから探す
            let notes = shared.notes && !shared.notes_stale && shared.notes_cleared == shared.notes_generation;
            // フォルダの資料は、いま登録しているフォルダの、足したときの実体から読んだ行だけを探す
            let scope: Vec<(String, String)> = shared
                .folders
                .iter()
                .filter(|f| !shared.hidden_folders.contains(Path::new(&f.path)))
                .filter_map(|f| Some((f.path.clone(), place_key(f.place.as_ref()?))))
                .collect();
            (shared.enabled, notes, exclude, scope)
        };
        // 索引を空にする・消す処理の途中は、接続を開かない
        if !enabled || reader.blocks > 0 || !self.inner.index_path.exists() {
            return (Vec::new(), Vec::new());
        }
        // 消去待ちの索引は、接続を持ち続けない（消すのを妨げないように）
        if is_search_suspended(&self.inner.index_path) {
            reader.index = None;
            return (Vec::new(), Vec::new());
        }
        // 索引のファイルが消されて作り直されていたら、開き直す
        if !reader.index.as_ref().is_some_and(SearchIndex::is_current) {
            reader.index = SearchIndex::open_existing(&self.inner.index_path).ok();
        }
        let Some(index) = reader.index.as_ref() else {
            return (Vec::new(), Vec::new());
        };
        let hits = index.search_in(terms, &scope, limit).unwrap_or_default();
        let note_hits = if notes { index.search_notes_by(terms, &exclude, limit, note_key).unwrap_or_default() } else { Vec::new() };
        (hits, note_hits)
    }

    /// 作業用のスレッドを止める（終わるのは待たない。最後の窓口がなくなったときは、終わるまで待つ）。
    pub fn shutdown(&self) {
        self.inner.shared.lock().unwrap().generation += 1;
        let _ = self.inner.tx.send(Message::Shutdown);
    }

    /// 作業用のスレッドを止めて、`timeout` まで終わるのを待つ。終わったら true。
    pub fn shutdown_and_wait(&self, timeout: Duration) -> bool {
        self.shutdown();
        let start = Instant::now();
        let mut thread = self.inner.thread.lock().unwrap();
        loop {
            match thread.as_ref() {
                None => return true,
                Some(handle) if handle.is_finished() => {
                    let _ = thread.take().map(JoinHandle::join);
                    return true;
                }
                Some(_) if start.elapsed() >= timeout => return false,
                Some(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }
}

struct Worker {
    index_path: PathBuf,
    limits: Limits,
    intervals: Intervals,
    index: Option<SearchIndex>,
    shared: Arc<Mutex<Shared>>,
    order: Arc<Mutex<()>>,
    on_status: StatusCallback,
    tx: Sender<Message>,
    watcher: Option<notify::RecommendedWatcher>,
    /// 見張っているフォルダと、見張り始めたときのフォルダの識別子（同じ名前で作り直されたことを見つける）
    watched: HashMap<PathBuf, FileId>,
    /// 見張れないので、見直すフォルダ（ネットワークのドライブ・見つからなくなったフォルダ）
    polled: Vec<PathBuf>,
    last_poll: Instant,
    last_root_check: Instant,
    last_emit: Instant,
    truncated: HashSet<PathBuf>,
    missing: HashSet<PathBuf>,
    /// 索引を消し終えたか
    erasure: Erasure,
    last_purge: Instant,
    /// 索引に書いた回数（変わったファイルがあったときだけ、更新した時刻を進める）
    changes: u64,
    /// 中を読めなかったフォルダがあり、調べ切れていないフォルダ（見直しの間隔で調べ直す）
    incomplete: HashSet<PathBuf>,
    /// 出どころを確かめられない行を消せなかったフォルダ（検索には出さず、消し直す間隔で消し直す。
    /// 起動し直したあとは、索引の記録から同じことが分かる）
    unverified: HashSet<PathBuf>,
    last_retry: Instant,
    last_note_retry: Instant,
    last_reopen: Instant,
    /// 索引を開けなかった（書き込みのロック・容量不足・版の移行の失敗など。壊れてはいない）。開き直す間隔で開き直し、
    /// 開けたら一覧と照らし合わせて読み直す（ファイルは消さない）
    reopen_pending: bool,
    /// 索引の資料を検索から外せず、索引ごと消去待ちにしている（消し終えるまで検索しない。記録のファイルで、起動し直しても続く）
    suspended: bool,
    /// 索引を空にしたので、すべてのフォルダを読み直す
    rescan: bool,
    /// 開いたときに実際の場所が取れず、足したときと同じ場所かを確かめられないフォルダ（中身を読まず、選び直してもらう）
    unplaced: HashSet<PathBuf>,
    /// 検索用の接続（索引を空にする・消すあいだは閉じて、開かせない）
    reader: Arc<Mutex<Reader>>,
    note_source: SharedSource,
    /// 索引から消せなかった、削除したノート（消し直す間隔で消し直す。そのあいだも検索には出さない）
    pending_removals: BTreeSet<String>,
    /// ノートの索引を空にできなかった（消し直す間隔で空にし直し、空にできたら入れ直す）
    notes_clear_pending: bool,
    /// 入れ直している途中の、まだ読んでいないノートの ID（`queue_generation` の世代のもの。1 件ずつ読む）
    note_queue: VecDeque<String>,
    queue_generation: u64,
    /// 一覧を読めず、入れ直しを始められていない世代（読み直す間隔で読み直す）
    list_pending: Option<u64>,
    /// 読めなかった・書けなかったノート（読み直す間隔で、原本から読み直す。そのあいだは検索に出さない）
    note_retry: BTreeSet<String>,
}

/// 探すフォルダの行を、どの実体から読むかの記録（足したときの実体と、開いたフォルダの識別子）。
fn binding_of(place: &Place, root: FileId) -> Binding {
    Binding { place: place_key(place), root: format!("{}:{}", root.0, root.1) }
}

fn place_key(place: &Place) -> String {
    serde_json::to_string(place).unwrap_or_default()
}

/// ネットワークのドライブ（UNC のパス `\\server\share` か、ネットワークに割り当てたドライブ）か。
/// 変更の知らせが届かないことがあるので、見張らずに見直す。
fn is_network_path(path: &Path) -> bool {
    let text = path.to_string_lossy();
    if text.starts_with("\\\\") || text.starts_with("//") {
        return true;
    }
    #[cfg(windows)]
    if let Some(std::path::Component::Prefix(prefix)) = path.components().next() {
        use std::path::Prefix;
        match prefix.kind() {
            Prefix::UNC(..) | Prefix::VerbatimUNC(..) => return true,
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                let root: Vec<u16> = format!("{}:\\", letter as char).encode_utf16().chain(Some(0)).collect();
                // DRIVE_REMOTE（4）: ネットワークに割り当てたドライブ
                return unsafe { windows_sys::Win32::Storage::FileSystem::GetDriveTypeW(root.as_ptr()) } == 4;
            }
            _ => {}
        }
    }
    false
}

/// 索引を消し終えたか。「オフ」を知らせるのは、消し終えたときだけ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Erasure {
    /// 索引に中身があるかもしれない（起動した直後・索引を開いた・作った・作り直したあと）
    Unknown,
    /// 消し終えた（索引のファイルを消したか、中身を空にした）
    Erased,
    /// 消せなかった（あとで消し直す）
    Pending,
}

/// 索引に入れる中身を読む。クラウドにだけあるファイルは、ダウンロードさせないよう読まない（読む直前にも確かめる）。
/// 探すフォルダ `root` の中に、リンクをたどらずに届かなくなっていたら None（索引に入れない）。
fn read_content(root: &Root, file: &FileEntry, max_bytes: u64) -> Option<Extracted> {
    if file.online_only || is_online_only(&file.path) {
        return root.contains_file(&file.path).then(Extracted::name_only);
    }
    extract_within(root, &file.path, file.kind, max_bytes)
}

impl Worker {
    fn run(mut self, rx: Receiver<Message>) {
        let mut pending: BTreeSet<PathBuf> = BTreeSet::new();
        let mut pending_since: Option<Instant> = None;
        let mut watch_failed = false;
        // 最後に変更の知らせを受けた時刻（知らせが途切れて CHANGE_DEBOUNCE たったら反映する）
        let mut last_change = Instant::now();
        loop {
            let timeout = match pending_since {
                None if self.erasure == Erasure::Pending
                    || !self.unverified.is_empty()
                    || self.suspended
                    || !self.pending_removals.is_empty()
                    || self.notes_clear_pending
                    || !self.note_retry.is_empty()
                    || self.list_pending.is_some()
                    || self.reopen_pending =>
                {
                    self.intervals.root_check.min(self.intervals.poll).min(self.intervals.purge_retry)
                }
                None => self.intervals.root_check.min(self.intervals.poll),
                Some(since) => CHANGE_DEBOUNCE.saturating_sub(last_change.elapsed()).min(MAX_CHANGE_DELAY.saturating_sub(since.elapsed())),
            };
            // 入れ直しの途中のノートがあれば待たずに 1 件ずつ読む（届いた命令・知らせは、そのあいだにも受ける）
            let timeout = if self.note_queue.is_empty() { timeout } else { Duration::ZERO };
            let received = rx.recv_timeout(timeout);
            if matches!(received, Ok(Message::Rescan | Message::WatchFailed | Message::Changed(_))) {
                last_change = Instant::now();
            }
            let flush = match received {
                Ok(Message::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
                Ok(Message::Rescan) => {
                    pending.extend(self.folders().iter().map(|folder| PathBuf::from(&folder.path)));
                    pending_since.get_or_insert_with(Instant::now).elapsed() >= MAX_CHANGE_DELAY
                }
                Ok(Message::WatchFailed) => {
                    // 見張りが止まったことがある（Windows では、見張りに失敗すると自分で見張りを外す）。
                    // 反映するときに見張り直して、すべて調べ直す（失敗が続いても、見張り直すのは反映ごとに 1 回）
                    watch_failed = true;
                    pending.extend(self.folders().iter().map(|folder| PathBuf::from(&folder.path)));
                    pending_since.get_or_insert_with(Instant::now).elapsed() >= MAX_CHANGE_DELAY
                }
                Ok(Message::Changed(path)) => {
                    pending.insert(path);
                    // 知らせが続いても、最初の知らせから MAX_CHANGE_DELAY たったら反映する
                    pending_since.get_or_insert_with(Instant::now).elapsed() >= MAX_CHANGE_DELAY
                }
                Ok(Message::ResetNotes(generation)) => {
                    self.reset_notes(generation);
                    false
                }
                Ok(Message::NoteChanged(id)) => {
                    self.load_note(&id, None);
                    false
                }
                Ok(Message::RemoveNote(id)) => {
                    self.remove_note(id);
                    false
                }
                Ok(Message::Command(generation, command)) => {
                    // 命令は、どれもフォルダをすべて調べ直して見張り直すか、索引を消す
                    pending.clear();
                    pending_since = None;
                    watch_failed = false;
                    // オフにする・作り直す命令は、出したときに検索用の接続を止めている。処理を終えたら戻す
                    let release = matches!(command, Command::Disable | Command::Rebuild);
                    self.handle(generation, command);
                    if release {
                        self.unblock_reader();
                    }
                    false
                }
                // 知らせが途切れたか、最初の知らせから MAX_CHANGE_DELAY たったら反映する
                // （ノートを読むために待たずに戻ったときは、まだ反映しない）
                Err(RecvTimeoutError::Timeout) => pending_since
                    .is_none_or(|since| last_change.elapsed() >= CHANGE_DEBOUNCE || since.elapsed() >= MAX_CHANGE_DELAY),
            };
            if flush {
                pending_since = None;
                if std::mem::take(&mut watch_failed) {
                    self.reset_watcher();
                }
                let changed = std::mem::take(&mut pending);
                self.apply_changes(changed);
            }
            // オフのあいだに消せなかった索引・前回の起動から消去待ちの索引を、消し直す（消し終えて消去待ちを解くまで続ける）
            let unerased = self.erasure == Erasure::Pending || self.suspended;
            if unerased && !self.enabled() && self.last_purge.elapsed() >= self.intervals.purge_retry {
                let was = self.erasure;
                self.purge();
                if self.erasure == Erasure::Erased && was != Erasure::Erased {
                    // オンにする命令が先に出ていたら、知らせない（emit が、オンのあいだの「オフ」を捨てる）
                    let generation = self.generation();
                    self.emit(generation, IndexState::Off, (0, 0), true, None);
                }
            }
            if self.enabled() && self.suspended && self.last_retry.elapsed() >= self.intervals.purge_retry {
                // 検索から外せなかった索引を、消し直す（消せたら、すべてのフォルダを読み直す）
                self.last_retry = Instant::now();
                self.invalidate_all();
            }
            if (!self.pending_removals.is_empty() || self.notes_clear_pending || !self.note_retry.is_empty() || self.list_pending.is_some())
                && self.last_note_retry.elapsed() >= self.intervals.purge_retry
            {
                self.retry_notes();
            }
            if let Some(id) = self.note_queue.pop_front() {
                let generation = self.queue_generation;
                self.load_note(&id, Some(generation));
            }
            if self.enabled() && self.reopen_pending && self.last_reopen.elapsed() >= self.intervals.purge_retry {
                self.reopen();
            }
            if self.enabled() && std::mem::take(&mut self.rescan) {
                let all = self.folders().iter().map(|folder| PathBuf::from(&folder.path)).collect();
                self.apply_changes(all);
            }
            if self.enabled() && !self.unverified.is_empty() && self.last_retry.elapsed() >= self.intervals.purge_retry {
                // 出どころを確かめられない行を消し直す（フォルダが見つからなくても消す）。
                // 消せたフォルダは、一覧にあれば、すべて読み直す（消せなかったフォルダは、また消し直す）
                self.last_retry = Instant::now();
                let mut erased = BTreeSet::new();
                for folder in std::mem::take(&mut self.unverified) {
                    if self.erase_folder(&folder) {
                        erased.insert(folder);
                    }
                }
                self.apply_changes(erased);
            }
            if self.enabled() && self.last_root_check.elapsed() >= self.intervals.root_check {
                self.last_root_check = Instant::now();
                self.check_roots();
            }
            let poll = !self.polled.is_empty() || !self.incomplete.is_empty();
            if self.enabled() && poll && self.last_poll.elapsed() >= self.intervals.poll {
                self.last_poll = Instant::now();
                let folders: BTreeSet<PathBuf> = self.polled.iter().chain(self.incomplete.iter()).cloned().collect();
                self.apply_changes(folders);
            }
        }
    }

    fn enabled(&self) -> bool {
        self.shared.lock().unwrap().enabled
    }

    /// 相棒がオンなら、いまの世代（オンかと世代を同じ鍵の中で読む。あいだにオフにされても、新しい世代で作業しない）。
    fn enabled_generation(&self) -> Option<u64> {
        let shared = self.shared.lock().unwrap();
        shared.enabled.then_some(shared.generation)
    }

    /// 見張りを作り直し、すべてのフォルダを見張り直す（見張れないフォルダは、見直す一覧に入れる）。
    fn reset_watcher(&mut self) {
        if !self.enabled() {
            return;
        }
        let folders = self.folders();
        self.watch(&folders);
    }

    /// 探すフォルダそのものを確かめる。見張っているフォルダがなくなった・同じ名前で作り直された・
    /// 見つからなかったフォルダが戻った、のどれかなら反映する（ネットワークのドライブは、見直しの間隔で調べる）。
    fn check_roots(&mut self) {
        let mut changed = BTreeSet::new();
        for root in self.folders().iter().filter(|folder| folder.place.is_some()).map(|folder| PathBuf::from(&folder.path)) {
            if is_network_path(&root) {
                continue;
            }
            // フォルダそのものがリンクに替わった・上のフォルダが替わって別の場所を指すようになったときも、
            // 見つからないものとして扱う（リンクの先は見ない）
            let was_unplaced = self.unplaced.contains(&root);
            let opened = self.open_root(&root);
            let exists = opened.is_some();
            // 実際の場所を確かめられなくなった・確かめられるようになったときだけ反映する
            if was_unplaced || self.unplaced.contains(&root) {
                if exists || !was_unplaced {
                    changed.insert(root);
                }
                continue;
            }
            if self.missing.contains(&root) {
                if exists {
                    changed.insert(root);
                }
                continue;
            }
            if !exists {
                changed.insert(root);
                continue;
            }
            let Some(identity) = self.watched.get(&root).copied() else { continue };
            if opened.map(|opened| opened.id()) != Some(identity) {
                // 見張りは、移す前のフォルダに付いたまま。見張り直して、すべて調べ直す
                self.unwatch(&root);
                self.missing.insert(root.clone());
                if !self.polled.contains(&root) {
                    self.polled.push(root.clone());
                }
                changed.insert(root);
            }
        }
        self.apply_changes(changed);
    }

    fn unwatch(&mut self, root: &Path) {
        use notify::Watcher;
        if self.watched.remove(root).is_some() {
            if let Some(watcher) = self.watcher.as_mut() {
                let _ = watcher.unwatch(root);
            }
        }
    }

    fn folders(&self) -> Vec<SearchFolder> {
        self.shared.lock().unwrap().folders.clone()
    }

    fn generation(&self) -> u64 {
        self.shared.lock().unwrap().generation
    }

    /// この世代のあとに、新しい命令が出たか。
    fn stale(&self, generation: u64) -> bool {
        self.generation() != generation
    }

    fn open_index(&mut self) -> Option<&SearchIndex> {
        if self.index.is_none() {
            // 開くと、索引のファイルができる・前の中身が見える。消し終えたとはいえなくなる
            self.erasure = Erasure::Unknown;
            match SearchIndex::open(&self.index_path) {
                Ok(index) => self.index = Some(index),
                Err(err) => {
                    // 壊れていない索引は消さずに、あとで開き直す（[`SearchIndex::open`]）
                    eprintln!("資料の索引を開けません（あとで開き直します）: {err}");
                    self.reopen_pending = true;
                }
            }
        }
        self.index.as_ref()
    }

    fn handle(&mut self, generation: u64, command: Command) {
        match command {
            // あとの命令で取り消された一覧でも、外したフォルダ・実体が替わったフォルダの行は、検索から外しておく
            // （あとの命令が同じ実体に戻しても、そのあいだの行を使い回さない）
            Command::Enable(folders) | Command::SetFolders(folders) if self.stale(generation) => {
                if self.enabled() {
                    self.retire(&folders);
                }
            }
            Command::Rebuild if self.stale(generation) => {}
            // オフのあいだは、索引を作らない。一覧を変えた状態を知らせる（消し終えていなければ「消している」）
            Command::Enable(_) | Command::SetFolders(_) | Command::Rebuild if !self.enabled() => {
                // 消し終えていなければ消す（前回の起動の索引が残っているかもしれないときも、消してから「オフ」にする）
                if self.erasure != Erasure::Erased {
                    self.purge();
                }
                let state = if self.erasure == Erasure::Erased { IndexState::Off } else { IndexState::Erasing };
                self.emit(generation, state, (0, 0), true, None);
            }
            Command::Enable(folders) | Command::SetFolders(folders) => {
                let changes = self.changes;
                if self.suspended {
                    self.invalidate_all();
                }
                self.retire(&folders);
                self.rescan = false;
                self.build(generation, false, changes);
            }
            Command::Rebuild => {
                if !self.reset_index() {
                    self.invalidate_all();
                }
                self.rescan = false;
                self.build(generation, true, self.changes);
            }
            Command::Disable => {
                self.watcher = None;
                self.watched.clear();
                self.polled.clear();
                self.truncated.clear();
                self.missing.clear();
                self.incomplete.clear();
                self.unverified.clear();
                self.unplaced.clear();
                self.rescan = false;
                self.purge();
                // 消し終えてから「オフ」を知らせる。消せなかったときは、作成と見張りは止めたまま「消している」を知らせ、
                // 消し直せたときに「オフ」を知らせる
                let state = if self.erasure == Erasure::Erased { IndexState::Off } else { IndexState::Erasing };
                self.emit(generation, state, (0, 0), true, None);
            }
        }
    }

    /// 検索用の接続を閉じ、開かせない（索引を空にする・消す前に呼ぶ）。
    fn block_reader(&self) {
        let mut reader = self.reader.lock().unwrap();
        reader.index = None;
        reader.blocks += 1;
    }

    /// [`Worker::block_reader`] を 1 回分戻す。
    fn unblock_reader(&self) {
        let mut reader = self.reader.lock().unwrap();
        reader.blocks = reader.blocks.saturating_sub(1);
    }

    fn set_notes_stale(&self, stale: bool) {
        self.shared.lock().unwrap().notes_stale = stale;
    }

    /// ノートの索引が、入れ直し以外で空になった（索引を空にした・消した）。設定は変えず、ノートの索引を作っていれば、
    /// いまの世代のまま原本から入れ直す（オンかと世代は、同じ鍵の中で読む。あいだにオフにされたら入れ直さない）。
    fn notes_lost(&mut self) {
        let generation = {
            let shared = self.shared.lock().unwrap();
            (shared.enabled && shared.notes).then_some(shared.notes_generation)
        };
        if let Some(generation) = generation {
            self.list_notes(generation);
        }
    }

    /// 索引の中身がすべてなくなった（空にした・ファイルを消した）。消し残したノートの処理も済み、いまの世代の
    /// ノートの索引も空になったことになる。
    fn notes_erased(&mut self) {
        self.pending_removals.clear();
        self.notes_clear_pending = false;
        self.note_retry.clear();
        self.note_queue.clear();
        self.list_pending = None;
        let mut shared = self.shared.lock().unwrap();
        shared.notes_stale = false;
        shared.unsynced_notes.clear();
        shared.notes_cleared = shared.notes_cleared.max(shared.notes_generation);
    }

    /// ノートの索引を空にする（`generation` の入れ直しの始まり）。空にし終えたら、その世代の検索を許し、
    /// 原本からすべてのノートを入れ直す。空にできなければ、空にし直すまでノートを探さない。
    fn reset_notes(&mut self, generation: u64) {
        self.note_queue.clear();
        self.note_retry.clear();
        self.list_pending = None;
        self.shared.lock().unwrap().unsynced_notes.clear();
        if !self.enabled() && self.index.is_none() && !self.index_path.exists() {
            self.notes_erased();
            return;
        }
        match self.open_index().map(SearchIndex::clear_notes) {
            Some(Ok(())) => {
                self.notes_clear_pending = false;
                {
                    let mut shared = self.shared.lock().unwrap();
                    shared.notes_stale = false;
                    shared.notes_cleared = shared.notes_cleared.max(generation);
                }
                self.list_notes(generation);
            }
            other => {
                if let Some(Err(err)) = other {
                    eprintln!("過去のノートの索引を空にできません（あとで空にし直します）: {err}");
                }
                self.notes_clear_pending = true;
                self.set_notes_stale(true);
            }
        }
    }

    /// `generation` の入れ直しを始める: 原本からすべてのノートの ID を読み、1 件ずつ読む列に並べる
    /// （本文は、列から取り出すときに読む。すべての本文を一度に持たない）。
    /// 一覧を読めなければ、読み直す間隔で読み直す。世代が替わった・ノートの索引をやめたときは、何もしない。
    fn list_notes(&mut self, generation: u64) {
        let current = {
            let shared = self.shared.lock().unwrap();
            shared.enabled && shared.notes && shared.notes_generation == generation
        };
        if !current {
            self.list_pending = None;
            return;
        }
        let source = self.note_source.lock().unwrap().clone();
        match source.map(|source| source.note_ids()) {
            Some(Ok(ids)) => {
                self.note_queue = ids.into();
                self.queue_generation = generation;
                self.list_pending = None;
            }
            Some(Err(err)) => {
                eprintln!("過去のノートの一覧を読めません（あとで読み直します）: {err}");
                self.list_pending = Some(generation);
            }
            None => self.list_pending = Some(generation),
        }
    }

    /// ノートを原本から読み、索引に入れる。`generation` は入れ直しの世代（保存の知らせ・読み直しなら None）。
    /// 入れ直しが古い・ノートの索引を作っていない・削除した・録音中のノートは入れない。
    /// 原本で削除されていたら、索引からも消す。読めない・書けないときは、そのノートを検索から外し（索引の行が
    /// 古いかもしれない）、読み直す間隔で原本から読み直す。
    fn load_note(&mut self, note_id: &str, generation: Option<u64>) {
        let skip = {
            let shared = self.shared.lock().unwrap();
            !shared.enabled
                || !shared.notes
                || shared.removed_notes.contains(note_id)
                || generation.is_some_and(|g| g != shared.notes_generation)
                || shared.recording_note.as_deref() == Some(note_id)
        };
        if skip {
            return;
        }
        let source = self.note_source.lock().unwrap().clone();
        let loaded = match source {
            Some(source) => source.note(note_id),
            None => Err(crate::CoreError::Other("過去のノートの原本が決まっていません".into())),
        };
        let stored = match loaded {
            Ok(Some(doc)) => self.open_index().map(|index| index.upsert_note(&doc)),
            Ok(None) => {
                self.shared.lock().unwrap().removed_notes.insert(note_id.to_string());
                self.remove_note(note_id.to_string());
                return;
            }
            Err(err) => {
                eprintln!("過去のノートを読めません（あとで読み直します）: {err}");
                None
            }
        };
        if matches!(stored, Some(Ok(()))) {
            self.note_retry.remove(note_id);
            self.shared.lock().unwrap().unsynced_notes.remove(note_id);
        } else {
            if let Some(Err(err)) = stored {
                eprintln!("過去のノートを索引に入れられません（あとで入れ直します）: {err}");
            }
            self.note_retry.insert(note_id.to_string());
            self.shared.lock().unwrap().unsynced_notes.insert(note_id.to_string());
        }
    }

    /// 削除したノートを索引から消す。消せなければ、消し直す間隔で消し直す（そのあいだも検索には出さない）。
    fn remove_note(&mut self, note_id: String) {
        self.note_retry.remove(&note_id);
        self.note_queue.retain(|id| *id != note_id);
        self.shared.lock().unwrap().unsynced_notes.remove(&note_id);
        if self.index.is_none() && !self.index_path.exists() {
            return;
        }
        let removed = self.open_index().map(|index| index.remove_note(&note_id));
        if !matches!(removed, Some(Ok(()))) {
            if let Some(Err(err)) = removed {
                eprintln!("削除したノートを索引から消せません（あとで消し直します）: {err}");
            }
            self.pending_removals.insert(note_id);
        }
    }

    /// 読めなかったノートの一覧とノートを原本から読み直し、消せなかったノートと、空にできなかったノートの索引を、消し直す。
    fn retry_notes(&mut self) {
        self.last_note_retry = Instant::now();
        if let Some(generation) = self.list_pending {
            self.list_notes(generation);
        }
        for id in std::mem::take(&mut self.note_retry) {
            self.load_note(&id, None);
        }
        if self.pending_removals.is_empty() && !self.notes_clear_pending {
            return;
        }
        if self.index.is_none() && !self.index_path.exists() {
            self.notes_erased();
            return;
        }
        let pending = std::mem::take(&mut self.pending_removals);
        let clear = self.notes_clear_pending;
        let Some(index) = self.open_index() else {
            self.pending_removals = pending;
            return;
        };
        let left: BTreeSet<String> = pending.into_iter().filter(|id| index.remove_note(id).is_err()).collect();
        let cleared = clear && index.clear_notes().is_ok();
        self.pending_removals = left;
        if cleared {
            // 空にし直すあいだに入れたノートも消えたので、いまの世代のまま入れ直す
            self.notes_clear_pending = false;
            {
                let mut shared = self.shared.lock().unwrap();
                shared.notes_stale = false;
                shared.unsynced_notes.clear();
                shared.notes_cleared = shared.notes_cleared.max(shared.notes_generation);
            }
            self.note_retry.clear();
            self.notes_lost();
        }
    }

    /// 開けなかった索引を開き直す。開けたら、一覧にないフォルダ・実体が替わったフォルダの行を外し、すべて調べ直す。
    fn reopen(&mut self) {
        self.last_reopen = Instant::now();
        self.reopen_pending = false;
        if self.open_index().is_none() {
            return;
        }
        let folders = self.folders();
        self.retire(&folders);
        self.rescan = true;
    }

    /// 索引の中身を消し、ファイルも消す。消せなかったら、あとで消し直す。
    fn purge(&mut self) {
        self.block_reader();
        self.purge_files();
        self.unblock_reader();
        if self.erasure == Erasure::Erased {
            self.notes_erased();
            self.shared.lock().unwrap().hidden_folders.clear();
        }
    }

    fn purge_files(&mut self) {
        self.last_purge = Instant::now();
        // まず中身を消す（ファイルを消せないときにも、資料の中身がファイルに残らないように）
        let mut cleared = match self.index.take() {
            Some(index) => index.clear().is_ok() && index.close().is_ok(),
            None => false,
        };
        let removed = remove_index_files(&self.index_path);
        if let Err(err) = &removed {
            if !cleared && self.index_path.exists() {
                cleared = SearchIndex::open(&self.index_path).and_then(|index| {
                    index.clear()?;
                    index.close()
                }).is_ok();
            }
            if !cleared {
                eprintln!("資料の索引を消せません（あとで消し直します）: {err}");
            }
        }
        self.erasure = if removed.is_ok() || cleared { Erasure::Erased } else { Erasure::Pending };
        match self.erasure {
            Erasure::Erased if self.suspended => self.resume(),
            // 消し終えるまで検索を止める（記録のファイルで、起動し直しても、オンに戻しても続く）
            Erasure::Pending => self.suspend(),
            _ => {}
        }
    }

    /// 索引ごと消去待ちにして、検索を止める。記録のファイルを作れていなければ、作り直す
    /// （書き込めるようになったら、起動し直しても続くようにする）。
    fn suspend(&mut self) {
        if !self.suspended || !erase_marker(&self.index_path).exists() {
            if let Err(err) = suspend_search(&self.index_path) {
                eprintln!("資料の索引の消去待ちを記録できません（このあいだは検索しません）: {err}");
            }
            self.suspended = true;
        }
    }

    /// 「索引を作り直す」: 索引を空にする。空にできなければ、ファイルを消して作り直す。
    /// 空にできた・消せたら true（どちらもできなければ、それまでの行が残っている）。
    fn reset_index(&mut self) -> bool {
        self.block_reader();
        let reset = self.reset_index_files();
        self.unblock_reader();
        if reset {
            self.notes_erased();
            self.shared.lock().unwrap().hidden_folders.clear();
        }
        reset
    }

    fn reset_index_files(&mut self) -> bool {
        self.erasure = Erasure::Unknown;
        let cleared = self.open_index().map(|index| index.clear());
        if matches!(cleared, Some(Ok(()))) {
            return true;
        }
        if let Some(index) = self.index.take() {
            let _ = index.close();
        }
        match remove_index_files(&self.index_path) {
            Ok(()) => true,
            Err(err) => {
                eprintln!("資料の索引を空にできません: {err}");
                false
            }
        }
    }

    /// 索引の行を検索から外せないとき: 索引ごと消去待ちにして検索を止め（起動し直しても続く）、索引を空にする。
    /// 空にできたら消去待ちを解き、すべてのフォルダを読み直す。空にできなければ、消し直す間隔で消し直す。
    fn invalidate_all(&mut self) {
        self.suspend();
        if self.reset_index() {
            self.changes += 1;
            self.unverified.clear();
            self.rescan = true;
            self.resume();
            // 過去のノートの索引も空になったので、入れ直してもらう
            self.notes_lost();
        }
    }

    /// 索引を空にし終えたので、消去待ちを解いて検索を再開する（記録のファイルを消せなければ、消去待ちのまま消し直す）。
    fn resume(&mut self) {
        match resume_search(&self.index_path) {
            Ok(()) => self.suspended = false,
            Err(err) => eprintln!("資料の索引の消去待ちの記録を消せません（あとで消し直します）: {err}"),
        }
    }

    /// 一覧にないフォルダのファイルを、索引から消す。一覧にあっても、足したときの実体が索引の記録と違う
    /// （選び直した・実体を記録していない）フォルダの行は、出どころを確かめられないので、調べる前に検索から外す。
    /// 世代が替わって調べる前にやめても、検索には出ない（行は、そのフォルダを調べるときに消す）。
    fn retire(&mut self, folders: &[SearchFolder]) {
        let listed: HashMap<&str, Option<String>> =
            folders.iter().map(|f| (f.path.as_str(), f.place.as_ref().map(place_key))).collect();
        let is_listed = |p: &PathBuf| listed.contains_key(p.to_string_lossy().as_ref());
        self.truncated.retain(is_listed);
        self.missing.retain(is_listed);
        self.incomplete.retain(is_listed);
        self.unplaced.retain(is_listed);
        let Some(index) = self.open_index() else {
            // 索引を開けず、どのフォルダの行があるかを確かめられない。ファイルは消さず、開き直してから外す
            // （検索は、いま登録しているフォルダと実体が記録と一致する行だけを探すので、外したフォルダの行は出ない）
            return;
        };
        let folders = match index.folders() {
            Ok(folders) => folders,
            Err(err) => {
                eprintln!("資料の索引のフォルダを確かめられません（索引を作り直します）: {err}");
                self.invalidate_all();
                return;
            }
        };
        let mut stale = Vec::new();
        for folder in folders {
            let keep = match listed.get(folder.to_string_lossy().as_ref()) {
                None => false,
                // 記録が読めないときも、確かめられないものとして消す。記録のない行（検索に出さない行）も、ここで消す
                Some(place) => match index.binding(&folder) {
                    Ok(Some(binding)) => Some(&binding.place) == place.as_ref(),
                    Ok(None) => matches!(index.stored_rows(&folder), Ok(0)),
                    Err(_) => false,
                },
            };
            if !keep {
                stale.push(folder);
            }
        }
        for folder in stale {
            self.erase_folder(&folder);
        }
    }

    /// フォルダの行と記録を、索引から消す。消せなければ、検索から外して、あとで消し直す。
    /// 消せたら true。
    fn erase_folder(&mut self, folder: &Path) -> bool {
        let Some(index) = self.open_index() else {
            // 索引を開けない（壊れてはいない）。ファイルは消さず、消し終えるまで検索に出さずに、あとで消し直す
            self.shared.lock().unwrap().hidden_folders.insert(folder.to_path_buf());
            self.unverified.insert(folder.to_path_buf());
            return false;
        };
        // 行も記録もないと確かめられたときだけ、消すものがないとする（読めなければ消しにいく）
        if matches!(index.stored_rows(folder), Ok(0)) && matches!(index.binding(folder), Ok(None)) {
            self.unverified.remove(folder);
            self.shared.lock().unwrap().hidden_folders.remove(folder);
            return true;
        }
        match index.remove_folder(folder) {
            Ok(()) => {
                self.unverified.remove(folder);
                self.shared.lock().unwrap().hidden_folders.remove(folder);
                self.changes += 1;
                true
            }
            Err(err) => {
                eprintln!("資料の索引から、フォルダの行を消せません（あとで消し直します）: {err}");
                self.hide(folder);
                self.unverified.insert(folder.to_path_buf());
                false
            }
        }
    }

    /// フォルダの行を、出どころを確かめられないものにする（検索に出さない）。
    /// 索引に書けないときは、索引ごと消去待ちにして検索を止め、索引を空にする（確かめられない行を、検索に残さない）。
    fn hide(&mut self, folder: &Path) {
        let Some(index) = self.open_index() else {
            self.invalidate_all();
            return;
        };
        if index.binding(folder).is_ok_and(|binding| binding.is_none()) {
            return;
        }
        if let Err(err) = index.unbind(folder) {
            eprintln!("資料の索引の行を、検索から外せません（索引ごと消し直します）: {err}");
            self.invalidate_all();
        }
    }

    /// 探すフォルダの行が、足したときの実体 `place` の、開いたフォルダ `root` から読んだものだと記録されているか。
    fn is_bound(&self, folder: &Path, place: &Place, root: FileId) -> bool {
        let Some(index) = self.index.as_ref() else { return false };
        index.binding(folder).ok().flatten() == Some(binding_of(place, root))
    }

    /// すべてのフォルダを調べ、変わったファイルだけを読み直す（`all` なら、すべて読み直す）。
    /// `changes` は、この反映を始める前に索引に書いた回数（一覧から外したフォルダの削除も、変わったものに数える）。
    fn build(&mut self, generation: u64, all: bool, changes: u64) {
        let folders = self.folders();
        self.emit(generation, IndexState::Building, (0, 0), true, None);
        // 調べ始める前に見張る（調べ終えたフォルダで、見張り始めるまでに起きた変更も、知らせで反映する）
        self.watch(&folders);
        let mut done = 0;
        let mut total = 0;
        for folder in &folders {
            if self.stale(generation) {
                return;
            }
            let root = PathBuf::from(&folder.path);
            if !self.scan_folder(&root, all, generation, &mut done, &mut total) {
                return;
            }
        }
        if self.stale(generation) {
            return;
        }
        // 索引が変わったときだけ、更新した時刻を進める（作り直しと、初めて作ったときは、変わらなくても進める）
        let first = self.shared.lock().unwrap().status.updated_at.is_none();
        let updated_at = (all || first || self.changes != changes).then(now_ms);
        self.emit(generation, IndexState::Ready, (0, 0), true, updated_at);
    }

    /// 1 つのフォルダを調べて索引に反映する。途中でやめたら false。
    fn scan_folder(&mut self, root: &Path, all: bool, generation: u64, done: &mut usize, total: &mut usize) -> bool {
        let Some(place) = self.place_of(root) else {
            // 足したときの実体を記録していない（実体を記録する前の設定）。どこを読んだか確かめられないので、
            // それまでの索引も消し（消せなければ検索から外して消し直し）、選び直すまで読まない
            self.missing.remove(root);
            self.truncated.remove(root);
            self.incomplete.remove(root);
            self.erase_folder(root);
            return true;
        };
        let Some(opened) = self.open_root(root) else {
            if self.unplaced.contains(root) {
                self.set_aside_unplaced(root);
                return true;
            }
            // 見つからないとき・別の場所を指すようになったときは、索引を消さずに残す
            self.missing.insert(root.to_path_buf());
            return true;
        };
        // 索引の行を、選び直す前の実体・同じ場所で作り直す前のフォルダから読んでいた（記録がない・違う）なら、
        // 名前・大きさ・日時が同じファイルも使い回さない。それまでの行を消してから、すべて読み直す
        // （記録は索引にあるので、世代が替わっても、アプリを起動し直しても同じ）。消せなければ読まずに、あとで消し直す
        if self.open_index().is_none() {
            return true;
        }
        if !self.is_bound(root, &place, opened.id()) {
            let Some(index) = self.index.as_ref() else { return true };
            let rows = index.stored_rows(root).unwrap_or(0);
            match index.rebind(root, &binding_of(&place, opened.id())) {
                Ok(()) => {
                    self.unverified.remove(root);
                    if rows > 0 {
                        self.changes += 1;
                    }
                }
                Err(err) => {
                    eprintln!("資料の索引から、確かめられない行を消せません（あとで消し直します）: {err}");
                    self.hide(root);
                    self.unverified.insert(root.to_path_buf());
                    return true;
                }
            }
        }
        let walked = opened.walk(self.limits);
        self.missing.remove(root);
        if walked.unreadable.is_empty() {
            self.incomplete.remove(root);
        } else {
            self.incomplete.insert(root.to_path_buf());
        }
        if walked.truncated {
            self.truncated.insert(root.to_path_buf());
        } else {
            self.truncated.remove(root);
        }
        *total += walked.files.len();
        let Some(index) = self.index.as_ref() else { return true };
        let known = index.known_files(root).unwrap_or_default();
        let found: HashSet<&PathBuf> = walked.files.iter().map(|f| &f.path).collect();
        // 中を読めなかったフォルダのファイルは、消えたとみなさない（読めるようになったら調べ直す）
        let kept = |path: &Path| walked.unreadable.iter().any(|dir| path.starts_with(dir));
        for path in known.keys().filter(|p| !found.contains(p) && !kept(p)) {
            if self.stale(generation) {
                return false;
            }
            let _ = index.remove_path(path);
            self.changes += 1;
        }
        for file in &walked.files {
            if self.stale(generation) {
                return false;
            }
            let unchanged = known.get(&file.path) == Some(&stamp_of_file(file));
            if all || !unchanged {
                let content = read_content(&opened, file, self.limits.max_bytes);
                if let Some(index) = self.index.as_ref() {
                    match &content {
                        Some(content) => {
                            let _ = index.upsert(root, file, content);
                        }
                        // 調べたあとで、フォルダの範囲の外に替わった
                        None => {
                            let _ = index.remove_path(&file.path);
                        }
                    }
                }
                self.changes += 1;
            }
            *done += 1;
            self.emit(generation, IndexState::Building, (*done, *total), false, None);
        }
        true
    }

    /// 一覧のフォルダの、足したときの実体（記録していなければ None）。
    fn place_of(&self, root: &Path) -> Option<Place> {
        self.shared.lock().unwrap().folders.iter().find(|folder| Path::new(&folder.path) == root).and_then(|folder| folder.place.clone())
    }

    /// 探すフォルダを開き、足したときの実体と同じフォルダか（同じ場所で作り直したものを含む）を確かめる。
    /// 上のフォルダがリンクに替わり、別の場所の同じ名前のフォルダを指すようになったら None（見つからないものとして扱う）。
    /// 足したときの実体を記録していないフォルダも開かない。
    /// 開いたときに実際の場所が取れなければ、同じ場所かを確かめられないので None にし、選び直してもらう（`unplaced`）。
    fn open_root(&mut self, root: &Path) -> Option<Root> {
        let place = self.place_of(root)?;
        let opened = Root::open(root).ok();
        if opened.as_ref().is_some_and(|opened| opened.real().is_none()) {
            self.unplaced.insert(root.to_path_buf());
            return None;
        }
        self.unplaced.remove(root);
        let opened = opened?;
        place.admits(&opened).then_some(opened)
    }

    /// 実際の場所を確かめられなかったフォルダ: 中身を読まず、それまでの行も消す。確かめられるようになったら読み直す。
    fn set_aside_unplaced(&mut self, root: &Path) {
        self.missing.remove(root);
        self.truncated.remove(root);
        self.incomplete.remove(root);
        self.erase_folder(root);
        if !self.polled.iter().any(|polled| polled == root) {
            self.polled.push(root.to_path_buf());
        }
    }

    /// 見張りの知らせ・見直しで届いたパスを、それを含むフォルダごとに反映する。
    fn apply_changes(&mut self, changed: BTreeSet<PathBuf>) {
        if changed.is_empty() {
            return;
        }
        let Some(generation) = self.enabled_generation() else { return };
        let folders = self.folders();
        let mut roots: BTreeSet<PathBuf> = BTreeSet::new();
        for path in &changed {
            if let Some(folder) = folders.iter().map(|f| PathBuf::from(&f.path)).find(|root| path.starts_with(root)) {
                roots.insert(folder);
            }
        }
        if roots.is_empty() {
            return;
        }
        let (mut done, mut total) = (0, 0);
        let changes = self.changes;
        // 戻ったフォルダは、調べ始める前に見張る（調べているあいだの変更も、知らせで反映する）。
        // 見張れるようになったフォルダは、見張っていないあいだの変更を拾うため、すべて調べ直す
        let rewatched = self.rewatch();
        roots.extend(rewatched.iter().cloned());
        let mut changed = changed;
        changed.extend(rewatched);
        loop {
            while let Some(root) = roots.pop_first() {
                if self.stale(generation) {
                    return;
                }
                let Some(place) = self.place_of(&root) else {
                    // 足したときの実体を記録していない（選び直すまで読まない）。残っている行があれば消す
                    self.erase_folder(&root);
                    continue;
                };
                let Some(opened) = self.open_root(&root) else {
                    if self.unplaced.contains(&root) {
                        self.unwatch(&root);
                        self.set_aside_unplaced(&root);
                        continue;
                    }
                    // ドライブが外れた・フォルダが移った・フォルダそのものがリンクに替わった。
                    // 1 つずつのファイルを反映する前に見張りを外し、それまでの索引は残して、戻るまで見直す
                    self.unwatch(&root);
                    self.missing.insert(root.clone());
                    if !self.polled.contains(&root) {
                        self.polled.push(root);
                    }
                    continue;
                };
                // 見つからなかったフォルダが戻ったときは、すべて調べ直す
                let returned = self.missing.contains(&root);
                // 変わったファイルが分かっているときは、そのファイルだけを読み直す（フォルダそのものなら、すべて調べ直す）。
                // 索引の行を、いまの実体から読んだと記録していなければ、フォルダごと調べ直す
                self.open_index();
                let whole = returned || changed.contains(&root) || !self.is_bound(&root, &place, opened.id());
                let single: Vec<&PathBuf> = changed.iter().filter(|p| p.starts_with(&root) && *p != &root).collect();
                let mut handled = !whole && !single.is_empty();
                for path in &single {
                    if self.stale(generation) {
                        return;
                    }
                    if !self.apply_file(&opened, path) {
                        handled = false;
                        break;
                    }
                }
                drop(opened);
                if !handled && !self.scan_folder(&root, false, generation, &mut done, &mut total) {
                    return;
                }
            }
            if self.stale(generation) {
                return;
            }
            // 調べているあいだに見張れるようになったフォルダも、すべて調べ直す
            let rewatched = self.rewatch();
            if rewatched.is_empty() {
                break;
            }
            roots.extend(rewatched.iter().cloned());
            changed.extend(rewatched);
        }
        // 索引が変わったときだけ、更新した時刻を進める
        let updated_at = (self.changes != changes).then(now_ms);
        self.emit(generation, IndexState::Ready, (0, 0), true, updated_at);
    }

    /// 1 つのファイルの変更を反映する。フォルダの変更など、ファイル 1 つでは決められないときは false。
    fn apply_file(&mut self, opened: &Root, path: &Path) -> bool {
        let limits = self.limits;
        let root = opened.path();
        if self.open_index().is_none() {
            return true;
        }
        let Some(index) = self.index.as_ref() else { return true };
        match opened.stat(path) {
            Ok(meta) if meta.is_dir() => false,
            // 消えた（またはフォルダごと消えた）。ファイルなら行を消し、フォルダならたどり直す
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => match index.contains(path) {
                Ok(true) => {
                    let _ = index.remove_path(path);
                    self.changes += 1;
                    true
                }
                _ => false,
            },
            // 調べられない（読み取りの権限がないなど）。消えたとはみなさず、フォルダごと調べ直す
            Err(err) if !is_outside(&err) => false,
            // ファイル、または範囲の外（途中のフォルダがリンクに替わったなど。索引に入れず、あれば消す）
            Ok(_) | Err(_) => {
                let Some(file) = opened.inspect(path, limits) else {
                    if index.contains(path).unwrap_or(false) {
                        let _ = index.remove_path(path);
                        self.changes += 1;
                    }
                    return true;
                };
                let known = index.stamp(&file.path).unwrap_or_default();
                if known == Some(stamp_of_file(&file)) {
                    return true;
                }
                if known.is_none() && index.count_folder(root).unwrap_or(0) >= limits.max_files {
                    // 上限に達しているので入れない（一部だけ索引に入れている、と表示する）
                    self.truncated.insert(root.to_path_buf());
                    return true;
                }
                let content = read_content(opened, &file, limits.max_bytes);
                if let Some(index) = self.index.as_ref() {
                    match &content {
                        Some(content) => {
                            let _ = index.upsert(root, &file, content);
                        }
                        None => {
                            let _ = index.remove_path(&file.path);
                        }
                    }
                }
                self.changes += 1;
                true
            }
        }
    }

    /// フォルダの変更を見張る。見張れないフォルダは、見直す一覧に入れる。
    fn watch(&mut self, folders: &[SearchFolder]) {
        self.polled.clear();
        let tx = self.tx.clone();
        let handler = move |event: notify::Result<notify::Event>| {
            for message in watch_messages(event) {
                let _ = tx.send(message);
            }
        };
        // フォルダの中のリンクの先は見張らない（範囲の外の変更を受けない）
        let config = notify::Config::default().with_follow_symlinks(false);
        self.watcher = <notify::RecommendedWatcher as notify::Watcher>::new(handler, config).ok();
        self.watched.clear();
        // 足したときの実体を記録していないフォルダは、見張らず、見直さない（選び直すまで読まない）
        self.polled = folders.iter().filter(|folder| folder.place.is_some()).map(|folder| PathBuf::from(&folder.path)).collect();
        // 調べ始める前なので、見張りに移したフォルダは、このあとすべて調べる
        let _ = self.rewatch();
        self.last_poll = Instant::now();
    }

    /// 見直す一覧のフォルダのうち、見張れるようになったもの（ドライブが戻ったなど）を、見張りに移す。
    /// 見張りに移したフォルダを返す（見張っていないあいだの変更を拾うため、呼び出し側が調べ直す）。
    fn rewatch(&mut self) -> Vec<PathBuf> {
        use notify::{RecursiveMode, Watcher};
        if self.watcher.is_none() {
            return Vec::new();
        }
        let mut moved = Vec::new();
        for root in std::mem::take(&mut self.polled) {
            // リンクに替わったフォルダ・別の場所を指すようになったフォルダは見張らない
            let opened = if is_network_path(&root) { None } else { self.open_root(&root) };
            let watched = opened.is_some_and(|opened| {
                let Some(watcher) = self.watcher.as_mut() else { return false };
                if watcher.watch(&root, RecursiveMode::Recursive).is_err() {
                    return false;
                }
                self.watched.insert(root.clone(), opened.id());
                true
            });
            if watched {
                moved.push(root);
            } else {
                self.polled.push(root);
            }
        }
        moved
    }

    /// 状態を更新して知らせる。`force` でなければ、0.3 秒に 1 回に間引く。
    /// `generation` のあとに新しい命令が出ていたら、古い状態なので知らせない。
    /// `updated_at` を渡したら、最後に更新し終えた時刻をそれにする。
    fn emit(&mut self, generation: u64, state: IndexState, progress: (usize, usize), force: bool, updated_at: Option<i64>) {
        if !force && self.last_emit.elapsed() < Duration::from_millis(300) {
            return;
        }
        // 「オフ」は、索引を消し終えたときだけ知らせる（消せていないあいだは「消している」のまま）
        if state == IndexState::Off && self.erasure != Erasure::Erased {
            return;
        }
        let folders = self.folders();
        // 「作っている」「できている」はオンのあいだだけ、「消している」「オフ」はオフのあいだだけ知らせる（鍵の中で確かめる）
        let enabled = matches!(state, IndexState::Building | IndexState::Ready);
        let current = |shared: &Shared| shared.generation == generation && shared.enabled == enabled;
        // 古い・合わない状態のために、索引を開かない（オフのあいだに索引のファイルを作らない）
        if !current(&self.shared.lock().unwrap()) {
            return;
        }
        if enabled {
            self.open_index();
        }
        let index = if enabled { self.index.as_ref() } else { None };
        let rows: Vec<FolderStatus> = folders
            .iter()
            .map(|folder| {
                let root = PathBuf::from(&folder.path);
                FolderStatus {
                    path: folder.path.clone(),
                    no_content: folder.no_content,
                    files: index.and_then(|i| i.count_folder(&root).ok()).unwrap_or(0),
                    missing: enabled && self.missing.contains(&root),
                    truncated: enabled && self.truncated.contains(&root),
                    unconfirmed: folder.place.is_none() || (enabled && self.unplaced.contains(&root)),
                }
            })
            .collect();
        let files = rows.iter().map(|r| r.files).sum();
        let _order = self.order.lock().unwrap();
        let status = {
            let mut shared = self.shared.lock().unwrap();
            if !current(&shared) {
                return;
            }
            // 調べているあいだに「中身を送らない」が変わっていたら、いまの値にする
            let rows = rows
                .into_iter()
                .map(|row| {
                    let no_content = shared.folders.iter().find(|f| f.path == row.path).map_or(row.no_content, |f| f.no_content);
                    FolderStatus { no_content, ..row }
                })
                .collect();
            let status = IndexStatus {
                state,
                done: progress.0,
                total: progress.1,
                files,
                // オフのあいだは索引を持たないので、更新した時刻も持たない
                updated_at: if enabled { updated_at.or(shared.status.updated_at) } else { None },
                folders: rows,
                revision: 0,
            };
            shared.set_status(status);
            shared.status.clone()
        };
        self.last_emit = Instant::now();
        (self.on_status)(&status);
    }
}

/// 見張りの知らせを、作業用のスレッドへの知らせにする。取りこぼしがあったとき（知らせがあふれたなど）・
/// どのパスか分からないときは、すべて調べ直す。見張りに失敗したときは、見張り直してから、すべて調べ直す。
fn watch_messages(event: notify::Result<notify::Event>) -> Vec<Message> {
    match event {
        Ok(event) if !event.need_rescan() && !event.paths.is_empty() => event.paths.into_iter().map(Message::Changed).collect(),
        Ok(_) => vec![Message::Rescan],
        Err(_) => vec![Message::WatchFailed],
    }
}

/// 一覧の行を、いまのフォルダの並びに合わせる（作業用のスレッドが調べ終えるまでは、前の件数のまま）。
/// 足したフォルダや外したフォルダが、コマンドの戻り値にすぐ表れるようにする。
fn list_folders(shared: &mut Shared) {
    let mut status = shared.status.clone();
    // 相棒がオンなら、作業用のスレッドが調べ始める前から「作っている」にする（戻り値が、処理の進み方で変わらないように）。
    // オフにした直後は、作業用のスレッドが索引を消し終えて「オフ」にするまで、前の状態のまま
    if shared.enabled {
        status.state = IndexState::Building;
        status.done = 0;
        status.total = 0;
    }
    let previous = std::mem::take(&mut status.folders);
    status.folders = shared
        .folders
        .iter()
        .map(|folder| {
            let before = previous.iter().find(|row| row.path == folder.path);
            FolderStatus {
                path: folder.path.clone(),
                no_content: folder.no_content,
                files: before.map_or(0, |row| row.files),
                missing: before.is_some_and(|row| row.missing),
                truncated: before.is_some_and(|row| row.truncated),
                unconfirmed: folder.place.is_none(),
            }
        })
        .collect();
    status.files = status.folders.iter().map(|row| row.files).sum();
    shared.set_status(status);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(messages: Vec<Message>) -> Vec<String> {
        messages
            .into_iter()
            .map(|message| match message {
                Message::Changed(path) => path.to_string_lossy().to_string(),
                Message::Rescan => "調べ直す".into(),
                Message::WatchFailed => "見張り直す".into(),
                _ => "ほか".into(),
            })
            .collect()
    }

    // [IDX-009]
    #[test]
    fn watch_errors_and_dropped_notifications_rescan_every_folder() {
        use notify::event::{EventKind, Flag};
        let changed = notify::Event::new(EventKind::Any).add_path(PathBuf::from("a.txt"));
        assert_eq!(kinds(watch_messages(Ok(changed))), ["a.txt"]);
        let overflowed = notify::Event::new(EventKind::Other).set_flag(Flag::Rescan);
        assert_eq!(kinds(watch_messages(Ok(overflowed))), ["調べ直す"], "知らせがあふれた");
        assert_eq!(kinds(watch_messages(Ok(notify::Event::new(EventKind::Any)))), ["調べ直す"], "どのパスか分からない");
        assert_eq!(kinds(watch_messages(Err(notify::Error::generic("失敗")))), ["見張り直す"], "見張りに失敗した");
    }

    fn worker(index_path: PathBuf, shared: Arc<Mutex<Shared>>, seen: Arc<Mutex<Vec<IndexState>>>) -> Worker {
        let (tx, _rx) = channel();
        let on_status: StatusCallback = Arc::new(move |status: &IndexStatus| seen.lock().unwrap().push(status.state));
        Worker::new(index_path, Limits::default(), Intervals::default(), shared, Arc::new(Mutex::new(())), on_status, tx)
    }

    const INDEX_FILE_FOR_TEST: &str = "search-index.db";

    fn wait_for(indexer: &Indexer, what: &str, check: impl Fn(&IndexStatus) -> bool) {
        let start = Instant::now();
        while !check(&indexer.status()) {
            assert!(start.elapsed() < Duration::from_secs(20), "{what} になりませんでした: {:?}", indexer.status());
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// 開いたときに実際の場所が取れず、足したときと同じ場所かを確かめられないフォルダは、識別子と作成日時が同じでも
    /// 中身を読まない。それまでの索引も消し、「確認が必要」にする（選び直しても、確かめられなければ足せない）。
    /// 実際の場所を確かめられるようになったら、読み直す。
    // [IDX-027]
    #[test]
    fn a_folder_whose_real_place_cannot_be_confirmed_is_not_read() {
        use super::super::walk::test_hooks;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("資料");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "経費").unwrap();
        let index_path = dir.path().join("data").join(INDEX_FILE_FOR_TEST);
        let intervals =
            Intervals { root_check: Duration::from_millis(100), poll: Duration::from_secs(3600), purge_retry: Duration::from_millis(100) };
        let indexer = Indexer::spawn_with_intervals(index_path.clone(), Limits::default(), intervals, |_| {});
        let registered = SearchFolder::register(&root).unwrap();
        indexer.enable(vec![registered.clone()]);
        wait_for(&indexer, "索引", |s| s.state == IndexState::Ready && s.files == 1);
        let found = |term: &str| SearchIndex::open(&index_path).unwrap().search(&[term.to_string()], 8).unwrap().len();
        let stored = || SearchIndex::open(&index_path).unwrap().stored_rows(&root).unwrap();
        assert_eq!(found("経費"), 1);

        test_hooks::hide_real(&root, true);
        std::fs::write(root.join("b.txt"), "予算").unwrap();
        wait_for(&indexer, "確認が必要な表示", |s| {
            s.state == IndexState::Ready && s.files == 0 && s.folders[0].unconfirmed && !s.folders[0].missing
        });
        assert_eq!((found("経費"), found("予算"), stored()), (0, 0, 0), "中身を読まず、それまでの行も消す");
        assert!(SearchFolder::register(&root).is_err(), "確かめられないフォルダは、選び直しても足せない");
        let rebuilt = indexer.rebuild().unwrap();
        wait_for(&indexer, "作り直し", |s| s.state == IndexState::Ready && s.revision > rebuilt.revision);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!((found("予算"), stored()), (0, 0), "作り直しても読まない");
        assert!(indexer.status().folders[0].unconfirmed);

        test_hooks::hide_real(&root, false);
        wait_for(&indexer, "読み直し", |s| s.state == IndexState::Ready && s.files == 2 && !s.folders[0].unconfirmed);
        assert_eq!(found("予算"), 1);
        assert!(indexer.shutdown_and_wait(Duration::from_secs(5)));
    }

    /// オフにしたときに索引を消せなければ、すぐに検索を止め、消去待ちを記録する（起動し直しても続く）。
    /// 消し終える前にオンに戻しても、消去待ちのまま消し直し、消し終えてから検索を再開して、いまの中身を読み直す。
    // [IDX-028]
    #[test]
    fn an_index_that_could_not_be_erased_when_turned_off_stays_out_of_search() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("資料");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "経費").unwrap();
        let folders = vec![SearchFolder::register(&root).unwrap()];
        // 索引のファイルの場所に、中身のあるフォルダがある（消すことも、空にすることもできない）
        let index_path = dir.path().join("data").join(INDEX_FILE_FOR_TEST);
        std::fs::create_dir_all(index_path.join("中")).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let state = shared(false, 1);
        state.lock().unwrap().folders = folders.clone();
        let mut first = worker(index_path.clone(), state.clone(), seen.clone());

        first.handle(1, Command::Disable);
        assert_eq!(first.erasure, Erasure::Pending);
        assert!(first.suspended && is_search_suspended(&index_path), "消せなければ、すぐに検索を止める");
        assert!(erase_marker(&index_path).exists(), "消去待ちを記録する");
        drop(first);

        // 起動し直しても、消去待ちのまま
        let mut worker = worker(index_path.clone(), state.clone(), seen.clone());
        assert!(worker.suspended, "記録から、消去待ちを引き継ぐ");

        // 消し終える前にオンに戻しても、消去待ちのまま
        set(&state, true, 2);
        worker.handle(2, Command::Enable(folders.clone()));
        assert!(worker.suspended && erase_marker(&index_path).exists(), "オンに戻しても、消し終えるまで検索しない");

        // 消せるようになったら消し直し、消去待ちを解いて、いまの中身を読み直す
        std::fs::remove_dir_all(&index_path).unwrap();
        worker.invalidate_all();
        assert!(!worker.suspended && !erase_marker(&index_path).exists(), "消し終えたら、消去待ちを解く");
        assert!(worker.rescan, "すべてのフォルダを読み直す");
        let all = folders.iter().map(|folder| PathBuf::from(&folder.path)).collect();
        worker.apply_changes(all);
        let hits = SearchIndex::open(&index_path).unwrap().search(&["経費".to_string()], 8).unwrap();
        assert_eq!(hits.len(), 1);
    }

    /// オフのまま起動したときも、前回の起動から消去待ちの索引は、命令を待たずに消し、消去待ちを解く。
    // [IDX-028]
    #[test]
    fn an_index_waiting_to_be_erased_is_erased_after_starting_while_off() {
        let dir = tempfile::tempdir().unwrap();
        let index_path = dir.path().join("data").join(INDEX_FILE_FOR_TEST);
        {
            let index = SearchIndex::open(&index_path).unwrap();
            index.close().unwrap();
        }
        std::fs::write(erase_marker(&index_path), "erase\n").unwrap();
        let intervals =
            Intervals { root_check: Duration::from_millis(100), poll: Duration::from_secs(3600), purge_retry: Duration::from_millis(100) };
        let indexer = Indexer::spawn_with_intervals(index_path.clone(), Limits::default(), intervals, |_| {});
        let start = Instant::now();
        while erase_marker(&index_path).exists() || index_path.exists() {
            assert!(start.elapsed() < Duration::from_secs(20), "消去待ちの索引を消していません");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(indexer.status().state, IndexState::Off);
        assert!(indexer.shutdown_and_wait(Duration::from_secs(5)));
    }

    fn shared(enabled: bool, generation: u64) -> Arc<Mutex<Shared>> {
        Arc::new(Mutex::new(Shared {
            status: IndexStatus { state: IndexState::Erasing, done: 0, total: 0, files: 0, updated_at: None, folders: Vec::new(), revision: 0 },
            enabled,
            notes: false,
            notes_generation: 0,
            removed_notes: HashSet::new(),
            recording_note: None,
            notes_stale: false,
            notes_cleared: 0,
            unsynced_notes: HashSet::new(),
            hidden_folders: HashSet::new(),
            folders: Vec::new(),
            generation,
        }))
    }

    /// テスト用のノートの原本（一覧やノートを、読めなくすることもできる）。
    #[derive(Default)]
    struct MemoryNotes {
        notes: Mutex<Vec<NoteDoc>>,
        failing: std::sync::atomic::AtomicBool,
    }

    impl MemoryNotes {
        fn with(notes: Vec<NoteDoc>) -> Arc<Self> {
            Arc::new(Self { notes: Mutex::new(notes), ..Default::default() })
        }

        fn fail(&self, failing: bool) {
            self.failing.store(failing, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl NoteSource for MemoryNotes {
        fn note_ids(&self) -> crate::error::Result<Vec<String>> {
            if self.failing.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(crate::CoreError::Other("読めません".into()));
            }
            Ok(self.notes.lock().unwrap().iter().map(|n| n.note_id.clone()).collect())
        }

        fn note(&self, note_id: &str) -> crate::error::Result<Option<NoteDoc>> {
            if self.failing.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(crate::CoreError::Other("読めません".into()));
            }
            Ok(self.notes.lock().unwrap().iter().find(|n| n.note_id == note_id).cloned())
        }
    }

    fn note_doc(id: &str, memo: &str) -> NoteDoc {
        NoteDoc { note_id: id.into(), title: "定例".into(), created_at: 1, memo: memo.into(), minutes: String::new(), transcript: String::new() }
    }

    fn wait_until(what: &str, check: impl Fn() -> bool) {
        let start = Instant::now();
        while !check() {
            assert!(start.elapsed() < Duration::from_secs(20), "{what} になりませんでした");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// オンかと世代を別々に読むと、あいだにオフにされたとき、新しい世代で消す前の「オフ」を知らせてしまう。
    /// 知らせる状態は、鍵の中で、いまのオンとオフに合うものだけにする。
    // [IDX-028]
    #[test]
    fn statuses_that_do_not_match_on_or_off_are_not_reported() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let dir = tempfile::tempdir().unwrap();
        // オフにした命令（世代 5）のあと。索引は消し終えている
        let state = shared(false, 5);
        let mut worker = worker(dir.path().join(INDEX_FILE_FOR_TEST), state.clone(), seen.clone());
        worker.erasure = Erasure::Erased;
        worker.emit(5, IndexState::Ready, (0, 0), true, None);
        worker.emit(5, IndexState::Building, (0, 0), true, None);
        assert!(seen.lock().unwrap().is_empty(), "オフのあいだに、作っている・できているを知らせない");
        assert_eq!(state.lock().unwrap().status.state, IndexState::Erasing, "命令の戻り値・状態の読み取りも「消している」のまま");
        // 差分の反映は、オフなら何も知らせない（オンかと世代を同じ鍵で読む）
        worker.apply_changes(BTreeSet::from([PathBuf::from("/なし/a.txt")]));
        assert!(seen.lock().unwrap().is_empty());
        worker.emit(5, IndexState::Erasing, (0, 0), true, None);
        worker.emit(5, IndexState::Off, (0, 0), true, None);
        assert_eq!(*seen.lock().unwrap(), [IndexState::Erasing, IndexState::Off]);

        // オンにした命令（世代 6）のあとに、消し直しが終わっても「オフ」を知らせない
        {
            let mut shared = state.lock().unwrap();
            shared.enabled = true;
            shared.generation = 6;
        }
        worker.emit(6, IndexState::Off, (0, 0), true, None);
        worker.emit(6, IndexState::Erasing, (0, 0), true, None);
        assert_eq!(seen.lock().unwrap().len(), 2, "オンのあいだに、オフ・消しているを知らせない");
        assert!(!worker.index_path.exists(), "オフのあいだは、索引のファイルを作らない");
    }

    fn set(state: &Arc<Mutex<Shared>>, enabled: bool, generation: u64) {
        let mut shared = state.lock().unwrap();
        shared.enabled = enabled;
        shared.generation = generation;
    }

    /// 消し終えたあとに作り直した索引の中身は、消し終えたことにしない。オンにする命令が古くなって飛ばされ、
    /// 作り直しだけをしたあとで、オフにして消せなかったときも、一覧を変えたときに「オフ」を知らせない。
    // [IDX-028]
    #[test]
    fn a_past_erase_does_not_report_off_after_the_index_was_recreated() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("資料");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "経費").unwrap();
        let folders = vec![SearchFolder::register(&root).unwrap()];
        let index_path = dir.path().join("data").join(INDEX_FILE_FOR_TEST);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let state = shared(false, 1);
        state.lock().unwrap().folders = folders.clone();
        let mut worker = worker(index_path.clone(), state.clone(), seen.clone());
        let last = |seen: &Arc<Mutex<Vec<IndexState>>>| *seen.lock().unwrap().last().unwrap();

        // オフで起動して、索引を消し終える
        worker.handle(1, Command::Disable);
        assert_eq!(last(&seen), IndexState::Off);

        // オンにする命令（世代 2）のすぐあとに、作り直す命令（世代 3）が出た。オンにする命令は古いので飛ばす
        set(&state, true, 3);
        worker.handle(2, Command::Enable(folders.clone()));
        worker.handle(3, Command::Rebuild);
        assert_eq!(last(&seen), IndexState::Ready);
        assert_eq!(state.lock().unwrap().status.files, 1, "作り直した索引に中身がある");

        // オフにしたが、索引を消すことも空にすることもできない（索引のファイルの場所に、中身のあるフォルダがある）
        set(&state, false, 4);
        worker.index = None;
        let blocked = dir.path().join("消せない");
        std::fs::create_dir_all(blocked.join("中")).unwrap();
        worker.index_path = blocked;
        worker.handle(4, Command::Disable);
        assert_eq!(last(&seen), IndexState::Erasing);
        assert_eq!(worker.erasure, Erasure::Pending);

        // 消せないあいだに一覧を変えても、「オフ」にしない
        set(&state, false, 5);
        worker.handle(5, Command::SetFolders(folders.clone()));
        assert_eq!(last(&seen), IndexState::Erasing);
        assert_eq!(state.lock().unwrap().status.state, IndexState::Erasing);
        worker.emit(5, IndexState::Off, (0, 0), true, None);
        assert_eq!(last(&seen), IndexState::Erasing, "消し終えていなければ、「オフ」を知らせない");

        // 消せるようになったら、消してから「オフ」
        worker.index_path = index_path.clone();
        set(&state, false, 6);
        worker.handle(6, Command::SetFolders(folders));
        assert_eq!(last(&seen), IndexState::Off);
        assert!(!index_path.exists());
    }

    /// 索引を検索から外せず、空にして作り直したときは、過去のノートも入れ直す（フォルダだけを読み直さない）。
    /// 入れ直すときは設定を書き換えず、いまの世代のまま原本から読む（オフにされていたら、入れ直さない）。
    // [IDX-016][IDX-017]
    #[test]
    fn notes_are_refilled_after_the_index_is_reset_by_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INDEX_FILE_FOR_TEST);
        let state = shared(true, 1);
        {
            let mut shared = state.lock().unwrap();
            shared.notes = true;
            shared.notes_generation = 3;
        }
        let mut worker = worker(path.clone(), state.clone(), Arc::new(Mutex::new(Vec::new())));
        *worker.note_source.lock().unwrap() = Some(MemoryNotes::with(vec![note_doc("a", "予算")]));
        worker.load_note("a", None);
        assert_eq!(worker.open_index().unwrap().count_notes().unwrap(), 1);
        worker.invalidate_all();
        assert_eq!(worker.open_index().unwrap().count_notes().unwrap(), 0);
        assert_eq!(worker.note_queue, ["a"], "入れ直す");
        assert_eq!(worker.queue_generation, 3, "いまの世代のまま");
        {
            let shared = state.lock().unwrap();
            assert_eq!(shared.notes_generation, 3, "世代を進めない（設定を書き換えない）");
            assert_eq!(shared.notes_cleared, 3, "空にし終えた");
        }

        // 復旧の途中でオフにされていたら、オンに戻さず、入れ直さない
        worker.note_queue.clear();
        {
            let mut shared = state.lock().unwrap();
            shared.notes = false;
            shared.notes_generation = 4;
        }
        worker.invalidate_all();
        assert!(worker.note_queue.is_empty());
        assert!(!state.lock().unwrap().notes, "オフのまま");
    }

    /// ノートの索引をオンにしたら、その世代の索引を空にし終えるまで、ノートを探さない
    /// （前回の起動で消せなかった削除済みのノートの行を、起動直後の全フォルダの走査のあいだに出さない）。
    // [NOTE-015][IDX-017]
    #[test]
    fn notes_from_before_the_reset_are_not_searched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INDEX_FILE_FOR_TEST);
        {
            let index = SearchIndex::open(&path).unwrap();
            index.upsert_note(&note_doc("gone", "予算")).unwrap();
            index.close().unwrap();
        }
        let folder = dir.path().join("資料");
        std::fs::create_dir_all(&folder).unwrap();
        for i in 0..300 {
            std::fs::write(folder.join(format!("{i}.txt")), "本文").unwrap();
        }
        let indexer = Indexer::spawn(path.clone(), |_| {});
        indexer.set_note_source(MemoryNotes::with(vec![note_doc("a", "予算")]));
        indexer.enable(vec![SearchFolder::register(&folder).unwrap()]);
        indexer.set_notes(true);
        let found = || indexer.search(&["予算".to_string()], None, 8).1.into_iter().map(|h| h.note_id).collect::<Vec<_>>();
        let start = Instant::now();
        loop {
            let ids = found();
            assert!(!ids.contains(&"gone".to_string()), "空にする前の行を出さない");
            if ids == ["a"] {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(20), "{ids:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(indexer.shutdown_and_wait(Duration::from_secs(5)));
    }

    /// 一覧やノートを原本から読めないときは、読み直す。読めない・書けないノートは、読み直せるまで検索に出さない。
    // [IDX-017]
    #[test]
    fn notes_that_cannot_be_read_are_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INDEX_FILE_FOR_TEST);
        let intervals = Intervals { purge_retry: Duration::from_millis(100), ..Default::default() };
        let indexer = Indexer::spawn_with_intervals(path.clone(), Limits::default(), intervals, |_| {});
        let source = MemoryNotes::with(vec![note_doc("a", "予算")]);
        source.fail(true);
        indexer.set_note_source(source.clone());
        indexer.enable(Vec::new());
        indexer.set_notes(true);
        let found = |term: &str| indexer.search(&[term.to_string()], None, 8).1.into_iter().map(|h| h.note_id).collect::<Vec<_>>();
        std::thread::sleep(Duration::from_millis(300));
        assert!(found("予算").is_empty(), "一覧を読めない");
        source.fail(false);
        wait_until("一覧を読み直す", || found("予算") == ["a"]);

        // 読めなかったノートは、古い行を検索に出さず、読めるようになったら原本の最新の中身を入れる
        source.fail(true);
        source.notes.lock().unwrap()[0].memo = "経費".into();
        indexer.note_saved("a");
        wait_until("古い行を出さない", || found("予算").is_empty());
        assert!(found("経費").is_empty());
        source.fail(false);
        wait_until("読み直す", || found("経費") == ["a"]);
        assert!(found("予算").is_empty());

        // 原本で削除されていたら、索引からも消す
        source.notes.lock().unwrap().clear();
        indexer.note_saved("a");
        wait_until("消す", || SearchIndex::open_existing(&path).unwrap().note_ids().unwrap().is_empty());
        assert!(found("経費").is_empty());
        assert!(indexer.shutdown_and_wait(Duration::from_secs(5)));
    }

    /// 同じパスに別の実体を登録し直したら、索引の行を外す前でも、前の実体の行は探さない。
    // [IDX-027][IDX-034]
    #[test]
    fn rows_of_a_previous_place_at_the_same_path_are_not_searched() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("資料");
        let other = dir.path().join("別");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(folder.join("a.txt"), "予算の資料").unwrap();
        let indexer = Indexer::spawn(dir.path().join(INDEX_FILE_FOR_TEST), |_| {});
        indexer.enable(vec![SearchFolder::register(&folder).unwrap()]);
        let hits = || indexer.search(&["予算".to_string()], None, 8).0.len();
        wait_until("索引ができる", || hits() == 1);
        // 作業用のスレッドが行を外す前に、同じパスの登録が別の実体に替わった
        indexer.inner.shared.lock().unwrap().folders[0].place = SearchFolder::register(&other).unwrap().place;
        assert_eq!(hits(), 0);
        indexer.inner.shared.lock().unwrap().folders.clear();
        assert_eq!(hits(), 0, "外したフォルダの行も探さない");
        assert!(indexer.shutdown_and_wait(Duration::from_secs(5)));
    }

    /// 索引を空にする・消すあいだは、検索用の接続を開かない（作り直す命令を出してから、作業用のスレッドが空にし終えるまで）。
    // [IDX-011][IDX-028]
    #[test]
    fn search_does_not_reopen_the_index_while_it_is_being_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INDEX_FILE_FOR_TEST);
        let indexer = Indexer::spawn(path.clone(), |_| {});
        indexer.set_note_source(MemoryNotes::with(vec![note_doc("a", "予算")]));
        indexer.enable(Vec::new());
        indexer.set_notes(true);
        let found = || indexer.search(&["予算".to_string()], None, 8).1.len();
        let start = Instant::now();
        while found() != 1 {
            assert!(start.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(20));
        }
        // 命令を出した時点で、接続を閉じて開かせない（オフにする・作り直す命令と同じ）
        {
            let mut reader = indexer.inner.reader.lock().unwrap();
            reader.index = None;
            reader.blocks += 1;
        }
        assert_eq!(found(), 0, "空にし終えるまで探さない");
        assert!(indexer.inner.reader.lock().unwrap().index.is_none(), "接続を開かない");
        indexer.inner.reader.lock().unwrap().blocks -= 1;
        assert_eq!(found(), 1);

        indexer.rebuild().unwrap();
        let start = Instant::now();
        while indexer.inner.reader.lock().unwrap().blocks > 0 {
            assert!(start.elapsed() < Duration::from_secs(20), "作り直し終えたら、検索を戻す");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(found(), 0, "作り直した索引には、入れ直すまでノートがない");
        indexer.disable();
        assert_eq!(found(), 0);
        assert!(indexer.shutdown_and_wait(Duration::from_secs(5)));
    }
}
