//! 資料の索引を裏で作り、フォルダの変更を見張って更新する。
//!
//! 索引への読み書きは、1 本の作業用のスレッドだけが行う。画面の操作と録音は止めない。
//! 相棒がオフのあいだは、索引を作らず、見張りもしない（オフにしたときは、索引の中身を消す）。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::extract::{extract_within, Extracted};
use super::store::{remove_index_files, stamp_of_file, SearchIndex};
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
    /// 文字に直せない名前（損なわずに保存できない）・リンク・フォルダでないものは、エラー。
    pub fn register(path: &Path) -> std::io::Result<SearchFolder> {
        let text = path.to_str().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "文字に直せない名前を含むフォルダは追加できません")
        })?;
        let root = Root::open(path)?;
        Ok(SearchFolder { path: text.to_string(), no_content: false, place: Some(root.place()) })
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
    /// 作業用のスレッドを止める
    Shutdown,
}

struct Shared {
    status: IndexStatus,
    enabled: bool,
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

struct Inner {
    tx: Sender<Message>,
    shared: Arc<Mutex<Shared>>,
    /// 状態の知らせ（`on_status`）と命令の順を守る。知らせの途中に出た命令は、知らせのあとに出たことになる
    order: Arc<Mutex<()>>,
    thread: Mutex<Option<JoinHandle<()>>>,
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
            folders: Vec::new(),
            generation: 0,
        }));
        let order = Arc::new(Mutex::new(()));
        let worker = Worker::new(index_path, limits, intervals, shared.clone(), order.clone(), Arc::new(on_status), tx.clone());
        let thread = std::thread::Builder::new()
            .name("search-index".into())
            .spawn(move || worker.run(rx))
            .expect("索引の作業用のスレッドを起こせません");
        Self { inner: Arc::new(Inner { tx, shared, order, thread: Mutex::new(Some(thread)) }) }
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
            index_path,
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
            seen: HashMap::new(),
            changes: 0,
            incomplete: HashSet::new(),
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
        self.command(|shared| {
            if !shared.enabled {
                return None;
            }
            let status = IndexStatus { state: IndexState::Building, done: 0, total: 0, ..shared.status.clone() };
            shared.set_status(status);
            Some(Command::Rebuild)
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.shared.lock().unwrap().enabled
    }

    pub fn status(&self) -> IndexStatus {
        self.inner.shared.lock().unwrap().status.clone()
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
    /// 探すフォルダを最後に調べたときの識別子（同じ場所で作り直されたら、すべて読み直す）。一覧から外すまで覚えておく
    seen: HashMap<PathBuf, FileId>,
    /// 索引に書いた回数（変わったファイルがあったときだけ、更新した時刻を進める）
    changes: u64,
    /// 中を読めなかったフォルダがあり、調べ切れていないフォルダ（見直しの間隔で調べ直す）
    incomplete: HashSet<PathBuf>,
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
        loop {
            let timeout = match pending_since {
                None if self.erasure == Erasure::Pending => self.intervals.root_check.min(self.intervals.poll).min(self.intervals.purge_retry),
                None => self.intervals.root_check.min(self.intervals.poll),
                Some(since) => CHANGE_DEBOUNCE.min(MAX_CHANGE_DELAY.saturating_sub(since.elapsed())),
            };
            let flush = match rx.recv_timeout(timeout) {
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
                Ok(Message::Command(generation, command)) => {
                    // 命令は、どれもフォルダをすべて調べ直して見張り直すか、索引を消す
                    pending.clear();
                    pending_since = None;
                    watch_failed = false;
                    self.handle(generation, command);
                    false
                }
                Err(RecvTimeoutError::Timeout) => true,
            };
            if flush {
                pending_since = None;
                if std::mem::take(&mut watch_failed) {
                    self.reset_watcher();
                }
                let changed = std::mem::take(&mut pending);
                self.apply_changes(changed);
            }
            if self.erasure == Erasure::Pending && !self.enabled() && self.last_purge.elapsed() >= self.intervals.purge_retry {
                self.purge();
                if self.erasure == Erasure::Erased {
                    // オンにする命令が先に出ていたら、知らせない（emit が、オンのあいだの「オフ」を捨てる）
                    let generation = self.generation();
                    self.emit(generation, IndexState::Off, (0, 0), true, None);
                }
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
            let opened = self.open_root(&root);
            let exists = opened.is_some();
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
                Err(err) => eprintln!("資料の索引を開けません: {err}"),
            }
        }
        self.index.as_ref()
    }

    fn handle(&mut self, generation: u64, command: Command) {
        match command {
            Command::Enable(_) | Command::SetFolders(_) | Command::Rebuild if self.stale(generation) => {}
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
                self.remove_unlisted(&folders);
                self.build(generation, false, changes);
            }
            Command::Rebuild => {
                self.reset_index();
                self.build(generation, true, self.changes);
            }
            Command::Disable => {
                self.watcher = None;
                self.watched.clear();
                self.polled.clear();
                self.truncated.clear();
                self.missing.clear();
                self.incomplete.clear();
                self.purge();
                // 消し終えてから「オフ」を知らせる。消せなかったときは、作成と見張りは止めたまま「消している」を知らせ、
                // 消し直せたときに「オフ」を知らせる
                let state = if self.erasure == Erasure::Erased { IndexState::Off } else { IndexState::Erasing };
                self.emit(generation, state, (0, 0), true, None);
            }
        }
    }

    /// 索引の中身を消し、ファイルも消す。消せなかったら、あとで消し直す。
    fn purge(&mut self) {
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
    }

    /// 「索引を作り直す」: 索引を空にする。空にできなければ、ファイルを消して作り直す。
    fn reset_index(&mut self) {
        self.erasure = Erasure::Unknown;
        let cleared = self.open_index().map(|index| index.clear());
        if !matches!(cleared, Some(Ok(()))) {
            if let Some(index) = self.index.take() {
                let _ = index.close();
            }
            if let Err(err) = remove_index_files(&self.index_path) {
                eprintln!("資料の索引を空にできません: {err}");
            }
        }
    }

    /// 一覧にないフォルダのファイルを、索引から消す。
    fn remove_unlisted(&mut self, folders: &[SearchFolder]) {
        let listed: HashSet<&str> = folders.iter().map(|f| f.path.as_str()).collect();
        self.truncated.retain(|p| listed.contains(p.to_string_lossy().as_ref()));
        self.missing.retain(|p| listed.contains(p.to_string_lossy().as_ref()));
        self.incomplete.retain(|p| listed.contains(p.to_string_lossy().as_ref()));
        self.seen.retain(|p, _| listed.contains(p.to_string_lossy().as_ref()));
        let Some(index) = self.open_index() else { return };
        let mut removed = 0;
        for folder in index.folders().unwrap_or_default() {
            if !listed.contains(folder.to_string_lossy().as_ref()) && index.remove_folder(&folder).is_ok() {
                removed += 1;
            }
        }
        self.changes += removed;
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
            // それまでの索引も消し、選び直すまで読まない
            self.missing.remove(root);
            self.truncated.remove(root);
            self.incomplete.remove(root);
            if let Some(index) = self.open_index() {
                if index.count_folder(root).unwrap_or(0) > 0 && index.remove_folder(root).is_ok() {
                    self.changes += 1;
                }
            }
            return true;
        };
        let Some(opened) = self.open_root(root) else {
            // 見つからないとき・別の場所を指すようになったときは、索引を消さずに残す
            self.missing.insert(root.to_path_buf());
            return true;
        };
        // 同じ場所で別のフォルダに替わっていたら、名前・大きさ・日時が同じファイルも読み直す
        // （初めて調べるときは、足したときの実体と比べる。アプリを起動し直したあとも）
        let previous = self.seen.get(root).copied().unwrap_or(place.id());
        let replaced = previous != opened.id();
        let all = all || replaced;
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
        if self.open_index().is_none() {
            return true;
        }
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
        self.seen.insert(root.to_path_buf(), opened.id());
        true
    }

    /// 一覧のフォルダの、足したときの実体（記録していなければ None）。
    fn place_of(&self, root: &Path) -> Option<Place> {
        self.shared.lock().unwrap().folders.iter().find(|folder| Path::new(&folder.path) == root).and_then(|folder| folder.place.clone())
    }

    /// 探すフォルダを開き、足したときの実体と同じフォルダか（同じ場所で作り直したものを含む）を確かめる。
    /// 上のフォルダがリンクに替わり、別の場所の同じ名前のフォルダを指すようになったら None（見つからないものとして扱う）。
    /// 足したときの実体を記録していないフォルダも開かない。
    fn open_root(&mut self, root: &Path) -> Option<Root> {
        let place = self.place_of(root)?;
        let opened = Root::open(root).ok()?;
        place.admits(&opened).then_some(opened)
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
                if self.place_of(&root).is_none() {
                    // 足したときの実体を記録していない（選び直すまで読まない）
                    continue;
                }
                let Some(opened) = self.open_root(&root) else {
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
                // 変わったファイルが分かっているときは、そのファイルだけを読み直す（フォルダそのものなら、すべて調べ直す）
                let whole = returned || changed.contains(&root);
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
                    unconfirmed: folder.place.is_none(),
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
                Message::Command(..) | Message::Shutdown => "ほか".into(),
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

    fn shared(enabled: bool, generation: u64) -> Arc<Mutex<Shared>> {
        Arc::new(Mutex::new(Shared {
            status: IndexStatus { state: IndexState::Erasing, done: 0, total: 0, files: 0, updated_at: None, folders: Vec::new(), revision: 0 },
            enabled,
            folders: Vec::new(),
            generation,
        }))
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
}
