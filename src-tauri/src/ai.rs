//! Claude による議事録生成と AI チャット。生成中の文字はイベントで画面へ逐次送る。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use kiroscribe_core::chat::ChatMessage;
use kiroscribe_core::claude::{ClaudeClient, Effort, MessageRequest, Role, Turn};
use kiroscribe_core::notes::Note;
use kiroscribe_core::prompts::{
    chat_system_prompt, enhance_user_message, split_title, MeetingContext, ENHANCE_SYSTEM_PROMPT,
};
use kiroscribe_core::retry::RetryPolicy;
use kiroscribe_core::secrets::{require_api_key, Provider};
use kiroscribe_core::settings::Settings;
use kiroscribe_core::transcript::{Segment, TranscriptBreak};
use serde::Serialize;
use serde_json::json;
use tauri::{Emitter, State};

use crate::error::{CmdResult, CommandError};
use crate::runtime::AppHandle;
use crate::state::AppState;

pub const EVENT_ENHANCE_DELTA: &str = "enhance-delta";
pub const EVENT_CHAT_DELTA: &str = "chat-delta";

/// チャットで送る過去の会話の上限（往復数ではなくメッセージ数）
const MAX_HISTORY: usize = 20;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DeltaEvent {
    note_id: String,
    delta: String,
}

/// 実行中の生成（ノート × 種類ごとに 1 つ）と、その中止フラグ。
#[derive(Default)]
pub struct AiState {
    running: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

struct RunGuard<'a> {
    state: &'a AiState,
    key: String,
    cancel: Arc<AtomicBool>,
}

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.state.running.lock().unwrap().remove(&self.key);
    }
}

impl AiState {
    fn begin(&self, key: String, busy_message: &str) -> CmdResult<RunGuard<'_>> {
        let mut running = self.running.lock().unwrap();
        if running.contains_key(&key) {
            return Err(CommandError(busy_message.to_string()));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        running.insert(key.clone(), cancel.clone());
        Ok(RunGuard {
            state: self,
            key,
            cancel,
        })
    }

    fn cancel(&self, key: &str) {
        if let Some(flag) = self.running.lock().unwrap().get(key) {
            flag.store(true, Ordering::SeqCst);
        }
    }

    /// 議事録やチャットの回答を生成中か（アプリの更新など、途中で終了させたくない処理の前に確かめる）。
    pub fn is_busy(&self) -> bool {
        !self.running.lock().unwrap().is_empty()
    }
}

struct Loaded {
    note: Note,
    segments: Vec<Segment>,
    breaks: Vec<TranscriptBreak>,
    settings: Settings,
    api_key: String,
}

fn load(state: &AppState, note_id: &str) -> CmdResult<Loaded> {
    let (note, segments, breaks, settings) = {
        let db = state.db();
        (
            db.get_note(note_id)?,
            db.list_segments(note_id)?,
            db.list_breaks(note_id)?,
            db.load_settings()?,
        )
    };
    let api_key = require_api_key(state.secrets.as_ref(), Provider::Anthropic)?;
    Ok(Loaded {
        note,
        segments,
        breaks,
        settings,
        api_key,
    })
}

fn context(loaded: &Loaded) -> MeetingContext<'_> {
    MeetingContext {
        title: &loaded.note.title,
        created_at: loaded.note.created_at,
        user_notes: &loaded.note.user_notes,
        enhanced_notes: loaded.note.enhanced_notes.as_deref(),
        segments: &loaded.segments,
        breaks: &loaded.breaks,
        my_name: &loaded.settings.my_name,
    }
}

fn emitter(app: &AppHandle, event: &'static str, note_id: &str) -> impl FnMut(&str) + Send {
    let app = app.clone();
    let note_id = note_id.to_string();
    move |delta: &str| {
        let _ = app.emit(
            event,
            DeltaEvent {
                note_id: note_id.clone(),
                delta: delta.to_string(),
            },
        );
    }
}

/// AI の処理の始まりを書き、終わったら成否と時間を書く（送った内容と、返ってきた内容は書かない）。
fn log_started(app: &AppHandle, kind: &str, note_id: &str, model: &str) -> std::time::Instant {
    crate::debug::info(app, "ai.started", json!({ "kind": kind, "noteId": note_id, "model": model }));
    std::time::Instant::now()
}

fn log_finished<T>(
    app: &AppHandle,
    kind: &str,
    note_id: &str,
    started: std::time::Instant,
    cancelled: bool,
    result: &CmdResult<T>,
) {
    let ms = started.elapsed().as_millis() as i64;
    match result {
        Ok(_) => crate::debug::info(app, "ai.finished", json!({ "kind": kind, "noteId": note_id, "ms": ms })),
        Err(_) if cancelled => {
            crate::debug::info(app, "ai.cancelled", json!({ "kind": kind, "noteId": note_id, "ms": ms }))
        }
        Err(err) => crate::debug::error(
            app,
            "ai.failed",
            json!({ "kind": kind, "noteId": note_id, "ms": ms, "reason": err.0 }),
        ),
    }
}

