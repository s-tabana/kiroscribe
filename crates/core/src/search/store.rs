//! 資料の索引の保存先（アプリのデータフォルダの `search-index.db`。ノートのデータベースとは別のファイル）と検索。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{params, params_from_iter, Connection};

use super::extract::Extracted;
use super::normalize::{normalize, normalize_with_map};
use super::url;
use super::walk::FileEntry;
use crate::error::Result;

/// 索引のファイルの名前。
pub const INDEX_FILE: &str = "search-index.db";
/// 抜粋の長さの上限（文字）。
pub const EXCERPT_CHARS: usize = 600;
/// 抜粋の位置を決めるときに数える、1 つの検索語の一致の数の上限。
const MAX_HITS_PER_TERM: usize = 5_000;

const SCHEMA_VERSION: i64 = 4;
/// 過去のノートの表がない版（指定したフォルダの資料と、どの実体から読んだかの記録だけを持つ）。
const SCHEMA_VERSION_WITHOUT_NOTES: i64 = 3;
const SCHEMA: &str = r#"
CREATE TABLE docs (
    id          INTEGER PRIMARY KEY,
    source      TEXT NOT NULL,
    folder      TEXT NOT NULL,
    path        TEXT NOT NULL UNIQUE,
    title       TEXT NOT NULL,
    file_name   TEXT NOT NULL,
    body        TEXT NOT NULL,
    norm_title  TEXT NOT NULL,
    norm_name   TEXT NOT NULL,
    norm_body   TEXT NOT NULL,
    name_only   INTEGER NOT NULL,
    modified    INTEGER NOT NULL,
    size        INTEGER NOT NULL,
    online_only INTEGER NOT NULL
);
CREATE INDEX docs_folder ON docs(folder);
CREATE TABLE bindings (
    folder TEXT PRIMARY KEY,
    place  TEXT NOT NULL,
    root   TEXT NOT NULL
);
"#;
const SCHEMA_NOTES: &str = r#"
CREATE TABLE notes (
    note_id          TEXT PRIMARY KEY,
    title            TEXT NOT NULL,
    created_at       INTEGER NOT NULL,
    memo             TEXT NOT NULL,
    minutes          TEXT NOT NULL,
    transcript       TEXT NOT NULL,
    norm_title       TEXT NOT NULL,
    norm_memo        TEXT NOT NULL,
    norm_minutes     TEXT NOT NULL,
    norm_transcript  TEXT NOT NULL
);
"#;

/// 探し先の種類（索引の行の source）。
pub const SOURCE_FOLDER: &str = "folder";

/// 検索で見つかった資料。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub path: PathBuf,
    /// 資料が入っている、追加したフォルダ
    pub folder: PathBuf,
    pub title: String,
    pub file_name: String,
    /// 一致した部分の前後（名前だけで探した資料には付けない）
    pub excerpt: Option<String>,
    /// AI に送る抜粋（本文の中で URL のクエリとフラグメントを見つけてから切り出し、それを除いたもの）
    pub send_excerpt: Option<String>,
    pub modified: i64,
    /// 一致した検索語の数
    pub matched_terms: usize,
    /// 題名かファイル名に一致したか
    pub named: bool,
}

pub struct SearchIndex {
    conn: Connection,
    /// 索引のファイルのパス（メモリの中の索引は None）。消去待ちのあいだは検索しない（[`is_search_suspended`]）
    path: Option<PathBuf>,
    /// 開いたときの、索引のファイルの世代（[`remove_index_files`] で進む）。世代が古い接続からは検索しない
    epoch: u64,
}

/// 探すフォルダの行を、どの実体から読んだか: 足したときの実体（設定に保存したもの）と、読んだときに開いたフォルダの識別子。
/// 索引は、フォルダごとにこれを記録する。記録のないフォルダの行は、出どころを確かめられないので、検索に出さない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub place: String,
    pub root: String,
}

/// 読み直すかを決める、ファイルの目印（更新日時・大きさ・クラウドにだけあるか）。
/// クラウドにだけあるかが変わったら、日時と大きさが同じでも読み直す（名前だけで入れたファイルを、手元に来たら中身ごと入れる）。
pub type Stamp = (i64, u64, bool);

/// ファイルの目印（索引の行と比べる）。
pub fn stamp_of_file(file: &FileEntry) -> Stamp {
    (file.modified, file.size, file.online_only)
}

fn stamp_of(row: &rusqlite::Row<'_>, at: usize) -> rusqlite::Result<Stamp> {
    Ok((row.get::<_, i64>(at)?, row.get::<_, i64>(at + 1)? as u64, row.get::<_, bool>(at + 2)?))
}

/// 索引のファイル（本体と、書き込みの途中の内容を持つ `-wal`・`-shm`）のパス。
pub fn index_files(path: &Path) -> [PathBuf; 3] {
    ["", "-wal", "-shm"].map(|suffix| {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        PathBuf::from(name)
    })
}

/// 消去待ちの記録のファイル（索引の中の資料を検索から外せなかったときに作り、消し終えるまで残す）。
pub fn erase_marker(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push("-erase");
    PathBuf::from(name)
}

