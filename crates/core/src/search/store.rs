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

const SCHEMA_VERSION: i64 = 1;
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
    size        INTEGER NOT NULL
);
CREATE INDEX docs_folder ON docs(folder);
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
}

fn remove_db_files(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(name));
    }
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
                remove_db_files(path);
                Self::try_open(path)
            }
        }
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn try_open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let ok: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if ok != "ok" {
            return Err(crate::CoreError::Other("索引が壊れています".into()));
        }
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self> {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != SCHEMA_VERSION {
            // 索引は作り直せるので、形が違えば捨てて作る
            conn.execute_batch("DROP TABLE IF EXISTS docs;")?;
            conn.execute_batch(SCHEMA)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Self { conn })
    }

    /// ファイルを索引に入れる（あれば置き換える）。
    pub fn upsert(&self, folder: &Path, file: &FileEntry, content: &Extracted) -> Result<()> {
        let file_name = file.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let stem = file.path.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let title = content.title.clone().unwrap_or(stem);
        self.conn.execute(
            "INSERT INTO docs (source, folder, path, title, file_name, body, norm_title, norm_name, norm_body, name_only, modified, size)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(path) DO UPDATE SET folder = excluded.folder, title = excluded.title, file_name = excluded.file_name,
               body = excluded.body, norm_title = excluded.norm_title, norm_name = excluded.norm_name, norm_body = excluded.norm_body,
               name_only = excluded.name_only, modified = excluded.modified, size = excluded.size",
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
            ],
        )?;
        Ok(())
    }

    /// フォルダの、いま索引にあるファイルと、その更新日時と大きさ。
    pub fn known_files(&self, folder: &Path) -> Result<HashMap<PathBuf, (i64, u64)>> {
        let mut stmt = self.conn.prepare("SELECT path, modified, size FROM docs WHERE folder = ?1")?;
        let rows = stmt.query_map([folder.to_string_lossy()], |row| {
            Ok((PathBuf::from(row.get::<_, String>(0)?), (row.get::<_, i64>(1)?, row.get::<_, i64>(2)? as u64)))
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    pub fn remove_path(&self, path: &Path) -> Result<()> {
        self.conn.execute("DELETE FROM docs WHERE path = ?1", [path.to_string_lossy()])?;
        Ok(())
    }

    /// フォルダのファイルを、すべて索引から消す。
    pub fn remove_folder(&self, folder: &Path) -> Result<()> {
        self.conn.execute("DELETE FROM docs WHERE folder = ?1", [folder.to_string_lossy()])?;
        Ok(())
    }

    /// 索引を空にする。
    pub fn clear(&self) -> Result<()> {
        self.conn.execute("DELETE FROM docs", [])?;
        // 消した内容がファイルに残らないよう、空き領域も詰める
        self.conn.execute_batch("VACUUM;")?;
        Ok(())
    }

    pub fn count_folder(&self, folder: &Path) -> Result<usize> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM docs WHERE folder = ?1", [folder.to_string_lossy()], |row| row.get::<_, i64>(0))?
            as usize)
    }

    pub fn count(&self) -> Result<usize> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM docs", [], |row| row.get::<_, i64>(0))? as usize)
    }

    /// 索引に入っているフォルダ。
    pub fn folders(&self) -> Result<Vec<PathBuf>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT folder FROM docs WHERE source = ?1")?;
        let rows = stmt.query_map([SOURCE_FOLDER], |row| Ok(PathBuf::from(row.get::<_, String>(0)?)))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// そのパスのファイルが索引にあるか。
    pub fn contains(&self, path: &Path) -> Result<bool> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM docs WHERE path = ?1", [path.to_string_lossy()], |row| row.get::<_, i64>(0))? > 0)
    }

    /// 検索語のどれかを、題名・ファイル名・中身に含む資料を、よく一致する順に `limit` 件まで返す。
    /// 並びは、一致した検索語の数、題名かファイル名に一致したか、更新日時の新しさの順。
    pub fn search(&self, terms: &[String], limit: usize) -> Result<Vec<Hit>> {
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
    let original: Vec<char> = body.chars().collect();
    if original.is_empty() {
        return None;
    }
    let (normalized, map) = normalize_with_map(body);
    let normalized: Vec<char> = normalized.chars().collect();
    // 一致した位置（元の文字の位置。始まりと終わり）
    let mut hits: Vec<(usize, usize)> = Vec::new();
    for term in terms {
        let needle: Vec<char> = term.chars().collect();
        if needle.is_empty() || needle.len() > normalized.len() {
            continue;
        }
        let mut i = 0;
        while i + needle.len() <= normalized.len() && hits.len() < 5_000 {
            if normalized[i..i + needle.len()] == needle[..] {
                let end = map.get(i + needle.len()).copied().unwrap_or(original.len());
                hits.push((map[i], end));
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
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::extract::Kind;

    fn entry(path: &str, modified: i64) -> FileEntry {
        FileEntry { path: PathBuf::from(path), kind: Kind::Text, modified, size: 1 }
    }

    fn content(title: Option<&str>, body: &str) -> Extracted {
        Extracted { title: title.map(str::to_string), body: body.into(), name_only: false }
    }

    fn terms(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn paths(hits: &[Hit]) -> Vec<String> {
        hits.iter().map(|h| h.path.to_string_lossy().to_string()).collect()
    }

    // [IDX-013]
    #[test]
    fn substring_search_ignores_case_and_width() {
        let index = SearchIndex::open_in_memory().unwrap();
        let folder = Path::new("/r");
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
        index.upsert(Path::new("/a"), &entry("/a/1.txt", 1), &content(None, "x")).unwrap();
        index.upsert(Path::new("/a"), &entry("/a/2.txt", 1), &content(None, "x")).unwrap();
        index.upsert(Path::new("/b"), &entry("/b/3.txt", 1), &content(None, "x")).unwrap();
        assert_eq!(index.count_folder(Path::new("/a")).unwrap(), 2);
        index.remove_path(Path::new("/a/1.txt")).unwrap();
        assert_eq!(index.count_folder(Path::new("/a")).unwrap(), 1);
        index.remove_folder(Path::new("/a")).unwrap();
        assert_eq!(index.count().unwrap(), 1);
        index.clear().unwrap();
        assert_eq!(index.count().unwrap(), 0);
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
        remove_db_files(&path);
        std::fs::write(&path, b"this is not a sqlite database at all, just garbage bytes......").unwrap();
        let index = SearchIndex::open(&path).unwrap();
        assert_eq!(index.count().unwrap(), 0, "壊れていたら作り直す");
    }
}
