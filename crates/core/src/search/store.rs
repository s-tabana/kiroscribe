//! 資料の索引の保存先（アプリのデータフォルダの `search-index.db`。ノートのデータベースとは別のファイル）と検索。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{params, params_from_iter, Connection};

use super::extract::Extracted;
use super::normalize::{normalize, normalize_with_map};
use super::walk::FileEntry;
use crate::error::Result;

/// 索引のファイルの名前。
pub const INDEX_FILE: &str = "search-index.db";
/// 抜粋の長さの上限（文字）。
pub const EXCERPT_CHARS: usize = 600;
/// 抜粋の位置を決めるときに数える、1 つの検索語の一致の数の上限。
const MAX_HITS_PER_TERM: usize = 5_000;

const SCHEMA_VERSION: i64 = 3;
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
    pub modified: i64,
    /// 一致した検索語の数
    pub matched_terms: usize,
}

pub struct SearchIndex {
    conn: Connection,
    /// 索引のファイルのパス（メモリの中の索引は None）。消去待ちのあいだは検索しない（[`is_search_suspended`]）
    path: Option<PathBuf>,
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
    std::fs::write(&marker, b"erase\n")?;
    std::fs::File::open(&marker)?.sync_all()
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

/// 索引のファイルをすべて消す（もともとないファイルは、消せたものとみなす）。
pub fn remove_index_files(path: &Path) -> std::io::Result<()> {
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

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?, None)
    }

    fn try_open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let ok: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if ok != "ok" {
            return Err(crate::CoreError::Other("索引が壊れています".into()));
        }
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn, Some(path.to_path_buf()))
    }

    fn init(conn: Connection, path: Option<PathBuf>) -> Result<Self> {
        // 消した行の中身を、ファイルの空き領域に残さない
        conn.execute_batch("PRAGMA secure_delete = ON;")?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != SCHEMA_VERSION {
            // 索引は作り直せるので、形が違えば捨てて作る
            conn.execute_batch("DROP TABLE IF EXISTS docs; DROP TABLE IF EXISTS bindings;")?;
            conn.execute_batch(SCHEMA)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Self { conn, path })
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
        self.conn.execute_batch("DELETE FROM docs; DELETE FROM bindings;")?;
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
    /// 消去待ちの索引（[`suspend_search`]）からは、何も返さない。
    pub fn search(&self, terms: &[String], limit: usize) -> Result<Vec<Hit>> {
        if self.path.as_deref().is_some_and(is_search_suspended) {
            return Ok(Vec::new());
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
            "SELECT path, folder, title, file_name, body, name_only, modified, matched FROM (
               SELECT path, folder, title, file_name, body, name_only, modified, ({matched}) AS matched, ({named}) AS named FROM docs
               WHERE folder IN (SELECT folder FROM bindings)
             ) WHERE matched > 0 ORDER BY matched DESC, named DESC, modified DESC LIMIT {limit}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(terms.iter()), |row| {
            let name_only: bool = row.get(5)?;
            let body: String = row.get(4)?;
            Ok(Hit {
                path: PathBuf::from(row.get::<_, String>(0)?),
                folder: PathBuf::from(row.get::<_, String>(1)?),
                title: row.get(2)?,
                file_name: row.get(3)?,
                excerpt: if name_only { None } else { excerpt(&body, &terms, EXCERPT_CHARS) },
                modified: row.get(6)?,
                matched_terms: row.get::<_, i64>(7)? as usize,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

/// 中身のうち、検索語（正規化したもの）が最も集まっている部分の前後を、`max_chars` 文字までで返す。
/// 中身に検索語がなければ（題名やファイル名だけに一致したとき）、先頭から返す。
pub fn excerpt(body: &str, terms: &[String], max_chars: usize) -> Option<String> {
    // 空白をまとめてから切り出す（空白が続く部分で、文字数の上限を使い切らないように）
    let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let original: Vec<char> = body.chars().collect();
    if original.is_empty() {
        return None;
    }
    let (normalized, map) = normalize_with_map(&body);
    let normalized: Vec<char> = normalized.chars().collect();
    // 一致した位置（元の文字の位置。始まりと終わり）
    let mut hits: Vec<(usize, usize)> = Vec::new();
    for term in terms {
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
                hits.push((map[i], end));
                found += 1;
                i += needle.len();
            } else {
                i += 1;
            }
        }
    }
    hits.sort();
    let (start, end) = if hits.is_empty() {
        (0, original.len().min(max_chars))
    } else {
        // 幅 max_chars の中に入る一致の数が最も多い並びを選ぶ
        let mut best = (0, 0, 0); // (数, 最初, 最後)
        let mut last = 0;
        for first in 0..hits.len() {
            last = last.max(first);
            while last + 1 < hits.len() && hits[last + 1].1 - hits[first].0 <= max_chars {
                last += 1;
            }
            let count = last - first + 1;
            if count > best.0 {
                best = (count, first, last);
            }
        }
        let (from, to) = (hits[best.1].0, hits[best.2].1.max(hits[best.1].1));
        let center = (from + to) / 2;
        let start = center.saturating_sub(max_chars / 2);
        let end = (start + max_chars).min(original.len());
        (end.saturating_sub(max_chars), end)
    };
    let text: String = original[start..end].iter().collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::extract::Kind;

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
}