/// 消去待ちの記録を作れなかった索引（同じプロセスのあいだ、検索しない）。
static SUSPENDED: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// 索引を消去待ちにして、検索を止める。記録のファイルに残せたら Ok（アプリを起動し直しても、消し終えるまで検索しない）。
/// 残せなくても、このプロセスのあいだは検索しない。
pub fn suspend_search(path: &Path) -> std::io::Result<()> {
    {
        let mut suspended = SUSPENDED.lock().unwrap_or_else(|e| e.into_inner());
        if !suspended.iter().any(|p| p == path) {
            suspended.push(path.to_path_buf());
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let marker = erase_marker(path);
    let mut file = std::fs::File::create(&marker)?;
    std::io::Write::write_all(&mut file, b"erase\n")?;
    file.sync_all()
}

/// 消し終えた索引の消去待ちを解き、検索を再開する。記録のファイルを消せなければエラー（検索は止めたまま）。
pub fn resume_search(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(erase_marker(path)) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    SUSPENDED.lock().unwrap_or_else(|e| e.into_inner()).retain(|p| p != path);
    Ok(())
}

/// 索引が消去待ちか（記録のファイルがある・あるかを確かめられない・このプロセスで記録を作れなかった）。
pub fn is_search_suspended(path: &Path) -> bool {
    if SUSPENDED.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|p| p == path) {
        return true;
    }
    !matches!(std::fs::symlink_metadata(erase_marker(path)), Err(err) if err.kind() == std::io::ErrorKind::NotFound)
}

/// 索引のファイルの世代。ファイルを消すたびに進める（消す前に開いた接続は、消したファイルを読み続けるため、検索に使わない）。
/// 同じ鍵で、接続を開くこと・検索すること（読む側）と、ファイルを消すこと（書く側）を排他する。
static EPOCHS: std::sync::RwLock<Vec<(PathBuf, u64)>> = std::sync::RwLock::new(Vec::new());

/// 世代を数える鍵（絶対パスにする）。
fn epoch_key(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

fn epoch_in(epochs: &[(PathBuf, u64)], key: &Path) -> u64 {
    epochs.iter().find(|(p, _)| p == key).map_or(0, |(_, epoch)| *epoch)
}

/// 索引のファイルをすべて消す（もともとないファイルは、消せたものとみなす）。
/// 消す前に開いていた接続は、消せたかどうかにかかわらず、検索に使えなくする（進行中の検索は、終わるのを待ってから消す）。
pub fn remove_index_files(path: &Path) -> std::io::Result<()> {
    let key = epoch_key(path);
    let mut epochs = EPOCHS.write().unwrap_or_else(|e| e.into_inner());
    match epochs.iter_mut().find(|(p, _)| *p == key) {
        Some((_, epoch)) => *epoch += 1,
        None => epochs.push((key, 1)),
    }
    let mut result = Ok(());
    for file in index_files(path) {
        match std::fs::remove_file(&file) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound && result.is_ok() => result = Err(err),
            _ => {}
        }
    }
    result
}

impl SearchIndex {
    /// 索引を開く（なければ作る）。開けない・壊れているときは、ファイルを消して作り直す。
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match Self::try_open(path) {
            Ok(index) => Ok(index),
            Err(_) => {
                let _ = remove_index_files(path);
                Self::try_open(path)
            }
        }
    }

    /// 作業用のスレッドが作った索引を、検索のために開く（壊れていても作り直さない）。
    /// 開いたときのファイルの世代を記録する（ファイルを消されたら、[`SearchIndex::is_current`] が false になる）。
    pub fn open_existing(path: &Path) -> Result<Self> {
        // 開くあいだにファイルを消されないようにする
        let epochs = EPOCHS.read().unwrap_or_else(|e| e.into_inner());
        let epoch = epoch_in(&epochs, &epoch_key(path));
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != SCHEMA_VERSION {
            return Err(crate::CoreError::Other("索引の形が違います".into()));
        }
        Ok(Self { conn, path: Some(path.to_path_buf()), epoch })
    }

    /// この接続を開いたあとに、索引のファイルが消されていないか（消されていたら、開き直して検索する）。
    pub fn is_current(&self) -> bool {
        let epochs = EPOCHS.read().unwrap_or_else(|e| e.into_inner());
        self.path.as_deref().is_none_or(|path| epoch_in(&epochs, &epoch_key(path)) == self.epoch)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?, None, 0)
    }

    fn try_open(path: &Path) -> Result<Self> {
        // 開くあいだにファイルを消されないようにし、開いたファイルの世代を記録する
        let epochs = EPOCHS.read().unwrap_or_else(|e| e.into_inner());
        let epoch = epoch_in(&epochs, &epoch_key(path));
        let conn = Connection::open(path)?;
        let ok: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if ok != "ok" {
            return Err(crate::CoreError::Other("索引が壊れています".into()));
        }
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn, Some(path.to_path_buf()), epoch)
    }

    fn init(conn: Connection, path: Option<PathBuf>, epoch: u64) -> Result<Self> {
        // 消した行の中身を、ファイルの空き領域に残さない
        conn.execute_batch("PRAGMA secure_delete = ON;")?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version == SCHEMA_VERSION_WITHOUT_NOTES {
            // 資料とフォルダの記録は残し、ノートの表を足す（取り外したドライブなど、読み直せないフォルダの資料も残す）。
            // 表を足すことと版を上げることは、1 つの書き込みで行う
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(SCHEMA_NOTES)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
        } else if version != SCHEMA_VERSION {
            // 索引は作り直せるので、形が違えば捨てて作る
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch("DROP TABLE IF EXISTS docs; DROP TABLE IF EXISTS bindings; DROP TABLE IF EXISTS notes;")?;
            tx.execute_batch(SCHEMA)?;
            tx.execute_batch(SCHEMA_NOTES)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
        }
        Ok(Self { conn, path, epoch })
    }

    /// ファイルを索引に入れる（あれば置き換える）。
    pub fn upsert(&self, folder: &Path, file: &FileEntry, content: &Extracted) -> Result<()> {
        let file_name = file.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let stem = file.path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let title = content.title.clone().unwrap_or(stem);
        self.conn.execute(
            "INSERT INTO docs (source, folder, path, title, file_name, body, norm_title, norm_name, norm_body, name_only, modified, size, online_only)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(path) DO UPDATE SET folder = excluded.folder, title = excluded.title, file_name = excluded.file_name,
               body = excluded.body, norm_title = excluded.norm_title, norm_name = excluded.norm_name, norm_body = excluded.norm_body,
               name_only = excluded.name_only, modified = excluded.modified, size = excluded.size,
               online_only = excluded.online_only",
            params![
                SOURCE_FOLDER,
                folder.to_string_lossy(),
                file.path.to_string_lossy(),
                title,
                file_name,
                content.body,
                normalize(&title),
                normalize(&file_name),
                normalize(&content.body),
                content.name_only,
                file.modified,
                file.size as i64,
                file.online_only,
            ],
        )?;
        Ok(())
    }

    /// フォルダの、いま索引にあるファイルと、その目印（更新日時・大きさ・クラウドにだけあるか）。
    pub fn known_files(&self, folder: &Path) -> Result<HashMap<PathBuf, Stamp>> {
        let mut stmt = self.conn.prepare("SELECT path, modified, size, online_only FROM docs WHERE folder = ?1")?;
        let rows = stmt.query_map([folder.to_string_lossy()], |row| Ok((PathBuf::from(row.get::<_, String>(0)?), stamp_of(row, 1)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn remove_path(&self, path: &Path) -> Result<()> {
        self.conn.execute("DELETE FROM docs WHERE path = ?1", [path.to_string_lossy()])?;
        Ok(())
    }

    /// フォルダのファイルを、すべて索引から消し、どの実体から読んだかの記録も消す（1 つの書き込みで行う）。
    pub fn remove_folder(&self, folder: &Path) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM bindings WHERE folder = ?1", [folder.to_string_lossy()])?;
        tx.execute("DELETE FROM docs WHERE folder = ?1", [folder.to_string_lossy()])?;
        tx.commit()?;
        Ok(())
    }

    /// フォルダの行を、どの実体から読んだかの記録（なければ None）。
    pub fn binding(&self, folder: &Path) -> Result<Option<Binding>> {
        let mut stmt = self.conn.prepare("SELECT place, root FROM bindings WHERE folder = ?1")?;
        let mut rows = stmt.query([folder.to_string_lossy()])?;
        Ok(match rows.next()? {
            Some(row) => Some(Binding { place: row.get(0)?, root: row.get(1)? }),
            None => None,
        })
    }

    /// フォルダのそれまでの行をすべて消し、これから `binding` の実体から読むと記録する（1 つの書き込みで行う）。
    pub fn rebind(&self, folder: &Path, binding: &Binding) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM docs WHERE folder = ?1", [folder.to_string_lossy()])?;
        tx.execute(
            "INSERT INTO bindings (folder, place, root) VALUES (?1, ?2, ?3)
             ON CONFLICT(folder) DO UPDATE SET place = excluded.place, root = excluded.root",
            params![folder.to_string_lossy(), binding.place, binding.root],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// フォルダの行を、出どころを確かめられないものにする（検索に出さない。行は、あとで消す）。
    pub fn unbind(&self, folder: &Path) -> Result<()> {
        self.conn.execute("DELETE FROM bindings WHERE folder = ?1", [folder.to_string_lossy()])?;
        Ok(())
    }

    /// フォルダの、索引に残っている行の数（出どころを確かめられない行も数える）。
    pub fn stored_rows(&self, folder: &Path) -> Result<usize> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM docs WHERE folder = ?1", [folder.to_string_lossy()], |row| row.get::<_, i64>(0))?
            as usize)
    }

    /// 索引を空にする。消した内容が、本体と `-wal` のファイルに残らないところまで済ませる。
    pub fn clear(&self) -> Result<()> {
        self.conn.execute_batch("DELETE FROM docs; DELETE FROM bindings; DELETE FROM notes;")?;
        // 空き領域を詰めてから、書き込みの途中の内容（-wal）を本体に反映して空にする
        self.conn.execute_batch("VACUUM;")?;
        let busy: i64 = self.conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
        if busy != 0 {
            return Err(crate::CoreError::Other("索引の書き込みを、ファイルに反映できません".into()));
        }
        Ok(())
    }

    /// 接続を閉じる（閉じられないときはエラー）。
    pub fn close(self) -> Result<()> {
        self.conn.close().map_err(|(_, err)| err)?;
        Ok(())
    }

    /// そのパスのファイルの、索引にある目印（なければ None）。
    pub fn stamp(&self, path: &Path) -> Result<Option<Stamp>> {
        let mut stmt = self.conn.prepare("SELECT modified, size, online_only FROM docs WHERE path = ?1")?;
        let mut rows = stmt.query([path.to_string_lossy()])?;
        Ok(match rows.next()? {
            Some(row) => Some(stamp_of(row, 0)?),
            None => None,
        })
    }

    /// フォルダの、検索に出す行の数（出どころを確かめられない行は数えない）。
    pub fn count_folder(&self, folder: &Path) -> Result<usize> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM docs WHERE folder = ?1 AND folder IN (SELECT folder FROM bindings)",
            [folder.to_string_lossy()],
            |row| row.get::<_, i64>(0),
        )? as usize)
    }

    pub fn count(&self) -> Result<usize> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM docs", [], |row| row.get::<_, i64>(0))? as usize)
    }

    /// 索引に行か記録があるフォルダ。
    pub fn folders(&self) -> Result<Vec<PathBuf>> {
        let mut stmt = self.conn.prepare("SELECT folder FROM docs WHERE source = ?1 UNION SELECT folder FROM bindings")?;
        let rows = stmt.query_map([SOURCE_FOLDER], |row| Ok(PathBuf::from(row.get::<_, String>(0)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// そのパスのファイルが索引にあるか。
    pub fn contains(&self, path: &Path) -> Result<bool> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM docs WHERE path = ?1", [path.to_string_lossy()], |row| row.get::<_, i64>(0))? > 0)
    }

    /// 検索語のどれかを、題名・ファイル名・中身に含む資料を、よく一致する順に `limit` 件まで返す。
    /// 並びは、一致した検索語の数、題名かファイル名に一致したか、更新日時の新しさの順。
    /// どの実体から読んだかの記録がないフォルダの行（出どころを確かめられない行）は返さない。
    /// 消去待ちの索引（[`suspend_search`]）と、索引のファイルを消す前に開いた接続からは、何も返さない
    /// （開き直した接続で検索する）。
    pub fn search(&self, terms: &[String], limit: usize) -> Result<Vec<Hit>> {
        // 検索を終えるまで、索引のファイルを消させない
        let epochs = EPOCHS.read().unwrap_or_else(|e| e.into_inner());
        if let Some(path) = self.path.as_deref() {
            if epoch_in(&epochs, &epoch_key(path)) != self.epoch || is_search_suspended(path) {
                return Ok(Vec::new());
            }
        }
        let mut terms: Vec<String> = terms.iter().map(|t| normalize(t.trim())).filter(|t| !t.is_empty()).collect();
        terms.sort();
        terms.dedup();
        if terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let any = |i: usize| format!("(instr(norm_title, ?{i}) > 0 OR instr(norm_name, ?{i}) > 0 OR instr(norm_body, ?{i}) > 0)");
        let in_name = |i: usize| format!("(instr(norm_title, ?{i}) > 0 OR instr(norm_name, ?{i}) > 0)");
        let matched = (1..=terms.len()).map(any).collect::<Vec<_>>().join(" + ");
        let named = (1..=terms.len()).map(in_name).collect::<Vec<_>>().join(" OR ");
        let sql = format!(
            "SELECT path, folder, title, file_name, body, name_only, modified, matched, named FROM (
               SELECT path, folder, title, file_name, body, name_only, modified, ({matched}) AS matched, ({named}) AS named FROM docs
               WHERE folder IN (SELECT folder FROM bindings)
             ) WHERE matched > 0 ORDER BY matched DESC, named DESC, modified DESC LIMIT {limit}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(terms.iter()), |row| {
            let name_only: bool = row.get(5)?;
            let body: String = row.get(4)?;
            let cut = if name_only { None } else { cut_excerpt(&body, &terms, EXCERPT_CHARS) };
            Ok(Hit {
                path: PathBuf::from(row.get::<_, String>(0)?),
                folder: PathBuf::from(row.get::<_, String>(1)?),
                title: row.get(2)?,
                file_name: row.get(3)?,
                send_excerpt: cut.as_ref().map(|c| c.send.clone()),
                excerpt: cut.map(|c| c.text),
                modified: row.get(6)?,
                matched_terms: row.get::<_, i64>(7)? as usize,
                named: row.get(8)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

/// 索引に入れる、Kiroscribe の過去のノート 1 件（題名・自分のメモ・議事録・文字起こし）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteDoc {
    pub note_id: String,
    pub title: String,
    pub created_at: i64,
    pub memo: String,
    pub minutes: String,
    pub transcript: String,
}

/// 過去のノートの、一致した部分の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotePart {
    Memo,
    Minutes,
    Transcript,
}

impl NotePart {
    pub fn label(self) -> &'static str {
        match self {
            NotePart::Memo => "メモ",
            NotePart::Minutes => "議事録",
            NotePart::Transcript => "文字起こし",
        }
    }
}

/// 検索で見つかった過去のノート。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteHit {
    pub note_id: String,
    pub title: String,
    pub created_at: i64,
    /// 一致した部分の前後と、その部分の種類（題名だけに一致したときは None）
    pub excerpt: Option<(NotePart, String)>,
    /// AI に送る抜粋（[`Hit::send_excerpt`] と同じ作り方）
    pub send_excerpt: Option<String>,
    pub matched_terms: usize,
    /// 題名に一致したか
    pub in_title: bool,
}

impl SearchIndex {
    /// ノートを索引に入れる（あれば置き換える）。
    pub fn upsert_note(&self, note: &NoteDoc) -> Result<()> {
        self.conn.execute(
            "INSERT INTO notes (note_id, title, created_at, memo, minutes, transcript, norm_title, norm_memo, norm_minutes, norm_transcript)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(note_id) DO UPDATE SET title = excluded.title, created_at = excluded.created_at, memo = excluded.memo,
               minutes = excluded.minutes, transcript = excluded.transcript, norm_title = excluded.norm_title,
               norm_memo = excluded.norm_memo, norm_minutes = excluded.norm_minutes, norm_transcript = excluded.norm_transcript",
            params![
                note.note_id,
                note.title,
                note.created_at,
                note.memo,
                note.minutes,
                note.transcript,
                normalize(&note.title),
                normalize(&note.memo),
                normalize(&note.minutes),
                normalize(&note.transcript),
            ],
        )?;
        Ok(())
    }

    pub fn remove_note(&self, note_id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM notes WHERE note_id = ?1", [note_id])?;
        Ok(())
    }

    /// ノートの索引を空にする。
    pub fn clear_notes(&self) -> Result<()> {
        self.conn.execute("DELETE FROM notes", [])?;
        Ok(())
    }

    pub fn count_notes(&self) -> Result<usize> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM notes", [], |row| row.get::<_, i64>(0))? as usize)
    }

    /// 索引に入っているノートの ID。
    pub fn note_ids(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT note_id FROM notes")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// 検索語のどれかを、題名・自分のメモ・議事録・文字起こしに含むノート（`exclude` のノートは除く）。
    /// 並びは、一致した検索語の数、題名に一致したか、作成日時の新しさの順。
    /// 資料の検索（[`SearchIndex::search`]）と同じく、消去待ちの索引と、古い世代の接続からは何も返さない。
    pub fn search_notes(&self, terms: &[String], exclude: &[String], limit: usize) -> Result<Vec<NoteHit>> {
        self.search_notes_by(terms, exclude, limit, |_| ())
    }

    /// [`SearchIndex::search_notes`] と同じく探し、一致したすべてのノートを `key` の大きい順に並べてから `limit` 件にする
    /// （`key` が同じものは、[`SearchIndex::search_notes`] の並び）。抜粋は、残したノートにだけ作る。
    pub fn search_notes_by<K: Ord>(
        &self,
        terms: &[String],
        exclude: &[String],
        limit: usize,
        key: impl Fn(&NoteHit) -> K,
    ) -> Result<Vec<NoteHit>> {
        // 検索を終えるまで、索引のファイルを消させない
        let epochs = EPOCHS.read().unwrap_or_else(|e| e.into_inner());
        if let Some(path) = self.path.as_deref() {
            if epoch_in(&epochs, &epoch_key(path)) != self.epoch || is_search_suspended(path) {
                return Ok(Vec::new());
            }
        }
        let mut terms: Vec<String> = terms.iter().map(|t| normalize(t.trim())).filter(|t| !t.is_empty()).collect();
        terms.sort();
        terms.dedup();
        if terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let n = terms.len();
        let any = |i: usize| {
            format!("(instr(norm_title, ?{i}) > 0 OR instr(norm_memo, ?{i}) > 0 OR instr(norm_minutes, ?{i}) > 0 OR instr(norm_transcript, ?{i}) > 0)")
        };
        let matched = (1..=n).map(any).collect::<Vec<_>>().join(" + ");
        let titled = (1..=n).map(|i| format!("instr(norm_title, ?{i}) > 0")).collect::<Vec<_>>().join(" OR ");
        let excluded = if exclude.is_empty() {
            String::new()
        } else {
            format!("WHERE note_id NOT IN ({})", (n + 1..=n + exclude.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", "))
        };
        // 一致したノートは、本文を読まずにすべて並べる（時期の手がかりで上げるノートを、件数の上限で落とさないように）
        let sql = format!(
            "SELECT note_id, title, created_at, matched, titled FROM (
               SELECT note_id, title, created_at, ({matched}) AS matched, ({titled}) AS titled FROM notes {excluded}
             ) WHERE matched > 0 ORDER BY matched DESC, titled DESC, created_at DESC"
        );
        let values: Vec<&str> = terms.iter().map(String::as_str).chain(exclude.iter().map(String::as_str)).collect();
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values.iter()), |row| {
            Ok(NoteHit {
                note_id: row.get(0)?,
                title: row.get(1)?,
                created_at: row.get(2)?,
                excerpt: None,
                send_excerpt: None,
                matched_terms: row.get::<_, i64>(3)? as usize,
                in_title: row.get(4)?,
            })
        })?;
        let mut hits: Vec<NoteHit> = rows.collect::<std::result::Result<_, _>>()?;
        // 並べ替えは安定なので、鍵が同じものは、一致した検索語の数などの並びのまま
        hits.sort_by_cached_key(|hit| std::cmp::Reverse(key(hit)));
        hits.truncate(limit);
        let mut body = self.conn.prepare("SELECT memo, minutes, transcript FROM notes WHERE note_id = ?1")?;
        for hit in &mut hits {
            let parts = body.query_row([&hit.note_id], |row| {
                Ok([
                    (NotePart::Memo, row.get::<_, String>(0)?),
                    (NotePart::Minutes, row.get::<_, String>(1)?),
                    (NotePart::Transcript, row.get::<_, String>(2)?),
                ])
            })?;
            if let Some((part, cut)) = note_excerpt(&parts, &terms) {
                hit.excerpt = Some((part, cut.text));
                hit.send_excerpt = Some(cut.send);
            }
        }
        Ok(hits)
    }
}

