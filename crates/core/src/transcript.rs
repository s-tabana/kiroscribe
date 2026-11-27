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

/// 文字起こしの区切り。文字起こしのあるノートで録音を始め直した位置（回の番号と開始時刻）を表す。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptBreak {
    pub id: i64,
    pub note_id: String,
    /// 何回目の録音か（最初の区切りが 2。1 回目には区切りを付けない）
    pub round: i64,
    /// 録音を始めた時刻（UNIX ミリ秒）。この時刻以降に始まった発話が、この回に入る
    pub started_at: i64,
}

fn row_to_break(row: &Row<'_>) -> rusqlite::Result<TranscriptBreak> {
    Ok(TranscriptBreak {
        id: row.get("id")?,
        note_id: row.get("note_id")?,
        round: row.get("round")?,
        started_at: row.get("started_at")?,
    })
}

/// 回ごとに分けた文字起こし（[`split_rounds`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Round<'a> {
    /// 何回目の録音か（1 から）
    pub number: i64,
    /// 録音を始めた時刻。1 回目は区切りがないので、最初の発話の時刻
    pub started_at: i64,
    pub segments: &'a [Segment],
}

/// 「2回目の録音（14:30〜）」の形の見出し。`time` は時刻の表記（呼び出し側で作る）。
pub fn round_label(number: i64, time: &str) -> String {
    format!("{number}回目の録音（{time}〜）")
}

/// 開始時刻順の文字起こしを、区切りで回ごとに分ける。区切りがなければ 1 回分（空なら空の一覧）。
pub fn split_rounds<'a>(segments: &'a [Segment], breaks: &[TranscriptBreak]) -> Vec<Round<'a>> {
    if breaks.is_empty() {
        return match segments.first() {
            Some(first) => vec![Round {
                number: 1,
                started_at: first.started_at,
                segments,
            }],
            None => Vec::new(),
        };
    }
    let mut breaks: Vec<&TranscriptBreak> = breaks.iter().collect();
    breaks.sort_by_key(|b| (b.started_at, b.id));
    let mut rounds = Vec::with_capacity(breaks.len() + 1);
    let first_end = segments.partition_point(|s| s.started_at < breaks[0].started_at);
    if first_end > 0 {
        rounds.push(Round {
            number: 1,
            started_at: segments[0].started_at,
            segments: &segments[..first_end],
        });
    }
    let mut start = first_end;
    for (index, brk) in breaks.iter().enumerate() {
        let end = match breaks.get(index + 1) {
            Some(next) => start + segments[start..].partition_point(|s| s.started_at < next.started_at),
            None => segments.len(),
        };
        rounds.push(Round {
            number: brk.round,
            started_at: brk.started_at,
            segments: &segments[start..end],
        });
        start = end;
    }
    rounds
}

impl Database {
    /// 文字起こしが 1 件以上あるノートで録音を始めるときに、`started_at` の位置に区切りを足す。
    /// 回の番号は、それまでの区切りの数 + 2。文字起こしがなければ何もしない。
    pub fn add_break_if_resumed(&self, note_id: &str, started_at: i64) -> Result<Option<TranscriptBreak>> {
        let segments: i64 = self.conn().query_row(
            "SELECT count(*) FROM transcript_segments WHERE note_id = ?1",
            [note_id],
            |row| row.get(0),
        )?;
        if segments == 0 {
            return Ok(None);
        }
        let breaks: i64 = self.conn().query_row(
            "SELECT count(*) FROM transcript_breaks WHERE note_id = ?1",
            [note_id],
            |row| row.get(0),
        )?;
        let round = breaks + 2;
        self.conn().execute(
            "INSERT INTO transcript_breaks (note_id, round, started_at) VALUES (?1, ?2, ?3)",
            params![note_id, round, started_at],
        )?;
        Ok(Some(TranscriptBreak {
            id: self.conn().last_insert_rowid(),
            note_id: note_id.to_string(),
            round,
            started_at,
        }))
    }

