//! 文字起こしセグメントの保存。

use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::error::Result;

/// 話者。マイク＝自分、システム音声＝相手。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Speaker {
    Me,
    Them,
}

impl Speaker {
    pub fn as_str(self) -> &'static str {
        match self {
            Speaker::Me => "me",
            Speaker::Them => "them",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Speaker::Me => "自分",
            Speaker::Them => "相手",
        }
    }

    fn parse(value: &str) -> Speaker {
        if value == "them" {
            Speaker::Them
        } else {
            Speaker::Me
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub id: i64,
    pub note_id: String,
    pub speaker: Speaker,
    /// 発話の開始・終了（UNIX ミリ秒）
    pub started_at: i64,
    pub ended_at: i64,
    pub text: String,
}

fn row_to_segment(row: &Row<'_>) -> rusqlite::Result<Segment> {
    let speaker: String = row.get("speaker")?;
    Ok(Segment {
        id: row.get("id")?,
        note_id: row.get("note_id")?,
        speaker: Speaker::parse(&speaker),
        started_at: row.get("started_at")?,
        ended_at: row.get("ended_at")?,
        text: row.get("text")?,
    })
}

impl Database {
    pub fn add_segment(
        &self,
        note_id: &str,
        speaker: Speaker,
        started_at: i64,
        ended_at: i64,
        text: &str,
    ) -> Result<Segment> {
        self.conn().execute(
            "INSERT INTO transcript_segments (note_id, speaker, started_at, ended_at, text)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![note_id, speaker.as_str(), started_at, ended_at, text],
        )?;
        let id = self.conn().last_insert_rowid();
        // 文字起こしが増えたらノートの更新日時も進める
        self.conn().execute(
            "UPDATE notes SET updated_at = max(updated_at, ?2) WHERE id = ?1",
            params![note_id, ended_at],
        )?;
        Ok(Segment {
            id,
            note_id: note_id.to_string(),
            speaker,
            started_at,
            ended_at,
            text: text.to_string(),
        })
    }

    /// 発話の開始時刻順。自分と相手の発話が時系列に混ざって並ぶ。
    pub fn list_segments(&self, note_id: &str) -> Result<Vec<Segment>> {
        let mut stmt = self.conn().prepare(
            "SELECT * FROM transcript_segments WHERE note_id = ?1 ORDER BY started_at, id",
        )?;
        let rows = stmt.query_map([note_id], row_to_segment)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_are_ordered_by_start_time_across_speakers() {
        let db = Database::open_in_memory().unwrap();
        let note = db.create_note().unwrap();
        // 相手の発話の文字起こしが後から届いても、開始時刻順に並ぶ
        db.add_segment(&note.id, Speaker::Me, 2_000, 3_000, "二番目").unwrap();
        db.add_segment(&note.id, Speaker::Them, 1_000, 1_800, "一番目").unwrap();
        db.add_segment(&note.id, Speaker::Them, 4_000, 5_000, "三番目").unwrap();

        let segments = db.list_segments(&note.id).unwrap();
        let texts: Vec<_> = segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["一番目", "二番目", "三番目"]);
        assert_eq!(segments[0].speaker, Speaker::Them);
        assert_eq!(segments[1].speaker, Speaker::Me);
    }

    #[test]
    fn segments_are_deleted_with_note() {
        let db = Database::open_in_memory().unwrap();
        let note = db.create_note().unwrap();
        db.add_segment(&note.id, Speaker::Me, 0, 1, "x").unwrap();
        db.delete_note(&note.id).unwrap();
        assert!(db.list_segments(&note.id).unwrap().is_empty());
    }

    #[test]
    fn speaker_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Speaker::Them).unwrap(), "\"them\"");
    }
}
