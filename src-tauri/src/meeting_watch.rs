//! 会議の自動検出 → ノート作成と自動録音 → 会議が終わったら自動停止。

use std::time::Duration;

use chrono::Local;
use kiroscribe_core::meetings::{observe, Detection, MeetingApp, MeetingEvent, MeetingTracker};
use kiroscribe_core::notes::{Note, NotePatch};
use kiroscribe_core::secrets::{require_api_key, Provider};
use kiroscribe_core::time::now_ms;
use tauri::{AppHandle, Emitter, Manager};

use crate::background::{show_popup, PopupPayload};
use crate::recording::{start_for_note, stop_now, RecorderState};
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
            let mut tracker = MeetingTracker::default();
            let started = std::time::Instant::now();
            loop {
                std::thread::sleep(POLL_INTERVAL);
                let event = match simulated_meeting(started) {
                    Some(app) => {
                        let detection = Detection { app, via_browser: false };
                        tracker.tick(Some(detection), true, now_ms())
                    }
                    None if simulating() => tracker.tick(None, false, now_ms()),
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

/// 開発用: 環境変数 `KIROSCRIBE_SIMULATE_MEETING=10-40` で、起動 10 秒後から 40 秒後まで
/// Google Meet の会議中として扱う（会議アプリのない環境で自動録音の流れを確かめるため）。
#[cfg(debug_assertions)]
fn simulated_meeting(started: std::time::Instant) -> Option<MeetingApp> {
    let spec = std::env::var("KIROSCRIBE_SIMULATE_MEETING").ok()?;
    let (from, to) = spec.split_once('-')?;
    let (from, to): (u64, u64) = (from.trim().parse().ok()?, to.trim().parse().ok()?);
    let elapsed = started.elapsed().as_secs();
    (from..to).contains(&elapsed).then_some(MeetingApp::GoogleMeet)
}

#[cfg(not(debug_assertions))]
fn simulated_meeting(_started: std::time::Instant) -> Option<MeetingApp> {
    None
}

fn simulating() -> bool {
    cfg!(debug_assertions) && std::env::var_os("KIROSCRIBE_SIMULATE_MEETING").is_some()
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

async fn handle(app: &AppHandle, event: MeetingEvent) {
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
