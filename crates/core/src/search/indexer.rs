//! 資料の索引を裏で作り、フォルダの変更を見張って更新する。
//!
//! 索引への読み書きは、1 本の作業用のスレッドだけが行う。画面の操作と録音は止めない。
//! 相棒がオフのあいだは、索引を作らず、見張りもしない（オフにしたときは、索引の中身を消す）。

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::extract::{extract, Extracted};
use super::store::{remove_index_files, SearchIndex};
use super::walk::{inspect_file, reachable_without_links, walk, FileEntry, Limits};
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
/// 索引のファイルを消せなかったときに、消し直す間隔。
const PURGE_RETRY: Duration = Duration::from_secs(60);

/// 見直しの間隔。
#[derive(Debug, Clone, Copy)]
pub struct Intervals {
    /// 見張れないフォルダ・見つからないフォルダを見直す間隔
    pub poll: Duration,
    /// 見張っているフォルダそのものがあるかを確かめる間隔
    pub root_check: Duration,
}

impl Default for Intervals {
    fn default() -> Self {
        Self { poll: POLL_INTERVAL, root_check: ROOT_CHECK_INTERVAL }
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IndexState {
    /// 相棒がオフで、索引を作っていない
    Off,
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
        let worker = Worker {
            index_path,
            limits,
            intervals,
            index: None,
            shared: shared.clone(),
            order: order.clone(),
            on_status: Arc::new(on_status),
            tx: tx.clone(),
            watcher: None,
            polled: Vec::new(),
            last_poll: Instant::now(),
            last_root_check: Instant::now(),
            last_emit: Instant::now(),
            truncated: HashSet::new(),
            missing: HashSet::new(),
            erased: false,
            purge_pending: false,
            last_purge: Instant::now(),
        };
        let thread = std::thread::Builder::new()
            .name("search-index".into())
            .spawn(move || worker.run(rx))
            .expect("索引の作業用のスレッドを起こせません");
        Self { inner: Arc::new(Inner { tx, shared, order, thread: Mutex::new(Some(thread)) }) }
    }

    /// 共有の状態を変えて、命令を出す（同じ鍵の中で世代を進める。途中の処理が、古い状態を知らせないように）。
    fn command(&self, update: impl FnOnce(&mut Shared) -> Option<Command>) -> bool {
        let _order = self.inner.order.lock().unwrap();
        let mut shared = self.inner.shared.lock().unwrap();
        let Some(command) = update(&mut shared) else {
            return false;
        };
        shared.generation += 1;
        let _ = self.inner.tx.send(Message::Command(shared.generation, command));
        true
    }

    pub fn enable(&self, folders: Vec<SearchFolder>) {
        self.command(|shared| {
            shared.enabled = true;
            shared.folders = folders.clone();
            list_folders(shared);
            Some(Command::Enable(folders))
        });
    }

    /// 止めて、索引の中身を消す。状態が「オフ」になるのは、作業用のスレッドが消し終えてから。
    pub fn disable(&self) {
        self.command(|shared| {
            shared.enabled = false;
            Some(Command::Disable)
        });
    }

    /// 探すフォルダの一覧を変える（足した・外した）。相棒がオンなら、索引に反映する。
    pub fn set_folders(&self, folders: Vec<SearchFolder>) {
        self.command(|shared| {
            shared.folders = folders.clone();
            list_folders(shared);
            Some(Command::SetFolders(folders))
        });
    }

    /// 「中身を送らない」などの、索引の中身に関わらない設定だけを変える。
    /// 調べ直さず、作っている途中の処理も止めない。状態（作っている・できている）と更新した時刻も変えない。
    pub fn set_folder_options(&self, folders: Vec<SearchFolder>) {
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
    }

    /// 索引を作り直す。相棒がオフなら何もしない（false）。
    pub fn rebuild(&self) -> bool {
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
    /// 見張れないので、見直すフォルダ（ネットワークのドライブ・見つからなくなったフォルダ）
    polled: Vec<PathBuf>,
    last_poll: Instant,
    last_root_check: Instant,
    last_emit: Instant,
    truncated: HashSet<PathBuf>,
    missing: HashSet<PathBuf>,
    /// オフにして、索引を消し終えた（次にオンにするまで）
    erased: bool,
    /// オフにしたが、索引のファイルを消し切れていない（あとで消し直す）
    purge_pending: bool,
    last_purge: Instant,
}

/// UNC のパス（`\\server\share`）か。ネットワークのドライブは、変更の知らせが届かないことがあるので見直す。
fn is_network_path(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.starts_with("\\\\") || text.starts_with("//")
}

/// 索引に入れる中身を読む。クラウドにだけあるファイルは、ダウンロードさせないよう読まない。
fn read_content(file: &FileEntry) -> Extracted {
    if file.online_only {
        return Extracted { title: None, body: String::new(), name_only: true };
    }
    extract(&file.path, file.kind)
}

impl Worker {
    fn run(mut self, rx: Receiver<Message>) {
        let mut pending: BTreeSet<PathBuf> = BTreeSet::new();
        let mut pending_since: Option<Instant> = None;
        loop {
            let timeout = match pending_since {
                None => self.intervals.root_check.min(self.intervals.poll),
                Some(since) => CHANGE_DEBOUNCE.min(MAX_CHANGE_DELAY.saturating_sub(since.elapsed())),
            };
            let flush = match rx.recv_timeout(timeout) {
                Ok(Message::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
                Ok(Message::Changed(path)) => {
                    pending.insert(path);
                    // 知らせが続いても、最初の知らせから MAX_CHANGE_DELAY たったら反映する
                    pending_since.get_or_insert_with(Instant::now).elapsed() >= MAX_CHANGE_DELAY
                }
                Ok(Message::Command(generation, command)) => {
                    // 命令は、どれもフォルダをすべて調べ直すか、索引を消す
                    pending.clear();
                    pending_since = None;
                    self.handle(generation, command);
                    false
                }
                Err(RecvTimeoutError::Timeout) => true,
            };
            if flush {
                pending_since = None;
                let changed = std::mem::take(&mut pending);
                self.apply_changes(changed);
            }
            if self.purge_pending && !self.enabled() && self.last_purge.elapsed() >= PURGE_RETRY {
                self.purge();
            }
            if self.enabled() && self.last_root_check.elapsed() >= self.intervals.root_check {
                self.last_root_check = Instant::now();
                let gone: BTreeSet<PathBuf> = self
                    .folders()
                    .iter()
                    .map(|folder| PathBuf::from(&folder.path))
                    .filter(|root| !self.polled.contains(root) && !self.missing.contains(root) && !root.is_dir())
                    .collect();
                self.apply_changes(gone);
            }
            if self.enabled() && !self.polled.is_empty() && self.last_poll.elapsed() >= self.intervals.poll {
                self.last_poll = Instant::now();
                let folders: BTreeSet<PathBuf> = self.polled.iter().cloned().collect();
                self.apply_changes(folders);
            }
        }
    }

    fn enabled(&self) -> bool {
        self.shared.lock().unwrap().enabled
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
            // オフのあいだは、索引を作らない。索引を消し終えていれば、一覧を変えた「オフ」を知らせる
            // （消し終えていなければ、あとに続くオフにした命令が、消し終えてから知らせる）
            Command::Enable(_) | Command::SetFolders(_) | Command::Rebuild if !self.enabled() => {
                if self.erased {
                    self.emit(generation, IndexState::Off, (0, 0), true, None);
                }
            }
            Command::Enable(folders) | Command::SetFolders(folders) => {
                self.erased = false;
                self.purge_pending = false;
                self.remove_unlisted(&folders);
                self.build(generation, false);
            }
            Command::Rebuild => {
                self.reset_index();
                self.build(generation, true);
            }
            Command::Disable => {
                self.watcher = None;
                self.polled.clear();
                self.truncated.clear();
                self.missing.clear();
                self.purge();
                self.erased = true;
                // 消し終えてから「オフ」を知らせる（消せなかったときも、作成と見張りは止めている）
                self.emit(generation, IndexState::Off, (0, 0), true, None);
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
        self.purge_pending = removed.is_err() && !cleared;
    }

    /// 「索引を作り直す」: 索引を空にする。空にできなければ、ファイルを消して作り直す。
    fn reset_index(&mut self) {
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
        let Some(index) = self.open_index() else { return };
        for folder in index.folders().unwrap_or_default() {
            if !listed.contains(folder.to_string_lossy().as_ref()) {
                let _ = index.remove_folder(&folder);
            }
        }
    }

    /// すべてのフォルダを調べ、変わったファイルだけを読み直す（`all` なら、すべて読み直す）。
    fn build(&mut self, generation: u64, all: bool) {
        let folders = self.folders();
        self.emit(generation, IndexState::Building, (0, 0), true, None);
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
        self.watch(&folders);
        self.emit(generation, IndexState::Ready, (0, 0), true, Some(now_ms()));
    }

    /// 1 つのフォルダを調べて索引に反映する。途中でやめたら false。
    fn scan_folder(&mut self, root: &Path, all: bool, generation: u64, done: &mut usize, total: &mut usize) -> bool {
        let walked = match walk(root, self.limits) {
            Ok(walked) => walked,
            Err(_) => {
                // 見つからないときは、索引を消さずに残す
                self.missing.insert(root.to_path_buf());
                return true;
            }
        };
        self.missing.remove(root);
        if walked.truncated {
            self.truncated.insert(root.to_path_buf());
        } else {
            self.truncated.remove(root);
        }
        *total += walked.files.len();
        let Some(index) = self.open_index() else { return true };
        let known = index.known_files(root).unwrap_or_default();
        let found: HashSet<&PathBuf> = walked.files.iter().map(|f| &f.path).collect();
        for path in known.keys().filter(|p| !found.contains(p)) {
            let _ = index.remove_path(path);
        }
        for file in &walked.files {
            if self.stale(generation) {
                return false;
            }
            let unchanged = known.get(&file.path) == Some(&(file.modified, file.size));
            if all || !unchanged {
                let content = read_content(file);
                if let Some(index) = self.index.as_ref() {
                    let _ = index.upsert(root, file, &content);
                }
            }
            *done += 1;
            self.emit(generation, IndexState::Building, (*done, *total), false, None);
        }
        true
    }

    /// 見張りの知らせ・見直しで届いたパスを、それを含むフォルダごとに反映する。
    fn apply_changes(&mut self, changed: BTreeSet<PathBuf>) {
        if changed.is_empty() || !self.enabled() {
            return;
        }
        let generation = self.generation();
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
        for root in roots {
            if self.stale(generation) {
                return;
            }
            if !root.is_dir() {
                // ドライブが外れた・フォルダが移った。索引は残し、戻るまで見直す
                self.missing.insert(root.clone());
                if !self.polled.contains(&root) {
                    self.polled.push(root);
                }
                continue;
            }
            // 見つからなかったフォルダが戻ったときは、すべて調べ直す
            let returned = self.missing.contains(&root);
            // 変わったファイルが分かっているときは、そのファイルだけを読み直す
            let single: Vec<&PathBuf> = changed.iter().filter(|p| p.starts_with(&root) && *p != &root).collect();
            let handled = !returned && !single.is_empty() && single.iter().all(|path| self.apply_file(&root, path));
            if !handled && !self.scan_folder(&root, false, generation, &mut done, &mut total) {
                return;
            }
        }
        if self.stale(generation) {
            return;
        }
        self.rewatch();
        self.emit(generation, IndexState::Ready, (0, 0), true, Some(now_ms()));
    }

    /// 1 つのファイルの変更を反映する。フォルダの変更など、ファイル 1 つでは決められないときは false。
    fn apply_file(&mut self, root: &Path, path: &Path) -> bool {
        let limits = self.limits;
        let Some(index) = self.open_index() else { return true };
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => false,
            Ok(_) => {
                let Some(file) = inspect_file(path, limits).filter(|_| reachable_without_links(root, path)) else {
                    let _ = index.remove_path(path);
                    return true;
                };
                let known = index.stamp(&file.path).unwrap_or_default();
                if known == Some((file.modified, file.size)) {
                    return true;
                }
                if known.is_none() && index.count_folder(root).unwrap_or(0) >= limits.max_files {
                    // 上限に達しているので入れない（一部だけ索引に入れている、と表示する）
                    self.truncated.insert(root.to_path_buf());
                    return true;
                }
                let content = read_content(&file);
                if let Some(index) = self.index.as_ref() {
                    let _ = index.upsert(root, &file, &content);
                }
                true
            }
            // 消えた（またはフォルダごと消えた）。ファイルなら行を消し、フォルダならたどり直す
            Err(_) => match index.contains(path) {
                Ok(true) => {
                    let _ = index.remove_path(path);
                    true
                }
                _ => false,
            },
        }
    }

    /// フォルダの変更を見張る。見張れないフォルダは、見直す一覧に入れる。
    fn watch(&mut self, folders: &[SearchFolder]) {
        self.polled.clear();
        let tx = self.tx.clone();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event {
                for path in event.paths {
                    let _ = tx.send(Message::Changed(path));
                }
            }
        });
        self.watcher = watcher.ok();
        self.polled = folders.iter().map(|folder| PathBuf::from(&folder.path)).collect();
        self.rewatch();
        self.last_poll = Instant::now();
    }

    /// 見直す一覧のフォルダのうち、見張れるようになったもの（ドライブが戻ったなど）を、見張りに移す。
    fn rewatch(&mut self) {
        use notify::{RecursiveMode, Watcher};
        let Some(watcher) = self.watcher.as_mut() else { return };
        self.polled.retain(|root| {
            is_network_path(root) || !root.is_dir() || watcher.watch(root, RecursiveMode::Recursive).is_err()
        });
    }

    /// 状態を更新して知らせる。`force` でなければ、0.3 秒に 1 回に間引く。
    /// `generation` のあとに新しい命令が出ていたら、古い状態なので知らせない。
    /// `updated_at` を渡したら、最後に更新し終えた時刻をそれにする。
    fn emit(&mut self, generation: u64, state: IndexState, progress: (usize, usize), force: bool, updated_at: Option<i64>) {
        if !force && self.last_emit.elapsed() < Duration::from_millis(300) {
            return;
        }
        let folders = self.folders();
        let enabled = self.enabled();
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
                }
            })
            .collect();
        let files = rows.iter().map(|r| r.files).sum();
        let _order = self.order.lock().unwrap();
        let status = {
            let mut shared = self.shared.lock().unwrap();
            if shared.generation != generation {
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
                state: if enabled { state } else { IndexState::Off },
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
            }
        })
        .collect();
    status.files = status.folders.iter().map(|row| row.files).sum();
    shared.set_status(status);
}