    /// ノートの区切りを、開始時刻順に返す。区切りのないノート（以前の版のノートを含む）は空。
    pub fn list_breaks(&self, note_id: &str) -> Result<Vec<TranscriptBreak>> {
        let mut stmt = self.conn().prepare(
            "SELECT * FROM transcript_breaks WHERE note_id = ?1 ORDER BY started_at, id",
        )?;
        let rows = stmt.query_map([note_id], row_to_break)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// 区切りのあとに発話が 1 件も保存されていなければ、その区切りを消す（消したら true）。
    /// 録音を止めたときに呼び、何も話さなかった回を残さない。
    pub fn remove_break_if_empty(&self, break_id: i64) -> Result<bool> {
        let removed = self.conn().execute(
            "DELETE FROM transcript_breaks WHERE id = ?1 AND NOT EXISTS (
                 SELECT 1 FROM transcript_segments s
                 WHERE s.note_id = transcript_breaks.note_id AND s.started_at >= transcript_breaks.started_at
             )",
            [break_id],
        )?;
        Ok(removed > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // [TRN-011]
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

    // [NOTE-015]
    #[test]
    fn segments_are_deleted_with_note() {
        let db = Database::open_in_memory().unwrap();
        let note = db.create_note().unwrap();
        db.add_segment(&note.id, Speaker::Me, 0, 1, "x").unwrap();
        db.delete_note(&note.id).unwrap();
        assert!(db.list_segments(&note.id).unwrap().is_empty());
    }

    fn seg(id: i64, started_at: i64) -> Segment {
        Segment {
            id,
            note_id: "n".into(),
            speaker: Speaker::Me,
            started_at,
            ended_at: started_at + 500,
            text: format!("発話{id}"),
        }
    }

    fn brk(id: i64, round: i64, started_at: i64) -> TranscriptBreak {
        TranscriptBreak {
            id,
            note_id: "n".into(),
            round,
            started_at,
        }
    }

    // [TRN-023]
    #[test]
    fn a_break_is_added_only_when_the_note_already_has_a_transcript() {
        let db = Database::open_in_memory().unwrap();
        let note = db.create_note().unwrap();
        // 文字起こしのないノートで始めた録音（1 回目）には、区切りを付けない
        assert_eq!(db.add_break_if_resumed(&note.id, 1_000).unwrap(), None);
        db.add_segment(&note.id, Speaker::Me, 1_500, 2_000, "一回目").unwrap();

        let second = db.add_break_if_resumed(&note.id, 10_000).unwrap().unwrap();
        assert_eq!((second.round, second.started_at), (2, 10_000));
        db.add_segment(&note.id, Speaker::Them, 10_500, 11_000, "二回目").unwrap();
        let third = db.add_break_if_resumed(&note.id, 20_000).unwrap().unwrap();
        assert_eq!(third.round, 3);
        assert_eq!(db.list_breaks(&note.id).unwrap(), vec![second, third]);
    }

    // [TRN-023]
    #[test]
    fn a_break_without_any_utterance_after_it_is_removed() {
        let db = Database::open_in_memory().unwrap();
        let note = db.create_note().unwrap();
        db.add_segment(&note.id, Speaker::Me, 1_000, 2_000, "一回目").unwrap();
        let kept = db.add_break_if_resumed(&note.id, 10_000).unwrap().unwrap();
        db.add_segment(&note.id, Speaker::Me, 10_200, 11_000, "二回目").unwrap();
        assert!(!db.remove_break_if_empty(kept.id).unwrap(), "発話のある回は残す");

        let empty = db.add_break_if_resumed(&note.id, 20_000).unwrap().unwrap();
        assert!(db.remove_break_if_empty(empty.id).unwrap(), "何も話さなかった回は消す");
        assert_eq!(db.list_breaks(&note.id).unwrap(), vec![kept]);
        // 消したあとに再開したときも、番号は続きになる
        assert_eq!(db.add_break_if_resumed(&note.id, 30_000).unwrap().unwrap().round, 3);
    }

    // [TRN-023][NOTE-015]
    #[test]
    fn breaks_are_deleted_with_note() {
        let db = Database::open_in_memory().unwrap();
        let note = db.create_note().unwrap();
        db.add_segment(&note.id, Speaker::Me, 0, 1, "x").unwrap();
        db.add_break_if_resumed(&note.id, 5).unwrap();
        db.delete_note(&note.id).unwrap();
        assert!(db.list_breaks(&note.id).unwrap().is_empty());
    }

    // [TRN-023][MIN-021]
    #[test]
    fn segments_are_split_into_rounds_at_breaks() {
        let segments = [seg(1, 1_000), seg(2, 2_000), seg(3, 10_000), seg(4, 20_500), seg(5, 21_000)];
        assert!(split_rounds(&[], &[]).is_empty());
        let single = split_rounds(&segments, &[]);
        assert_eq!(single.len(), 1, "区切りがなければ 1 回分");
        assert_eq!((single[0].number, single[0].started_at, single[0].segments.len()), (1, 1_000, 5));

        let rounds = split_rounds(&segments, &[brk(2, 3, 20_000), brk(1, 2, 9_000)]);
        let shape: Vec<_> = rounds
            .iter()
            .map(|r| (r.number, r.started_at, r.segments.iter().map(|s| s.id).collect::<Vec<_>>()))
            .collect();
        assert_eq!(
            shape,
            [(1, 1_000, vec![1, 2]), (2, 9_000, vec![3]), (3, 20_000, vec![4, 5])]
        );
        assert_eq!(round_label(2, "14:30"), "2回目の録音（14:30〜）");
    }

    // [IPC-004]
    #[test]
    fn speaker_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Speaker::Them).unwrap(), "\"them\"");
    }
}
