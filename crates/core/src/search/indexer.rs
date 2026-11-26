//! 資料の索引を裏で作り、フォルダの変更を見張って更新する。
//!
//! 索引への読み書きは、1 本の作業用のスレッドだけが行う。画面の操作と録音は止めない。
//! 相棒がオフのあいだは、索引を作らず、見張りもしない（オフにしたときは、索引の中身を消す）。

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::extract::extract;
use super::store::SearchIndex;
use super::walk::{inspect_file, reachable_without_links, walk, Limits};
use crate::time::now_ms;

/// 見張れないフォルダ（ネットワークのドライブなど）を見直す間隔。
pub const POLL_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// 変更の知らせを受けてから、まとめて反映するまでの待ち時間（続けて届く知らせを 1 回にまとめる）。
pub const CHANGE_DEBOUNCE: Duration = Duration::from_secs(2);

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
    /// フォルダの中で変更があった（見張り・見直し）
    Changed(PathBuf),
    /// 作業用のスレッドを止める
    Shutdown,
}

struct Shared {
    status: IndexStatus,
    enabled: bool,
    folders: Vec<SearchFolder>,
}

/// 資料の索引の作業用のスレッドへの窓口。
#[derive(Clone)]
pub struct Indexer {
    tx: Sender<Command>,
    shared: Arc<Mutex<Shared>>,
    /// 命令を出すたびに増やす。作っている途中の処理は、これが変わったら途中でやめる
    generation: Arc<AtomicU64>,
}

impl Indexer {
    /// 作業用のスレッドを起こす。`on_status` は、状態が変わるたびに（作っている途中は間引いて）呼ぶ。
    pub fn spawn(index_path: PathBuf, on_status: impl Fn(&IndexStatus) + Send + Sync + 'static) -> Self {
        Self::spawn_with(index_path, Limits::default(), on_status)
    }

    pub fn spawn_with(index_path: PathBuf, limits: Limits, on_status: impl Fn(&IndexStatus) + Send + Sync + 'static) -> Self {
        let (tx, rx) = channel();
        let shared = Arc::new(Mutex::new(Shared {
            status: IndexStatus { state: IndexState::Off, done: 0, total: 0, files: 0, updated_at: None, folders: Vec::new() },
            enabled: false,
            folders: Vec::new(),
        }));
        let generation = Arc::new(AtomicU64::new(0));
        let worker = Worker {
            index_path,
            limits,
            index: None,
            shared: shared.clone(),
            generation: generation.clone(),
            on_status: Arc::new(on_status),
            tx: tx.clone(),
            watcher: None,
            polled: Vec::new(),
            last_poll: Instant::now(),
            last_emit: Instant::now(),
            truncated: HashSet::new(),
            missing: HashSet::new(),
        };
        std::thread::Builder::new()
            .name("search-index".into())
            .spawn(move || worker.run(rx))
            .expect("索引の作業用のスレッドを起こせません");
        Self { tx, shared, generation }
    }

    fn send(&self, command: Command) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let _ = self.tx.send(command);
    }

    pub fn enable(&self, folders: Vec<SearchFolder>) {
        {
            let mut shared = self.shared.lock().unwrap();
            shared.enabled = true;
            shared.folders = folders.clone();
            list_folders(&mut shared);
        }
        self.send(Command::Enable(folders));
    }

    pub fn disable(&self) {
        {
            let mut shared = self.shared.lock().unwrap();
            shared.enabled = false;
        }
        self.send(Command::Disable);
    }

    pub fn set_folders(&self, folders: Vec<SearchFolder>) {
        {
            let mut shared = self.shared.lock().unwrap();
            shared.folders = folders.clone();
            list_folders(&mut shared);
        }
        self.send(Command::SetFolders(folders));
    }

    /// 索引を作り直す。相棒がオフなら何もしない（false）。
    pub fn rebuild(&self) -> bool {
        {
            let mut shared = self.shared.lock().unwrap();
            if !shared.enabled {
                return false;
            }
            shared.status.state = IndexState::Building;
        }
        self.send(Command::Rebuild);
        true
    }

    pub fn is_enabled(&self) -> bool {
        self.shared.lock().unwrap().enabled
    }

    pub fn status(&self) -> IndexStatus {
        self.shared.lock().unwrap().status.clone()
    }

    pub fn shutdown(&self) {
        self.send(Command::Shutdown);
    }
}

