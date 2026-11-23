//! 会議の自動検出 → ノート作成と自動録音 → 会議が終わったら自動停止。

use std::time::Duration;

use chrono::Local;
use kiroscribe_core::meetings::{observe, MeetingApp, MeetingEvent, MeetingTracker};
use kiroscribe_core::notes::{Note, NotePatch};
use kiroscribe_core::secrets::{require_api_key, Provider};
use kiroscribe_core::time::now_ms;
use tauri::{Emitter, Manager};

use crate::background::{show_popup, PopupPayload};
use crate::recording::{start_for_note, stop_now, RecorderState};
use crate::runtime::AppHandle;
use crate::state::AppState;

pub const EVENT_NOTE_CREATED: &str = "note-created";
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// 検出を続けるスレッドを起動する。
pub fn spawn(app: AppHandle) {
    let own_exe = std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().to_string());
    let spawned = std::thread::Builder::new()
        .name("kiroscribe-meetings".into())
        .spawn(move || {
            #[allow(unused_mut)]
            let mut tracker = MeetingTracker::default();
            // E2E テスト用のビルドでだけ、待ち時間を縮め、実際の検出の代わりにテストが決めた状態を使う（src/e2e.rs）
            #[cfg(feature = "e2e")]
            crate::e2e::tune_tracker(&mut tracker);
            loop {
                std::thread::sleep(poll_interval());
                #[cfg(feature = "e2e")]
                let simulated = crate::e2e::simulated_detection();
                #[cfg(not(feature = "e2e"))]
                let simulated: Option<(Option<kiroscribe_core::meetings::Detection>, bool)> = None;
                let event = match simulated {
                    Some((detected, continuing)) => tracker.tick(detected, continuing, now_ms()),
                    None => tracker.observe(&observe(), own_exe.as_deref(), now_ms()),
                };
                if let Some(event) = event {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move { handle(&app, event).await });
                }
            }
        });
    if let Err(err) = spawned {
        eprintln!("会議の検出を開始できませんでした: {err}");
    }
}

/// 検出を調べる間隔。
pub(crate) fn poll_interval() -> Duration {
    #[cfg(feature = "e2e")]
    if let Some(interval) = crate::e2e::meeting_poll_interval() {
        return interval;
    }
    POLL_INTERVAL
}

/// 例: 「Google Meet 11/20 15:04」
pub fn note_title(meeting: MeetingApp) -> String {
    format!("{} {}", meeting.label(), Local::now().format("%-m/%-d %H:%M"))
}

fn create_meeting_note(app: &AppHandle, meeting: MeetingApp) -> Result<Note, String> {
    let state = app.state::<AppState>();
    let db = state.db();
    let note = db.create_note().map_err(|e| e.to_string())?;
    db.update_note(
        &note.id,
        &NotePatch {
            title: Some(note_title(meeting)),
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())
}

/// 確定した会議の開始・終了を処理する。
pub(crate) async fn handle(app: &AppHandle, event: MeetingEvent) {
    match event {
        MeetingEvent::Started(meeting) => on_started(app, meeting).await,
        MeetingEvent::Ended(meeting) => on_ended(app, meeting).await,
    }
}

async fn on_started(app: &AppHandle, meeting: MeetingApp) {
    let state = app.state::<AppState>();
    let enabled = state
        .db()
        .load_settings()
        .map(|s| s.auto_record_meetings)
        .unwrap_or(false);
    if !enabled {
        return;
    }
    // 手動で録音中なら何もしない
    if app.state::<RecorderState>().busy_note_id().await.is_some() {
        return;
    }
    if require_api_key(state.secrets.as_ref(), Provider::OpenAi).is_err() {
        show_popup(
            app,
            PopupPayload::Error {
                app: meeting.label().into(),
                message: "OpenAI の API キーが設定されていないため、自動で録音できませんでした".into(),
            },
        );
        return;
    }

    let note = match create_meeting_note(app, meeting) {
        Ok(note) => note,
        Err(message) => {
            show_popup(
                app,
                PopupPayload::Error {
                    app: meeting.label().into(),
                    message,
                },
            );
            return;
        }
    };
    let _ = app.emit(EVENT_NOTE_CREATED, &note);

    match start_for_note(app, note.id.clone(), Some(meeting)).await {
        Ok(_) => show_popup(
            app,
            PopupPayload::Recording {
                app: meeting.label().into(),
                note_id: note.id,
                title: note.title,
            },
        ),
        Err(err) => show_popup(
            app,
            PopupPayload::Error {
                app: meeting.label().into(),
                message: err.0,
            },
        ),
    }
}

async fn on_ended(app: &AppHandle, meeting: MeetingApp) {
    // 自動で始めた録音が続いているときだけ止める（手動で止めた・手動の録音は触らない）
    let Some((note_id, auto)) = app.state::<RecorderState>().auto_recording().await else {
        return;
    };
    if auto != meeting {
        return;
    }
    let title = app
        .state::<AppState>()
        .db()
        .get_note(&note_id)
        .map(|n| n.title)
        .unwrap_or_default();
    show_popup(
        app,
        PopupPayload::Finishing {
            app: meeting.label().into(),
            note_id: note_id.clone(),
            title: title.clone(),
        },
    );
    stop_now(app).await;
    show_popup(
        app,
        PopupPayload::Stopped {
            app: meeting.label().into(),
            note_id,
            title,
        },
    );
}
