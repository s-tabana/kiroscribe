//! 会議ノートの保存と取得。

use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::error::{CoreError, Result};
use crate::time::now_ms;

/// ノート本体。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub id: String,
    pub title: String,
    /// 会議中に自分で書いたメモ（Markdown 風のプレーンテキスト）。
    pub user_notes: String,
    /// AI が仕上げた議事録（Markdown）。未生成なら `None`。
    pub enhanced_notes: Option<String>,
    pub enhanced_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// サイドバー表示用の要約。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteSummary {
    pub id: String,
    pub title: String,
    pub preview: String,
    pub has_enhanced: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// ノートの部分更新。`None` のフィールドは変更しない。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotePatch {
    pub title: Option<String>,
    pub user_notes: Option<String>,
    pub enhanced_notes: Option<String>,
}

const PREVIEW_CHARS: usize = 60;

fn strip_markers(line: &str) -> &str {
    line.trim_start_matches(|c: char| matches!(c, '#' | '-' | '*' | '・' | '>') || c.is_whitespace())
        .trim_start_matches("[ ] ")
        .trim_start_matches("[x] ")
        .trim_end()
}

/// 一覧に出す 1 行。見出しより本文の最初の行を優先し、記号（見出し・箇条書き）は外す。
pub fn make_preview(text: &str) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let line = lines
        .iter()
        .find(|l| !l.trim_start().starts_with('#'))
        .or_else(|| lines.first())
        .map(|l| strip_markers(l))
        .unwrap_or("");
    let mut preview: String = line.chars().take(PREVIEW_CHARS).collect();
    if line.chars().count() > PREVIEW_CHARS {
        preview.push('…');
    }
    preview
}