struct Worker {
    index_path: PathBuf,
    limits: Limits,
    index: Option<SearchIndex>,
    shared: Arc<Mutex<Shared>>,
    generation: Arc<AtomicU64>,
    on_status: StatusCallback,
    tx: Sender<Command>,
    watcher: Option<notify::RecommendedWatcher>,
    /// 見張れないので、見直すフォルダ
    polled: Vec<PathBuf>,
    last_poll: Instant,
    last_emit: Instant,
    truncated: HashSet<PathBuf>,
    missing: HashSet<PathBuf>,
}

/// UNC のパス（`\\server\share`）か。ネットワークのドライブは、変更の知らせが届かないことがあるので見直す。
fn is_network_path(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.starts_with("\\\\") || text.starts_with("//")
}

impl Worker {
    fn run(mut self, rx: Receiver<Command>) {
        let mut pending: BTreeSet<PathBuf> = BTreeSet::new();
        loop {
            let timeout = if pending.is_empty() { Duration::from_secs(60) } else { CHANGE_DEBOUNCE };
            match rx.recv_timeout(timeout) {
                Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
                Ok(Command::Changed(path)) => {
                    pending.insert(path);
                    continue;
                }
                Ok(command) => {
                    pending.clear();
                    self.handle(command);
                }
                Err(RecvTimeoutError::Timeout) => {
                    let changed = std::mem::take(&mut pending);
                    self.apply_changes(changed);
                }
            }
            if self.enabled() && !self.polled.is_empty() && self.last_poll.elapsed() >= POLL_INTERVAL {
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

    fn open_index(&mut self) -> Option<&SearchIndex> {
        if self.index.is_none() {
            match SearchIndex::open(&self.index_path) {
                Ok(index) => self.index = Some(index),
                Err(err) => eprintln!("資料の索引を開けません: {err}"),
            }
        }
        self.index.as_ref()
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Enable(_) | Command::SetFolders(_) if !self.enabled() => self.emit(IndexState::Off, 0, 0, true),
            Command::Enable(folders) => {
                self.remove_unlisted(&folders);
                self.build(false);
            }
            Command::SetFolders(folders) => {
                self.remove_unlisted(&folders);
                self.build(false);
            }
            Command::Rebuild if self.enabled() => {
                if let Some(index) = self.open_index() {
                    let _ = index.clear();
                }
                self.build(true);
            }
            Command::Rebuild => {}
            Command::Disable => {
                self.watcher = None;
                self.polled.clear();
                self.truncated.clear();
                self.missing.clear();
                // 索引の中身をすべて消す（ファイルがなければ作らない）
                if self.index.is_some() || self.index_path.exists() {
                    if let Some(index) = self.open_index() {
                        let _ = index.clear();
                    }
                }
                self.shared.lock().unwrap().status.updated_at = None;
                self.emit(IndexState::Off, 0, 0, true);
            }
            Command::Changed(_) | Command::Shutdown => {}
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
    fn build(&mut self, all: bool) {
        let generation = self.generation.load(Ordering::SeqCst);
        let folders = self.folders();
        self.emit(IndexState::Building, 0, 0, true);
        let mut done = 0;
        let mut total = 0;
        for folder in &folders {
            if self.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            let root = PathBuf::from(&folder.path);
            if !self.scan_folder(&root, all, generation, &mut done, &mut total) {
                return;
            }
        }
        self.watch(&folders);
        self.shared.lock().unwrap().status.updated_at = Some(now_ms());
        self.emit(IndexState::Ready, 0, 0, true);
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
            if self.generation.load(Ordering::SeqCst) != generation {
                return false;
            }
            let unchanged = known.get(&file.path) == Some(&(file.modified, file.size));
            if all || !unchanged {
                let content = extract(&file.path, file.kind);
                if let Some(index) = self.index.as_ref() {
                    let _ = index.upsert(root, file, &content);
                }
            }
            *done += 1;
            self.emit(IndexState::Building, *done, *total, false);
        }
        true
    }

    /// 見張りの知らせ・見直しで届いたパスを、それを含むフォルダごとに反映する。
    fn apply_changes(&mut self, changed: BTreeSet<PathBuf>) {
        if changed.is_empty() || !self.enabled() {
            return;
        }
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
        let generation = self.generation.load(Ordering::SeqCst);
        let (mut done, mut total) = (0, 0);
        for root in roots {
            // 変わったファイルが分かっているときは、そのファイルだけを読み直す
            let single: Vec<&PathBuf> = changed.iter().filter(|p| p.starts_with(&root) && *p != &root).collect();
            let handled = !single.is_empty() && single.iter().all(|path| self.apply_file(&root, path));
            if !handled && !self.scan_folder(&root, false, generation, &mut done, &mut total) {
                return;
            }
        }
        self.shared.lock().unwrap().status.updated_at = Some(now_ms());
        self.emit(IndexState::Ready, 0, 0, true);
    }

    /// 1 つのファイルの変更を反映する。フォルダの変更など、ファイル 1 つでは決められないときは false。
    fn apply_file(&mut self, root: &Path, path: &Path) -> bool {
        let limits = self.limits;
        let Some(index) = self.open_index() else { return true };
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => false,
            Ok(_) => {
                match inspect_file(path, limits).filter(|_| reachable_without_links(root, path)) {
                    Some(file) => {
                        let known = index.known_files(root).unwrap_or_default();
                        if known.get(&file.path) != Some(&(file.modified, file.size)) {
                            if !known.contains_key(&file.path) && known.len() >= limits.max_files {
                                return true;
                            }
                            let content = extract(&file.path, file.kind);
                            let _ = index.upsert(root, &file, &content);
                        }
                    }
                    None => {
                        let _ = index.remove_path(path);
                    }
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
        use notify::{RecursiveMode, Watcher};
        self.polled.clear();
        let tx = self.tx.clone();
        let mut watcher = match notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event {
                for path in event.paths {
                    let _ = tx.send(Command::Changed(path));
                }
            }
        }) {
            Ok(watcher) => Some(watcher),
            Err(_) => None,
        };
        for folder in folders {
            let root = PathBuf::from(&folder.path);
            let watched = !is_network_path(&root)
                && watcher.as_mut().is_some_and(|w| w.watch(&root, RecursiveMode::Recursive).is_ok());
            if !watched {
                self.polled.push(root);
            }
        }
        self.last_poll = Instant::now();
        self.watcher = watcher;
    }

    /// 状態を更新して知らせる。`force` でなければ、0.3 秒に 1 回に間引く。
    fn emit(&mut self, state: IndexState, done: usize, total: usize, force: bool) {
        if !force && self.last_emit.elapsed() < Duration::from_millis(300) {
            return;
        }
        self.last_emit = Instant::now();
        let folders = self.folders();
        let enabled = self.enabled();
        if enabled {
            self.open_index();
        }
        let index = self.index.as_ref();
        let rows: Vec<FolderStatus> = folders
            .iter()
            .map(|folder| {
                let root = PathBuf::from(&folder.path);
                FolderStatus {
                    path: folder.path.clone(),
                    no_content: folder.no_content,
                    files: if enabled { index.and_then(|i| i.count_folder(&root).ok()).unwrap_or(0) } else { 0 },
                    missing: enabled && self.missing.contains(&root),
                    truncated: enabled && self.truncated.contains(&root),
                }
            })
            .collect();
        let files = rows.iter().map(|r| r.files).sum();
        let status = {
            let mut shared = self.shared.lock().unwrap();
            shared.status = IndexStatus {
                state: if enabled { state } else { IndexState::Off },
                done,
                total,
                files,
                updated_at: shared.status.updated_at,
                folders: rows,
            };
            shared.status.clone()
        };
        (self.on_status)(&status);
    }
}

/// 一覧の行を、いまのフォルダの並びに合わせる（作業用のスレッドが調べ終えるまでは、前の件数のまま）。
/// 足したフォルダや外したフォルダが、コマンドの戻り値にすぐ表れるようにする。
fn list_folders(shared: &mut Shared) {
    // 相棒がオンなら、作業用のスレッドが調べ始める前から「作っている」にする（戻り値が、処理の進み方で変わらないように）
    shared.status.state = if shared.enabled { IndexState::Building } else { IndexState::Off };
    let previous = std::mem::take(&mut shared.status.folders);
    shared.status.folders = shared
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
    shared.status.files = shared.status.folders.iter().map(|row| row.files).sum();
}
