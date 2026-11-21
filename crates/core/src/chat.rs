//! ノートごとの AI チャット履歴。

use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::claude::Role;
use crate::db::Database;
use crate::error::Result;
use crate::time::now_ms;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: i64,
    pub note_id: String,
    pub role: Role,
    pub content: String,
    pub created_at: i64,
}

fn role_str(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

impl Database {
    pub fn add_chat_message(&self, note_id: &str, role: Role, content: &str) -> Result<ChatMessage> {
        let created_at = now_ms();
        self.conn().execute(
            "INSERT INTO chat_messages (note_id, role, content, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![note_id, role_str(role), content, created_at],
        )?;
        Ok(ChatMessage {
            id: self.conn().last_insert_rowid(),
            note_id: note_id.to_string(),
            role,
            content: content.to_string(),
            created_at,
        })
    }

    pub fn list_chat_messages(&self, note_id: &str) -> Result<Vec<ChatMessage>> {
        let mut stmt = self.conn().prepare(
            "SELECT id, note_id, role, content, created_at FROM chat_messages
             WHERE note_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map([note_id], |row| {
            let role: String = row.get(2)?;
            Ok(ChatMessage {
                id: row.get(0)?,
                note_id: row.get(1)?,
                role: if role == "assistant" { Role::Assistant } else { Role::User },
                content: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn clear_chat_messages(&self, note_id: &str) -> Result<()> {
        self.conn()
            .execute("DELETE FROM chat_messages WHERE note_id = ?1", [note_id])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_history_roundtrip_and_clear() {
        let db = Database::open_in_memory().unwrap();
        let note = db.create_note().unwrap();
        let other = db.create_note().unwrap();
        db.add_chat_message(&note.id, Role::User, "決まったことは？").unwrap();
        db.add_chat_message(&note.id, Role::Assistant, "リリースは 12/5 です").unwrap();
        db.add_chat_message(&other.id, Role::User, "別ノート").unwrap();

        let history = db.list_chat_messages(&note.id).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, Role::User);
        assert_eq!(history[1].content, "リリースは 12/5 です");

        db.clear_chat_messages(&note.id).unwrap();
        assert!(db.list_chat_messages(&note.id).unwrap().is_empty());
        assert_eq!(db.list_chat_messages(&other.id).unwrap().len(), 1);
    }
}
