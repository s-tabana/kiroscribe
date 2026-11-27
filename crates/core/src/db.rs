//! SQLite の接続とスキーマ管理。
//!
//! スキーマのバージョンは `PRAGMA user_version` で管理し、起動時に未適用のマイグレーションを順に流す。

use std::path::Path;

use rusqlite::Connection;

use crate::error::Result;

/// 適用順に並べたマイグレーション。既存の要素は変更せず、末尾に追加していく。
const MIGRATIONS: &[&str] = &[
    // v1: 初期スキーマ
    r#"
    CREATE TABLE notes (
        id              TEXT PRIMARY KEY,
        title           TEXT NOT NULL DEFAULT '',
        user_notes      TEXT NOT NULL DEFAULT '',
        enhanced_notes  TEXT,
        enhanced_at     INTEGER,
        created_at      INTEGER NOT NULL,
        updated_at      INTEGER NOT NULL
    );
    CREATE INDEX idx_notes_updated_at ON notes(updated_at DESC);

    CREATE TABLE transcript_segments (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        note_id     TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
        speaker     TEXT NOT NULL CHECK (speaker IN ('me', 'them')),
        started_at  INTEGER NOT NULL,
        ended_at    INTEGER NOT NULL,
        text        TEXT NOT NULL
    );
    CREATE INDEX idx_segments_note ON transcript_segments(note_id, started_at);

    CREATE TABLE chat_messages (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        note_id     TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
        role        TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
        content     TEXT NOT NULL,
        created_at  INTEGER NOT NULL
    );
    CREATE INDEX idx_chat_note ON chat_messages(note_id, id);

    CREATE TABLE settings (
        key    TEXT PRIMARY KEY,
        value  TEXT NOT NULL
    );
    "#,
    // v2: 文字起こしの区切り（同じノートで録音を再開した位置）。区切りのないノートは 1 回分の録音として扱う
    r#"
    CREATE TABLE transcript_breaks (
        id          INTEGER PRIMARY KEY AUTOINCREMENT,
        note_id     TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
        round       INTEGER NOT NULL,
        started_at  INTEGER NOT NULL
    );
    CREATE INDEX idx_breaks_note ON transcript_breaks(note_id, started_at);
    "#,
];

/// アプリのデータベース。`rusqlite::Connection` は `Sync` ではないため、呼び出し側で `Mutex` に包む。
pub struct Database {
    conn: Connection,
}

impl Database {
    /// ファイルを開き（なければ作成し）、マイグレーションを適用する。
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::init(conn)
    }

    /// テスト用のインメモリデータベース。
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let mut db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&mut self) -> Result<()> {
        let current: usize = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?
            as usize;
        for (index, sql) in MIGRATIONS.iter().enumerate().skip(current) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", (index + 1) as i64)?;
            tx.commit()?;
        }
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // [DAT-001][DAT-002]
    #[test]
    fn migrations_apply_once_and_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("kiroscribe.db");
        {
            let db = Database::open(&path).unwrap();
            assert_eq!(db.schema_version().unwrap(), MIGRATIONS.len() as i64);
        }
        // 2 回目に開いても再適用されずに成功する
        let db = Database::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), MIGRATIONS.len() as i64);
    }

    // [DAT-010][DAT-002]
    #[test]
    fn notes_from_before_breaks_open_as_a_single_round() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kiroscribe.db");
        {
            // 区切りの表がない版（v1）のデータベース
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(MIGRATIONS[0]).unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
            conn.execute_batch(
                "INSERT INTO notes (id, title, user_notes, enhanced_notes, created_at, updated_at)
                     VALUES ('old', '定例', '- メモ', '## 決定事項', 1, 2);
                 INSERT INTO transcript_segments (note_id, speaker, started_at, ended_at, text)
                     VALUES ('old', 'me', 100, 200, '以前の発話');",
            )
            .unwrap();
        }
        let db = Database::open(&path).unwrap();
        assert_eq!(db.schema_version().unwrap(), MIGRATIONS.len() as i64);
        let note = db.get_note("old").unwrap();
        assert_eq!(note.enhanced_notes.as_deref(), Some("## 決定事項"));
        let segments = db.list_segments("old").unwrap();
        assert!(db.list_breaks("old").unwrap().is_empty());
        let rounds = crate::transcript::split_rounds(&segments, &[]);
        assert_eq!(rounds.len(), 1, "区切りのない古いノートは 1 回分の録音");
        // 古いノートで録音を再開すると、2 回目の区切りが付く
        assert_eq!(db.add_break_if_resumed("old", 1_000).unwrap().unwrap().round, 2);
    }

    // [DAT-003]
    #[test]
    fn foreign_keys_are_enforced() {
        let db = Database::open_in_memory().unwrap();
        let err = db.conn().execute(
            "INSERT INTO transcript_segments (note_id, speaker, started_at, ended_at, text)
             VALUES ('missing', 'me', 0, 1, 'x')",
            [],
        );
        assert!(err.is_err(), "存在しないノートへのセグメントは拒否される");
    }
}
