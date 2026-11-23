//! 会議の自動検出 → ノート作成と自動録音 → 会議が終わったら自動停止。

use std::sync::Mutex;
use std::time::Duration;

use chrono::Local;
use kiroscribe_core::meeting_record::MeetingRecorder;
use kiroscribe_core::meetings::{observe, MeetingApp, MeetingEvent, MeetingTracker};
use kiroscribe_core::notes::{Note, NotePatch};
use kiroscribe_core::secrets::{require_api_key, Provider};
use kiroscribe_core::settings::Settings;
use kiroscribe_core::time::now_ms;
use tauri::{Emitter, Manager};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use crate::background::{show_popup, PopupPayload};
use crate::error::{CmdResult, CommandError};
use crate::recording::{start_for_note, stop_now, RecorderState};
use crate::runtime::AppHandle;
use crate::state::AppState;

pub const EVENT_NOTE_CREATED: &str = "note-created";
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// 会議の検出の状態。
#[derive(Default)]
pub struct MeetingState {
    /// いま続いている会議（自動検出がオフのあいだに始まったものも含む）
    current: Mutex<Option<MeetingApp>>,
    /// 処理を 1 つずつ順に進めるための列
    queue: Mutex<Option<UnboundedSender<Job>>>,
    /// 検出の記録（メモリーの中だけ。利用者が「会議の検出の記録を保存」で書き出す）
    record: Mutex<MeetingRecorder>,
}

/// 順に処理すること。
enum Job {
    /// 会議の開始・終了が確定した
    Event(MeetingEvent),
    /// 設定の「会議の自動検出」がオフからオンに変わった
    AutoRecordEnabled,
}

/// 処理を列に入れる。前の処理（録音の停止と、残りの文字起こしの仕上げ）が終わってから、次の処理に進む。
/// 会議が終わってすぐ次の会議が始まったときに、前の録音の仕上げ中だからといって、次の会議の録音を取りこぼさない。
fn enqueue(app: &AppHandle, job: Job) {
    let state = app.state::<MeetingState>();
    let mut queue = state.queue.lock().unwrap();
    let sender = queue.get_or_insert_with(|| {
        let (sender, mut jobs) = unbounded_channel::<Job>();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            while let Some(job) = jobs.recv().await {
                match job {
                    Job::Event(event) => handle(&app, event).await,
                    Job::AutoRecordEnabled => on_auto_record_enabled(&app).await,
                }
            }
        });
        sender
    });
    let _ = sender.send(job);
}

/// 確定した会議の開始・終了を、順番待ちの列に入れる。
pub fn submit(app: &AppHandle, event: MeetingEvent) {
    enqueue(app, Job::Event(event));
}

/// 設定が保存された。「会議の自動検出」がオフからオンに変わったら、いま続いている会議の録音を始める。
pub fn settings_changed(app: &AppHandle, was_enabled: bool, settings: &Settings) {
    if !was_enabled && settings.auto_record_meetings {
        enqueue(app, Job::AutoRecordEnabled);
    }
}

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
                let simulated = crate::e2e::simulated_detection(tracker.active());
                #[cfg(not(feature = "e2e"))]
                let simulated: Option<(Option<kiroscribe_core::meetings::Detection>, bool)> = None;
                let event = match simulated {
                    Some((detected, continuing)) => tracker.tick(detected, continuing, now_ms()),
                    None => {
                        let now = now_ms();
                        let observation = observe();
                        let event = tracker.observe(&observation, own_exe.as_deref(), now);
                        app.state::<MeetingState>()
                            .record
                            .lock()
                            .unwrap()
                            .record(now, &observation, event);
                        event
                    }
                };
                if let Some(event) = event {
                    submit(&app, event);
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
        MeetingEvent::Started(meeting) => {
            *app.state::<MeetingState>().current.lock().unwrap() = Some(meeting);
            on_started(app, meeting).await;
        }
        MeetingEvent::Ended(meeting) => {
            {
                let state = app.state::<MeetingState>();
                let mut current = state.current.lock().unwrap();
                if *current == Some(meeting) {
                    *current = None;
                }
            }
            on_ended(app, meeting).await;
        }
    }
}

/// 「会議の自動検出」をオンにした時点で会議中なら、その会議の録音を始める。
/// 録音を始めるのは、会議の開始を検出したときと、このときだけ。利用者が録音を止めたあとに、
/// 続いている会議を勝手に録音し直すことはしない。
async fn on_auto_record_enabled(app: &AppHandle) {
    let current = *app.state::<MeetingState>().current.lock().unwrap();
    if let Some(meeting) = current {
        on_started(app, meeting).await;
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

    match start_for_note(app, note.id.clone(), Some(meeting)).await {
        Ok(_) => {
            // 画面の一覧には、録音を始められてから加える
            let _ = app.emit(EVENT_NOTE_CREATED, &note);
            show_popup(
                app,
                PopupPayload::Recording {
                    app: meeting.label().into(),
                    note_id: note.id,
                    title: note.title,
                },
            );
        }
        Err(err) => {
            // 録音できなかった会議の、タイトルだけの空のノートは残さない
            if let Err(delete_err) = state.db().delete_note(&note.id) {
                eprintln!("空のノートを消せませんでした: {delete_err}");
            }
            show_popup(
                app,
                PopupPayload::Error {
                    app: meeting.label().into(),
                    message: err.0,
                },
            );
        }
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

/// 会議の検出の記録を、保存ダイアログで選ばれた場所に JSON で書き出す。書き出した記録の件数を返す。
/// 誤って他の種類のファイルを上書きしないよう、拡張子は .json だけを受け付ける。
#[tauri::command(async)]
pub fn save_meeting_record(app: AppHandle, path: String) -> CmdResult<usize> {
    let path = std::path::PathBuf::from(path);
    let is_json = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("json"));
    if !is_json {
        return Err(CommandError("保存できるのは .json ファイルだけです".into()));
    }
    let own_exe = std::env::current_exe().ok().map(|p| p.to_string_lossy().to_string());
    let record = app
        .state::<MeetingState>()
        .record
        .lock()
        .unwrap()
        .export(&app.package_info().version.to_string(), own_exe.as_deref());
    let json = serde_json::to_string_pretty(&record)
        .map_err(|e| CommandError(format!("記録を書き出せませんでした: {e}")))?;
    std::fs::write(&path, json).map_err(|e| CommandError(format!("ファイルを保存できませんでした: {e}")))?;
    Ok(record.entries.len())
}