/// メモと文字起こしから議事録を作り、保存したノートを返す。
#[tauri::command]
pub async fn enhance_note(
    app: AppHandle,
    state: State<'_, AppState>,
    ai: State<'_, AiState>,
    note_id: String,
) -> CmdResult<Note> {
    let guard = ai.begin(format!("enhance:{note_id}"), "このノートの議事録を作成中です")?;
    let loaded = load(&state, &note_id)?;
    let started = log_started(&app, "enhance", &note_id, &loaded.settings.claude_model);
    let result = enhance(&app, &state, &guard, &loaded, &note_id).await;
    log_finished(&app, "enhance", &note_id, started, guard.cancel.load(Ordering::SeqCst), &result);
    result
}

async fn enhance(
    app: &AppHandle,
    state: &AppState,
    guard: &RunGuard<'_>,
    loaded: &Loaded,
    note_id: &str,
) -> CmdResult<Note> {
    let request = MessageRequest {
        system: ENHANCE_SYSTEM_PROMPT.to_string(),
        messages: vec![Turn {
            role: Role::User,
            content: enhance_user_message(&context(loaded))?,
        }],
        max_tokens: 64_000,
        effort: Effort::Medium,
    };
    let client = ClaudeClient::new(&loaded.api_key, &loaded.settings.claude_model);
    let mut on_text = emitter(app, EVENT_ENHANCE_DELTA, note_id);
    let result = client
        .stream(&request, &mut on_text, &guard.cancel, &RetryPolicy::default())
        .await?;

    let (title, body) = split_title(&result.text);
    if body.is_empty() {
        return Err(CommandError(
            "議事録を作成できませんでした（応答が空でした）。もう一度お試しください".into(),
        ));
    }
    let note = state
        .db()
        .save_enhanced_notes(note_id, &body, title.as_deref())?;
    Ok(note)
}

/// 生成を中止する。`kind` は "enhance" または "chat"。
#[tauri::command]
pub fn cancel_ai(ai: State<'_, AiState>, note_id: String, kind: String) {
    ai.cancel(&format!("{kind}:{note_id}"));
}

#[tauri::command]
pub fn list_chat_messages(
    state: State<'_, AppState>,
    note_id: String,
) -> CmdResult<Vec<ChatMessage>> {
    Ok(state.db().list_chat_messages(&note_id)?)
}

#[tauri::command]
pub fn clear_chat_messages(state: State<'_, AppState>, note_id: String) -> CmdResult<()> {
    Ok(state.db().clear_chat_messages(&note_id)?)
}

/// AI に送る会話: 履歴のうち新しいほうから `MAX_HISTORY` 件までを、交互の発言にまとめたもの。
fn recent_turns(history: &[ChatMessage]) -> Vec<Turn> {
    to_turns(&history[history.len().saturating_sub(MAX_HISTORY)..])
}

/// 同じ話者が続くメッセージをまとめる（API は user と assistant が交互である前提のため）。
fn to_turns(history: &[ChatMessage]) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    for message in history {
        match turns.last_mut() {
            Some(last) if last.role == message.role => {
                last.content.push_str("\n\n");
                last.content.push_str(&message.content);
            }
            _ => turns.push(Turn {
                role: message.role,
                content: message.content.clone(),
            }),
        }
    }
    // 先頭は利用者の発言から始める
    while turns.first().is_some_and(|t| t.role != Role::User) {
        turns.remove(0);
    }
    turns
}

/// 質問を保存して Claude に送り、回答を保存して返す。
#[tauri::command]
pub async fn send_chat_message(
    app: AppHandle,
    state: State<'_, AppState>,
    ai: State<'_, AiState>,
    note_id: String,
    content: String,
) -> CmdResult<ChatMessage> {
    let content = content.trim().to_string();
    if content.is_empty() {
        return Err(CommandError("質問を入力してください".into()));
    }
    let guard = ai.begin(format!("chat:{note_id}"), "回答を作成中です")?;
    let loaded = load(&state, &note_id)?;
    // 質問は、AI に送る前に履歴へ保存する
    let (question, history) = {
        let db = state.db();
        let question = db.add_chat_message(&note_id, Role::User, &content)?;
        (question, db.list_chat_messages(&note_id)?)
    };
    let started = log_started(&app, "chat", &note_id, &loaded.settings.claude_model);
    let answered = answer_question(&app, &state, &guard, &loaded, &note_id, &history).await;
    log_finished(&app, "chat", &note_id, started, guard.cancel.load(Ordering::SeqCst), &answered);
    if answered.is_err() {
        // 回答をもらえなかった質問（失敗・中止・空の回答）は、履歴に残さない。
        // 画面は質問を入力欄に戻すので、残すと、送り直したときに同じ質問が 2 つ並ぶ
        let _ = state.db().delete_chat_message(question.id);
    }
    answered
}