/// 一致した語が最も集まっている部分（600 文字の中の一致の多さで比べる。同じならメモ・議事録・文字起こしの順に優先）の抜粋。
fn note_excerpt(parts: &[(NotePart, String)], terms: &[String]) -> Option<(NotePart, Excerpt)> {
    parts
        .iter()
        .filter_map(|(part, text)| {
            let (score, cut) = scored_excerpt(text, terms, EXCERPT_CHARS)?;
            (score.0 > 0).then_some((score, *part, cut))
        })
        .max_by_key(|(score, part, _)| (*score, std::cmp::Reverse(*part as u8)))
        .map(|(_, part, cut)| (part, cut))
}

/// 切り出した抜粋: 画面に出すもの（`text`）と、AI に送るもの（`send`。URL のクエリとフラグメントを除いたもの）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Excerpt {
    pub text: String,
    pub send: String,
}

/// 抜粋の範囲の一致の多さ（異なる検索語の数、一致の数）。大きいほど、よく一致している。
type Score = (usize, usize);

/// 中身のうち、検索語（正規化したもの）が最も集まっている部分の前後を、`max_chars` 文字までで返す。
/// 中身に検索語がなければ（題名やファイル名だけに一致したとき）、先頭から返す。
pub fn excerpt(body: &str, terms: &[String], max_chars: usize) -> Option<String> {
    cut_excerpt(body, terms, max_chars).map(|cut| cut.text)
}