fn row_to_note(row: &Row<'_>) -> rusqlite::Result<Note> {
    Ok(Note {
        id: row.get("id")?,
        title: row.get("title")?,
        user_notes: row.get("user_notes")?,
        enhanced_notes: row.get("enhanced_notes")?,
        enhanced_at: row.get("enhanced_at")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

impl Database {
    pub fn create_note(&self) -> Result<Note> {
        let now = now_ms();
        let note = Note {
            id: uuid::Uuid::new_v4().to_string(),
            title: String::new(),
            user_notes: String::new(),
            enhanced_notes: None,
            enhanced_at: None,
            created_at: now,
            updated_at: now,
        };
        self.conn().execute(
            "INSERT INTO notes (id, title, user_notes, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![note.id, note.title, note.user_notes, note.created_at, note.updated_at],
        )?;
        Ok(note)
    }

    pub fn get_note(&self, id: &str) -> Result<Note> {
        self.conn()
            .query_row("SELECT * FROM notes WHERE id = ?1", [id], row_to_note)
            .optional()?
            .ok_or_else(|| CoreError::NoteNotFound(id.to_string()))
    }

    /// 作成日時の新しい順（会議の新しい順）に並べる。編集中にノートの並びが動かないよう、更新日時では並べない。
    pub fn list_notes(&self) -> Result<Vec<NoteSummary>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, title, user_notes, enhanced_notes, created_at, updated_at
             FROM notes ORDER BY created_at DESC, rowid DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            let user_notes: String = row.get("user_notes")?;
            let enhanced: Option<String> = row.get("enhanced_notes")?;
            let source = match &enhanced {
                Some(text) if !text.trim().is_empty() => text.as_str(),
                _ => user_notes.as_str(),
            };
            Ok(NoteSummary {
                id: row.get("id")?,
                title: row.get("title")?,
                preview: make_preview(source),
                has_enhanced: enhanced.is_some(),
                created_at: row.get("created_at")?,
                updated_at: row.get("updated_at")?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn update_note(&self, id: &str, patch: &NotePatch) -> Result<Note> {
        let mut note = self.get_note(id)?;
        if let Some(title) = &patch.title {
            note.title = title.clone();
        }
        if let Some(user_notes) = &patch.user_notes {
            note.user_notes = user_notes.clone();
        }
        if let Some(enhanced) = &patch.enhanced_notes {
            note.enhanced_notes = Some(enhanced.clone());
        }
        note.updated_at = now_ms().max(note.updated_at);
        self.conn().execute(
            "UPDATE notes SET title = ?2, user_notes = ?3, enhanced_notes = ?4, updated_at = ?5
             WHERE id = ?1",
            params![
                note.id,
                note.title,
                note.user_notes,
                note.enhanced_notes,
                note.updated_at
            ],
        )?;
        Ok(note)
    }

    /// AI が生成した議事録を保存する。タイトルが空なら `suggested_title` を設定する。
    pub fn save_enhanced_notes(
        &self,
        id: &str,
        enhanced: &str,
        suggested_title: Option<&str>,
    ) -> Result<Note> {
        let mut note = self.get_note(id)?;
        let now = now_ms();
        note.enhanced_notes = Some(enhanced.to_string());
        note.enhanced_at = Some(now);
        note.updated_at = now.max(note.updated_at);
        if note.title.trim().is_empty() {
            if let Some(title) = suggested_title.map(str::trim).filter(|t| !t.is_empty()) {
                note.title = title.to_string();
            }
        }
        self.conn().execute(
            "UPDATE notes SET title = ?2, enhanced_notes = ?3, enhanced_at = ?4, updated_at = ?5
             WHERE id = ?1",
            params![
                note.id,
                note.title,
                note.enhanced_notes,
                note.enhanced_at,
                note.updated_at
            ],
        )?;
        Ok(note)
    }

    pub fn delete_note(&self, id: &str) -> Result<()> {
        let affected = self.conn().execute("DELETE FROM notes WHERE id = ?1", [id])?;
        if affected == 0 {
            return Err(CoreError::NoteNotFound(id.to_string()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn create_and_get_note() {
        let db = db();
        let note = db.create_note().unwrap();
        assert_eq!(note.title, "");
        assert_eq!(db.get_note(&note.id).unwrap(), note);
    }

    #[test]
    fn get_missing_note_is_not_found() {
        let err = db().get_note("nope").unwrap_err();
        assert!(matches!(err, CoreError::NoteNotFound(_)));
    }

    #[test]
    fn update_note_changes_only_given_fields() {
        let db = db();
        let note = db.create_note().unwrap();
        let updated = db
            .update_note(
                &note.id,
                &NotePatch {
                    title: Some("定例".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.title, "定例");
        assert_eq!(updated.user_notes, "");

        let updated = db
            .update_note(
                &note.id,
                &NotePatch {
                    user_notes: Some("- 予算の件".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.title, "定例", "タイトルは保持される");
        assert_eq!(updated.user_notes, "- 予算の件");
        assert!(updated.updated_at >= note.updated_at);
        assert_eq!(db.get_note(&note.id).unwrap(), updated);
    }

    #[test]
    fn list_notes_newest_first_with_preview() {
        let db = db();
        let first = db.create_note().unwrap();
        let second = db.create_note().unwrap();
        db.update_note(
            &first.id,
            &NotePatch {
                user_notes: Some("\n\n## 予算\n- 来期は増額".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let list = db.list_notes().unwrap();
        assert_eq!(list.len(), 2);
        // 作成が新しい順。編集しても並びは変わらない
        assert_eq!(list[0].id, second.id);
        assert_eq!(list[1].id, first.id);
        assert_eq!(list[1].preview, "来期は増額", "見出しより本文を優先");
        assert!(!list[1].has_enhanced);
    }

    #[test]
    fn save_enhanced_sets_title_only_when_empty() {
        let db = db();
        let note = db.create_note().unwrap();
        let saved = db
            .save_enhanced_notes(&note.id, "## 決定事項\n- A", Some("予算会議"))
            .unwrap();
        assert_eq!(saved.title, "予算会議");
        assert_eq!(saved.enhanced_notes.as_deref(), Some("## 決定事項\n- A"));
        assert!(saved.enhanced_at.is_some());

        let saved = db
            .save_enhanced_notes(&note.id, "## 決定事項\n- B", Some("別のタイトル"))
            .unwrap();
        assert_eq!(saved.title, "予算会議", "既存のタイトルは上書きしない");

        let list = db.list_notes().unwrap();
        assert!(list[0].has_enhanced);
        assert_eq!(list[0].preview, "B", "最新の議事録の本文がプレビューになる");
    }

    #[test]
    fn delete_note_removes_it() {
        let db = db();
        let note = db.create_note().unwrap();
        db.delete_note(&note.id).unwrap();
        assert!(db.list_notes().unwrap().is_empty());
        assert!(matches!(
            db.delete_note(&note.id).unwrap_err(),
            CoreError::NoteNotFound(_)
        ));
    }

    #[test]
    fn preview_truncates_long_lines() {
        let long = "あ".repeat(100);
        let preview = make_preview(&long);
        assert_eq!(preview.chars().count(), PREVIEW_CHARS + 1);
        assert!(preview.ends_with('…'));
        assert_eq!(make_preview("   \n  "), "");
        assert_eq!(make_preview("- * 箇条書き"), "箇条書き");
        assert_eq!(make_preview("## 見出しだけ"), "見出しだけ");
        assert_eq!(make_preview("## ToDo\n- [ ] 見積もり"), "見積もり");
    }
}