/// 履歴の会話を AI に送り、回答を履歴に保存して返す。
async fn answer_question(
    app: &AppHandle,
    state: &AppState,
    guard: &RunGuard<'_>,
    loaded: &Loaded,
    note_id: &str,
    history: &[ChatMessage],
) -> CmdResult<ChatMessage> {
    let request = MessageRequest {
        system: chat_system_prompt(&context(loaded)),
        messages: recent_turns(history),
        max_tokens: 16_000,
        effort: Effort::Low,
    };
    let client = ClaudeClient::new(&loaded.api_key, &loaded.settings.claude_model);
    let mut on_text = emitter(app, EVENT_CHAT_DELTA, note_id);
    let result = client
        .stream(&request, &mut on_text, &guard.cancel, &RetryPolicy::default())
        .await?;
    let answer = result.text.trim();
    if answer.is_empty() {
        return Err(CommandError("回答が空でした。もう一度お試しください".into()));
    }
    Ok(state.db().add_chat_message(note_id, Role::Assistant, answer)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: i64, role: Role, content: &str) -> ChatMessage {
        ChatMessage {
            id,
            note_id: "n".into(),
            role,
            content: content.into(),
            created_at: 0,
        }
    }

    // [CHT-007]
    #[test]
    fn turns_alternate_and_start_with_user() {
        let history = [
            msg(1, Role::Assistant, "（前回の途中の回答）"),
            msg(2, Role::User, "決まったことは？"),
            msg(3, Role::User, "あと ToDo も"),
            msg(4, Role::Assistant, "リリースは 12/5 です"),
            msg(5, Role::User, "ありがとう"),
        ];
        let turns = to_turns(&history);
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].role, Role::User);
        assert_eq!(turns[0].content, "決まったことは？\n\nあと ToDo も");
        assert_eq!(turns[1].role, Role::Assistant);
        assert_eq!(turns[2].content, "ありがとう");
    }

    // [CHT-007]
    #[test]
    fn only_the_latest_twenty_messages_are_sent() {
        // 質問と回答が交互に 15 往復（30 件）
        let history: Vec<ChatMessage> = (0..30)
            .map(|i| {
                let role = if i % 2 == 0 { Role::User } else { Role::Assistant };
                msg(i, role, &format!("発言 {i}"))
            })
            .collect();
        let turns = recent_turns(&history);
        assert_eq!(turns.len(), 20, "新しいほうから 20 件まで");
        assert_eq!(turns[0].content, "発言 10");
        assert_eq!(turns[0].role, Role::User);
        assert_eq!(turns[19].content, "発言 29");

        // 20 件に切ったところが AI の回答から始まるときは、利用者の発言から始まるように詰める
        let turns = recent_turns(&history[..29]);
        assert_eq!(turns.len(), 19);
        assert_eq!((turns[0].role, turns[0].content.as_str()), (Role::User, "発言 10"));
        assert_eq!(turns[18].content, "発言 28");

        // 20 件に満たなければ全部
        assert_eq!(recent_turns(&history[..4]).len(), 4);
        assert!(recent_turns(&[]).is_empty());
    }

    // [MIN-008]
    #[test]
    fn only_one_run_per_key() {
        let state = AiState::default();
        assert!(!state.is_busy());
        let guard = state.begin("enhance:a".into(), "busy").unwrap();
        assert!(state.is_busy());
        assert!(state.begin("enhance:a".into(), "busy").is_err());
        assert_eq!(state.begin("enhance:a".into(), "このノートの議事録を作成中です").err().unwrap().0, "このノートの議事録を作成中です");
        // 同じノートの AI チャットや、別のノートの議事録とは同時に進められる
        assert!(state.begin("chat:a".into(), "busy").is_ok());
        let other = state.begin("enhance:b".into(), "busy").expect("別のノートは始められる");
        drop(other);
        assert!(state.is_busy(), "ほかの作成が終わっても、このノートの作成は続いている");
        state.cancel("enhance:a");
        assert!(guard.cancel.load(Ordering::SeqCst));
        drop(guard);
        assert!(!state.is_busy());
        assert!(state.begin("enhance:a".into(), "busy").is_ok(), "終わったら次を始められる");
    }
}