/// [`excerpt`] と同じ範囲を、画面に出す形と AI に送る形の両方で返す。
pub fn cut_excerpt(body: &str, terms: &[String], max_chars: usize) -> Option<Excerpt> {
    scored_excerpt(body, terms, max_chars).map(|(_, cut)| cut)
}

/// 抜粋と、その範囲の一致の多さ。
fn scored_excerpt(body: &str, terms: &[String], max_chars: usize) -> Option<(Score, Excerpt)> {
    // 空白をまとめてから切り出す（空白が続く部分で、文字数の上限を使い切らないように）
    let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let original: Vec<char> = body.chars().collect();
    if original.is_empty() {
        return None;
    }
    let (normalized, map) = normalize_with_map(&body);
    let normalized: Vec<char> = normalized.chars().collect();
    // 一致した位置（元の文字の位置。始まりと終わり）と、どの検索語か
    let mut hits: Vec<(usize, usize, usize)> = Vec::new();
    for (index, term) in terms.iter().enumerate() {
        let needle: Vec<char> = term.chars().collect();
        if needle.is_empty() || needle.len() > normalized.len() {
            continue;
        }
        // 語ごとに上限を付ける（よく出る語が、ほかの語の一致を数えさせなくしないように）
        let mut found = 0;
        let mut i = 0;
        while i + needle.len() <= normalized.len() && found < MAX_HITS_PER_TERM {
            if normalized[i..i + needle.len()] == needle[..] {
                let end = map.get(i + needle.len()).copied().unwrap_or(original.len());
                hits.push((map[i], end, index));
                found += 1;
                i += needle.len();
            } else {
                i += 1;
            }
        }
    }
    hits.sort();
    let (score, start, end) = match densest(&hits, terms.len(), max_chars) {
        None => ((0, 0), 0, original.len().min(max_chars)),
        Some((score, first, last)) => {
            let (from, to) = (hits[first].0, hits[last].1.max(hits[first].1));
            let center = (from + to) / 2;
            let start = center.saturating_sub(max_chars / 2);
            let end = (start + max_chars).min(original.len());
            (score, end.saturating_sub(max_chars), end)
        }
    };
    // 前後の空白を除く
    let mut start = start;
    let mut end = end;
    while start < end && original[start].is_whitespace() {
        start += 1;
    }
    while end > start && original[end - 1].is_whitespace() {
        end -= 1;
    }
    if start == end {
        return None;
    }
    // URL は本文の中で見つける（抜粋の端で URL が切れても、クエリとフラグメントを送らないように）
    let spans = url::query_spans(&original);
    let text: String = original[start..end].iter().collect();
    let send = url::without_spans(&original, start, end, &spans).trim().to_string();
    Some((score, Excerpt { text, send }))
}

/// 幅 `max_chars` の中に入る一致の並びのうち、異なる検索語の数、一致の数の順に最も多いもの（多さ、最初、最後）。
fn densest(hits: &[(usize, usize, usize)], terms: usize, max_chars: usize) -> Option<(Score, usize, usize)> {
    let mut best: Option<(Score, usize, usize)> = None;
    let mut counts = vec![0usize; terms];
    let mut distinct = 0;
    // 窓に入っている一致は hits[first..next]
    let mut next = 0;
    for first in 0..hits.len() {
        if next <= first {
            next = first;
        }
        while next < hits.len() && (next == first || hits[next].1 - hits[first].0 <= max_chars) {
            counts[hits[next].2] += 1;
            if counts[hits[next].2] == 1 {
                distinct += 1;
            }
            next += 1;
        }
        let score = (distinct, next - first);
        if best.is_none_or(|(b, _, _)| score > b) {
            best = Some((score, first, next - 1));
        }
        counts[hits[first].2] -= 1;
        if counts[hits[first].2] == 0 {
            distinct -= 1;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::extract::Kind;

    fn note(id: &str, title: &str, created_at: i64, memo: &str, minutes: &str, transcript: &str) -> NoteDoc {
        NoteDoc {
            note_id: id.into(),
            title: title.into(),
            created_at,
            memo: memo.into(),
            minutes: minutes.into(),
            transcript: transcript.into(),
        }
    }

    // [IDX-016][IDX-019]
    #[test]
    fn notes_are_searched_in_title_memo_minutes_and_transcript() {
        let index = SearchIndex::open_in_memory().unwrap();
        index.upsert_note(&note("a", "予算の定例", 100, "- 予算の確認", "", "")).unwrap();
        index.upsert_note(&note("b", "週次", 200, "", "## 決定事項\n- 予算はＡ案", "")).unwrap();
        index.upsert_note(&note("c", "採用", 300, "", "", "予算の話をしました")).unwrap();
        index.upsert_note(&note("rec", "録音中", 400, "予算", "", "")).unwrap();
        let hits = index.search_notes(&terms(&["予算"]), &["rec".to_string()], 8).unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.note_id.as_str()).collect();
        assert_eq!(ids, ["a", "c", "b"], "題名に一致したもの、作成日時の新しいものの順。録音中のノートは除く");
        let kind = |id: &str| hits.iter().find(|h| h.note_id == id).unwrap().excerpt.as_ref().map(|(p, _)| p.label());
        assert_eq!(kind("a"), Some("メモ"));
        assert_eq!(kind("b"), Some("議事録"));
        assert_eq!(kind("c"), Some("文字起こし"));
        assert!(hits[2].excerpt.as_ref().unwrap().1.contains("予算はＡ案"));
        // 題名だけに一致したら、抜粋はない
        assert_eq!(index.search_notes(&terms(&["定例"]), &[], 8).unwrap()[0].excerpt, None);

        index.remove_note("a").unwrap();
        assert_eq!(index.count_notes().unwrap(), 3);
        index.clear_notes().unwrap();
        assert_eq!(index.count_notes().unwrap(), 0);
    }

    fn entry(path: &str, modified: i64) -> FileEntry {
        FileEntry { path: PathBuf::from(path), kind: Kind::Text, modified, size: 1, online_only: false }
    }

    fn content(title: Option<&str>, body: &str) -> Extracted {
        Extracted { title: title.map(str::to_string), body: body.into(), name_only: false }
    }

    fn terms(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn bound(index: &SearchIndex, folder: &str) {
        index.rebind(Path::new(folder), &Binding { place: "p".into(), root: "r".into() }).unwrap();
    }

    fn paths(hits: &[Hit]) -> Vec<String> {
        hits.iter().map(|h| h.path.to_string_lossy().to_string()).collect()
    }

    /// クラウドにだけあるかが変わったら、更新日時と大きさが同じでも、目印が変わる（読み直す）。
    // [IDX-027][IDX-009]
    #[test]
    fn stamp_includes_whether_the_file_is_online_only() {
        let index = SearchIndex::open_in_memory().unwrap();
        let folder = Path::new("/r");
        let mut file = entry("/r/a.txt", 5);
        file.online_only = true;
        index.upsert(folder, &file, &Extracted::name_only()).unwrap();
        assert_eq!(index.stamp(&file.path).unwrap(), Some((5, 1, true)));
        assert_eq!(index.known_files(folder).unwrap()[&file.path], (5, 1, true));
        file.online_only = false;
        assert_ne!(index.stamp(&file.path).unwrap(), Some(stamp_of_file(&file)), "手元に来たら読み直す");
    }

    // [IDX-013]
    #[test]
    fn substring_search_ignores_case_and_width() {
        let index = SearchIndex::open_in_memory().unwrap();
        let folder = Path::new("/r");
        bound(&index, "/r");
        index.upsert(folder, &entry("/r/規程.txt", 1), &content(None, "経費の精算は月末までに申請する")).unwrap();
        index.upsert(folder, &entry("/r/VPN.md", 2), &content(Some("ＶＰＮの設定"), "社内の Wiki を見る")).unwrap();
        index.upsert(folder, &entry("/r/無関係.txt", 3), &content(None, "昼食の予定")).unwrap();

        assert_eq!(paths(&index.search(&terms(&["精算"]), 8).unwrap()), ["/r/規程.txt"], "2 文字の語も部分一致で探せる");
        assert_eq!(paths(&index.search(&terms(&["vpn"]), 8).unwrap()), ["/r/VPN.md"], "全角の題名に小文字の半角で一致");
        assert_eq!(paths(&index.search(&terms(&["ＷＩＫＩ"]), 8).unwrap()), ["/r/VPN.md"], "中身に全角の大文字で一致");
        assert_eq!(paths(&index.search(&terms(&["規程"]), 8).unwrap()), ["/r/規程.txt"], "ファイル名に一致");
        assert!(index.search(&terms(&["どこにもない"]), 8).unwrap().is_empty());
        assert!(index.search(&terms(&["  "]), 8).unwrap().is_empty());
    }

    // [IDX-014]
    #[test]
    fn ranks_by_matched_terms_then_title_then_recency() {
        let index = SearchIndex::open_in_memory().unwrap();
        let folder = Path::new("/r");
        bound(&index, "/r");
        index.upsert(folder, &entry("/r/a.txt", 100), &content(None, "経費 と 精算 の 手順")).unwrap();
        index.upsert(folder, &entry("/r/b.txt", 300), &content(None, "経費 だけ")).unwrap();
        index.upsert(folder, &entry("/r/c.txt", 200), &content(Some("経費の規程"), "本文")).unwrap();
        index.upsert(folder, &entry("/r/d.txt", 400), &content(None, "経費 もう一つ")).unwrap();
        let hits = index.search(&terms(&["経費", "精算"]), 8).unwrap();
        assert_eq!(paths(&hits), ["/r/a.txt", "/r/c.txt", "/r/d.txt", "/r/b.txt"]);
        assert_eq!(hits[0].matched_terms, 2);
        assert_eq!(index.search(&terms(&["経費"]), 2).unwrap().len(), 2, "件数の上限");
    }

    // [IDX-015]
    #[test]
    fn excerpt_is_around_the_densest_matches_and_at_most_600_chars() {
        let body = format!("{}経費の話。{}精算と経費と申請。{}", "前置き".repeat(300), "あ".repeat(800), "後書き".repeat(300));
        let text = excerpt(&body, &terms(&["経費", "精算", "申請"]), EXCERPT_CHARS).unwrap();
        assert!(text.chars().count() <= EXCERPT_CHARS);
        assert!(text.contains("精算と経費と申請"), "一致が最も集まっている部分: {text}");
        assert!(!text.contains("経費の話"), "離れた一致は入らない");

        let short = excerpt("短い本文で経費。", &terms(&["経費"]), EXCERPT_CHARS).unwrap();
        assert_eq!(short, "短い本文で経費。");

        let index = SearchIndex::open_in_memory().unwrap();
        let folder = Path::new("/r");
        bound(&index, "/r");
        let name_only = Extracted { title: None, body: String::new(), name_only: true };
        index.upsert(folder, &entry("/r/経費.png", 1), &name_only).unwrap();
        let hit = &index.search(&terms(&["経費"]), 8).unwrap()[0];
        assert_eq!(hit.excerpt, None, "名前だけで探した資料には抜粋を付けない");
        assert_eq!(hit.title, "経費", "文書の中の題名がなければ、拡張子を除いたファイル名");
    }

    // [IDX-010]
    #[test]
    fn remove_folder_and_paths() {
        let index = SearchIndex::open_in_memory().unwrap();
        bound(&index, "/a");
        bound(&index, "/b");
        index.upsert(Path::new("/a"), &entry("/a/1.txt", 1), &content(None, "x")).unwrap();
        index.upsert(Path::new("/a"), &entry("/a/2.txt", 1), &content(None, "x")).unwrap();
        index.upsert(Path::new("/b"), &entry("/b/3.txt", 1), &content(None, "x")).unwrap();
        assert_eq!(index.count_folder(Path::new("/a")).unwrap(), 2);
        index.remove_path(Path::new("/a/1.txt")).unwrap();
        assert_eq!(index.count_folder(Path::new("/a")).unwrap(), 1);
        index.remove_folder(Path::new("/a")).unwrap();
        assert_eq!(index.count().unwrap(), 1);
        assert_eq!(index.binding(Path::new("/a")).unwrap(), None, "記録も消す");
        assert_eq!(index.folders().unwrap(), [PathBuf::from("/b")]);
        index.clear().unwrap();
        assert_eq!(index.count().unwrap(), 0);
    }

    /// どの実体から読んだかの記録がないフォルダの行は、検索に出さず、件数にも数えない。
    /// 記録し直すと、それまでの行を消す（出どころを確かめられない行を、新しい記録の行として使わない）。
    // [IDX-027]
    #[test]
    fn rows_without_a_binding_are_not_searched() {
        let index = SearchIndex::open_in_memory().unwrap();
        let folder = Path::new("/r");
        index.upsert(folder, &entry("/r/a.txt", 1), &content(None, "経費")).unwrap();
        assert!(index.search(&terms(&["経費"]), 8).unwrap().is_empty(), "記録がない");
        assert_eq!(index.count_folder(folder).unwrap(), 0);
        assert_eq!(index.stored_rows(folder).unwrap(), 1);
        assert_eq!(index.folders().unwrap(), [PathBuf::from("/r")], "消すために、記録のないフォルダも返す");

        let binding = Binding { place: "p".into(), root: "r".into() };
        index.rebind(folder, &binding).unwrap();
        assert_eq!(index.stored_rows(folder).unwrap(), 0, "記録し直すと、それまでの行を消す");
        index.upsert(folder, &entry("/r/a.txt", 1), &content(None, "経費")).unwrap();
        assert_eq!(paths(&index.search(&terms(&["経費"]), 8).unwrap()), ["/r/a.txt"]);
        assert_eq!(index.binding(folder).unwrap(), Some(binding));

        index.unbind(folder).unwrap();
        assert!(index.search(&terms(&["経費"]), 8).unwrap().is_empty(), "記録を外すと、検索に出さない");
        assert_eq!(index.stored_rows(folder).unwrap(), 1, "行は残る（あとで消す）");
    }

    /// 消去待ちの索引からは検索しない。記録のファイルが残っていれば、別のプロセス（起動し直したあと）でも検索しない。
    /// 消去待ちを解くと、検索を再開する。
    // [IDX-027]
    #[test]
    fn a_suspended_index_is_not_searched_until_resumed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data").join(INDEX_FILE);
        let index = SearchIndex::open(&path).unwrap();
        bound(&index, "/r");
        index.upsert(Path::new("/r"), &entry("/r/a.txt", 1), &content(None, "経費")).unwrap();
        assert_eq!(index.search(&terms(&["経費"]), 8).unwrap().len(), 1);

        suspend_search(&path).unwrap();
        assert!(erase_marker(&path).exists(), "消去待ちを記録する");
        assert!(index.search(&terms(&["経費"]), 8).unwrap().is_empty(), "開いていた接続からも検索しない");
        // 起動し直したあと（このプロセスの記憶がない）も、記録のファイルがあれば検索しない
        SUSPENDED.lock().unwrap().retain(|p| p != &path);
        assert!(is_search_suspended(&path));
        assert!(SearchIndex::open(&path).unwrap().search(&terms(&["経費"]), 8).unwrap().is_empty());
        index.close().unwrap();
        remove_index_files(&path).unwrap();
        assert!(erase_marker(&path).exists(), "索引のファイルを消しても、記録は消さない");

        resume_search(&path).unwrap();
        assert!(!is_search_suspended(&path));
        assert!(!erase_marker(&path).exists());
        let reopened = SearchIndex::open(&path).unwrap();
        bound(&reopened, "/r");
        reopened.upsert(Path::new("/r"), &entry("/r/a.txt", 1), &content(None, "経費")).unwrap();
        assert_eq!(reopened.search(&terms(&["経費"]), 8).unwrap().len(), 1, "消去待ちを解いたら検索する");
        assert_eq!(SearchIndex::open_in_memory().unwrap().path, None, "メモリの中の索引は、消去待ちにしない");
    }

    /// 過去のノートも、資料と同じく、消去待ちの索引と、ファイルを消す前に開いた検索用の接続からは探さない。
    // [IDX-017][IDX-028]
    #[test]
    fn notes_are_not_searched_from_a_suspended_or_removed_index() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data").join(INDEX_FILE);
        let index = SearchIndex::open(&path).unwrap();
        index.upsert_note(&note("a", "予算の定例", 1, "予算", "", "")).unwrap();
        let reader = SearchIndex::open_existing(&path).unwrap();
        assert!(reader.is_current());
        assert_eq!(reader.search_notes(&terms(&["予算"]), &[], 8).unwrap().len(), 1);

        suspend_search(&path).unwrap();
        assert!(reader.search_notes(&terms(&["予算"]), &[], 8).unwrap().is_empty(), "消去待ちのあいだは探さない");
        resume_search(&path).unwrap();
        assert_eq!(reader.search_notes(&terms(&["予算"]), &[], 8).unwrap().len(), 1);

        index.close().unwrap();
        // Windows では、検索用の接続が開いているとファイルを消せないことがある。消せなくても、それまでの接続からは探さない
        let removed = remove_index_files(&path);
        #[cfg(unix)]
        removed.as_ref().unwrap();
        assert!(!reader.is_current(), "ファイルを消したら、開き直す");
        assert!(reader.search_notes(&terms(&["予算"]), &[], 8).unwrap().is_empty());
        // 接続を閉じれば消せる
        reader.close().unwrap();
        remove_index_files(&path).unwrap();
        assert!(index_files(&path).iter().all(|file| !file.exists()));
    }

    /// 索引のファイルを消す前に開いた接続は、消したファイルの中身を読み続けられても、検索に使わない
    /// （同じパスで作り直した索引の中身は、開き直した接続で検索する）。
    // [IDX-027][IDX-028]
    #[test]
    fn connections_opened_before_the_files_were_removed_are_not_searched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data").join(INDEX_FILE);
        let held = SearchIndex::open(&path).unwrap();
        bound(&held, "/r");
        held.upsert(Path::new("/r"), &entry("/r/a.txt", 1), &content(None, "経費")).unwrap();
        assert_eq!(held.search(&terms(&["経費"]), 8).unwrap().len(), 1);

        // Windows では、開いている接続があるとファイルを消せないことがある。消せなくても、それまでの接続は検索に使わない
        let removed = remove_index_files(&path);
        #[cfg(unix)]
        {
            removed.as_ref().unwrap();
            assert_eq!(held.stored_rows(Path::new("/r")).unwrap(), 1, "消したファイルの行が、開いていた接続から見える");
        }
        assert!(held.search(&terms(&["経費"]), 8).unwrap().is_empty(), "消す前に開いた接続からは検索しない");

        let reopened = SearchIndex::open(&path).unwrap();
        bound(&reopened, "/r");
        reopened.upsert(Path::new("/r"), &entry("/r/b.txt", 1), &content(None, "予算")).unwrap();
        assert_eq!(paths(&reopened.search(&terms(&["予算"]), 8).unwrap()), ["/r/b.txt"], "開き直した接続で検索する");
        if removed.is_ok() {
            assert!(reopened.search(&terms(&["経費"]), 8).unwrap().is_empty(), "作り直した索引に、前の中身はない");
        }
        assert!(held.search(&terms(&["予算"]), 8).unwrap().is_empty(), "古い接続は、作り直した索引でも検索しない");
    }

    // [IDX-012][DAT-001]
    #[test]
    fn broken_index_file_is_recreated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data").join(INDEX_FILE);
        {
            let index = SearchIndex::open(&path).unwrap();
            index.upsert(Path::new("/a"), &entry("/a/1.txt", 1), &content(None, "x")).unwrap();
            assert_eq!(index.count().unwrap(), 1);
        }
        assert_eq!(SearchIndex::open(&path).unwrap().count().unwrap(), 1, "開き直しても残る");
        remove_index_files(&path).unwrap();
        std::fs::write(&path, b"this is not a sqlite database at all, just garbage bytes......").unwrap();
        let index = SearchIndex::open(&path).unwrap();
        assert_eq!(index.count().unwrap(), 0, "壊れていたら作り直す");
    }

    /// 索引のファイル（本体・-wal・-shm）のどこかに、`marker` の文字が残っているか。
    pub(crate) fn left_on_disk(path: &Path, marker: &str) -> bool {
        index_files(path).iter().any(|file| {
            std::fs::read(file).is_ok_and(|bytes| bytes.windows(marker.len()).any(|w| w == marker.as_bytes()))
        })
    }

    // [IDX-011][IDX-028]
    #[test]
    fn clearing_leaves_no_contents_in_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INDEX_FILE);
        let index = SearchIndex::open(&path).unwrap();
        for i in 0..50 {
            let file = entry(&format!("/a/{i}.txt"), 1);
            index.upsert(Path::new("/a"), &file, &content(None, &format!("ZEBRA-SECRET-{i} {}", "x".repeat(2_000)))).unwrap();
        }
        assert!(left_on_disk(&path, "ZEBRA-SECRET-1"), "消す前は、ファイルにある");
        index.clear().unwrap();
        assert_eq!(index.count().unwrap(), 0);
        assert!(!left_on_disk(&path, "ZEBRA-SECRET"), "空にしたら、本体にも -wal にも残らない");
        index.close().unwrap();
        remove_index_files(&path).unwrap();
        assert!(index_files(&path).iter().all(|file| !file.exists()));
        remove_index_files(&path).unwrap();
    }

    // [IDX-015]
    #[test]
    fn excerpt_boundaries() {
        let exact = "あ".repeat(EXCERPT_CHARS);
        assert_eq!(excerpt(&exact, &terms(&["あ"]), EXCERPT_CHARS).unwrap().chars().count(), EXCERPT_CHARS, "ちょうど 600 文字");
        let over = format!("{}い", "あ".repeat(EXCERPT_CHARS));
        assert_eq!(excerpt(&over, &terms(&["い"]), EXCERPT_CHARS).unwrap().chars().count(), EXCERPT_CHARS, "601 文字なら 600 文字に切る");
        assert!(excerpt(&over, &terms(&["い"]), EXCERPT_CHARS).unwrap().ends_with('い'));
        assert_eq!(excerpt("   \n\t  ", &terms(&["x"]), EXCERPT_CHARS), None, "空白だけ");

        // 先頭の空白で上限を使い切らない
        let spaced = format!("{}経費の手順{}", " ".repeat(2_000), "\n".repeat(2_000));
        assert_eq!(excerpt(&spaced, &terms(&["どこにもない"]), EXCERPT_CHARS).unwrap(), "経費の手順");

        // よく出る語があっても、ほかの語が集まっている部分を選ぶ
        let body = format!("{}経費と精算と申請", format!("また{}", "ー".repeat(30)).repeat(MAX_HITS_PER_TERM + 10));
        let text = excerpt(&body, &terms(&["また", "経費", "精算", "申請"]), 20).unwrap();
        assert!(text.contains("経費と精算と申請"), "{text}");
    }

    /// 一致したノートをすべて鍵で並べてから件数を絞る（一致した語の少ないノートでも、鍵が大きければ残す）。抜粋は残したものだけに作る。
    // [IDX-018]
    #[test]
    fn notes_are_ranked_by_the_key_before_the_limit() {
        let index = SearchIndex::open_in_memory().unwrap();
        for i in 0..9 {
            index.upsert_note(&note(&format!("n{i}"), "採用", i, "予算 経費", "", "")).unwrap();
        }
        index.upsert_note(&note("prev", "週次定例", 100, "予算", "", "")).unwrap();
        let plain = index.search_notes(&terms(&["予算", "経費"]), &[], 8).unwrap();
        assert!(plain.iter().all(|h| h.note_id != "prev"), "一致した語が少ないので、鍵がなければ落ちる");
        let ranked = index.search_notes_by(&terms(&["予算", "経費"]), &[], 8, |h| h.title == "週次定例").unwrap();
        assert_eq!(ranked.len(), 8);
        assert_eq!(ranked[0].note_id, "prev");
        assert!(ranked[0].excerpt.is_some());
        assert_eq!(ranked[1].note_id, "n8", "鍵が同じものは、一致した語の数・新しさの順");
        let excluded = index.search_notes(&terms(&["予算"]), &["prev".to_string(), "n8".to_string()], 20).unwrap();
        assert!(excluded.iter().all(|h| h.note_id != "prev" && h.note_id != "n8"), "除くノートは返さない");
        assert_eq!(excluded.len(), 8);
    }

    /// ノートの抜粋は、600 文字の中の一致の多さで部分を選ぶ（部分全体の語の数では選ばない）。
    // [IDX-015][IDX-019]
    #[test]
    fn note_excerpt_prefers_the_part_with_the_densest_window() {
        let parts = |memo: &str, minutes: &str| [(NotePart::Memo, memo.to_string()), (NotePart::Minutes, minutes.to_string()), (NotePart::Transcript, String::new())];
        let t = terms(&["予算", "経費"]);
        let (part, cut) = note_excerpt(&parts("予算の確認", "予算は据え置き。予算の内訳。予算の上限。"), &t).unwrap();
        assert_eq!(part, NotePart::Minutes, "同じ語が多く集まっている部分");
        assert!(cut.text.contains("予算の上限"));
        let far = format!("予算{}経費", "あ".repeat(EXCERPT_CHARS + 10));
        let (part, _) = note_excerpt(&parts(&far, "予算と経費"), &t).unwrap();
        assert_eq!(part, NotePart::Minutes, "遠く離れた語は、同じ範囲に入らない");
        let (part, _) = note_excerpt(&parts("予算", "予算"), &t).unwrap();
        assert_eq!(part, NotePart::Memo, "同じなら、メモを優先");
    }

    /// 抜粋の端で URL が切れても、本文の中で見つけた URL のクエリとフラグメントは送らない。
    // [CMP-043][IDX-015]
    #[test]
    fn excerpts_to_send_drop_url_queries_found_in_the_whole_body() {
        let body = format!("https://wiki.example.com/page?token=SECRET&id=1 を見る。{}経費の申請", "あ".repeat(EXCERPT_CHARS - 30));
        let cut = cut_excerpt(&body, &terms(&["経費"]), EXCERPT_CHARS).unwrap();
        assert!(!cut.text.starts_with("https://"), "抜粋の左端が URL の途中: {}", &cut.text[..40]);
        assert!(cut.text.contains("SECRET"), "画面に出す抜粋はそのまま");
        assert!(!cut.send.contains("SECRET") && !cut.send.contains("id=1"), "{}", cut.send);
        assert!(cut.send.ends_with("経費の申請"));

        let index = SearchIndex::open_in_memory().unwrap();
        bound(&index, "/r");
        index.upsert(Path::new("/r"), &entry("/r/a.txt", 1), &content(None, "手順は HTTPS://Wiki.example.com/x?Sig=abc#top を見る")).unwrap();
        let hit = &index.search(&terms(&["手順"]), 8).unwrap()[0];
        assert_eq!(hit.send_excerpt.as_deref(), Some("手順は HTTPS://Wiki.example.com/x を見る"));
        index.upsert_note(&note("a", "定例", 1, "手順 https://a.example.com/b?k=1", "", "")).unwrap();
        let note = &index.search_notes(&terms(&["手順"]), &[], 8).unwrap()[0];
        assert_eq!(note.send_excerpt.as_deref(), Some("手順 https://a.example.com/b"));
    }

    /// 版 3（ノートの表がない）の索引は、資料とフォルダの記録を残したまま、ノートの表を足す。
    // [IDX-009][IDX-010][IDX-012]
    #[test]
    fn version_three_index_keeps_documents_and_bindings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(INDEX_FILE);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(SCHEMA).unwrap();
            conn.pragma_update(None, "user_version", SCHEMA_VERSION_WITHOUT_NOTES).unwrap();
            let index = SearchIndex { conn, path: None, epoch: 0 };
            bound(&index, "/r");
            index.upsert(Path::new("/r"), &entry("/r/a.txt", 1), &content(None, "経費")).unwrap();
        }
        let index = SearchIndex::open(&path).unwrap();
        assert_eq!(paths(&index.search(&terms(&["経費"]), 8).unwrap()), ["/r/a.txt"], "資料を残す");
        assert!(index.binding(Path::new("/r")).unwrap().is_some(), "フォルダの記録を残す");
        index.upsert_note(&note("n", "定例", 1, "経費", "", "")).unwrap();
        assert_eq!(index.search_notes(&terms(&["経費"]), &[], 8).unwrap().len(), 1, "ノートの表を足す");
        let version: i64 = index.conn.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        // 知らない版は、捨てて作り直す
        index.conn.pragma_update(None, "user_version", 2).unwrap();
        drop(index);
        assert_eq!(SearchIndex::open(&path).unwrap().count().unwrap(), 0);
    }
}
