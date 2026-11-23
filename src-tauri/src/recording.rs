//! 録音の状態管理と、画面へのイベント通知。

use std::sync::{Arc, Mutex};

use kiroscribe_core::audio::capture::{self, AudioDevice};
use kiroscribe_core::meetings::MeetingApp;
use kiroscribe_core::recorder::RecorderSink;
use kiroscribe_core::secrets::{require_api_key, Provider};
use kiroscribe_core::session::{RecordingSession, SessionOptions};
use kiroscribe_core::time::now_ms;
use kiroscribe_core::transcribe::OpenAiTranscriber;
use kiroscribe_core::transcript::{Segment, Speaker};
use serde::Serialize;
use tauri::{Emitter, Manager, State};

use crate::background::{show_popup, PopupPayload};
use crate::error::{CmdResult, CommandError};
use crate::runtime::AppHandle;
use crate::state::AppState;

pub const EVENT_STATUS: &str = "recording-status";
pub const EVENT_SEGMENT: &str = "transcript-segment";
pub const EVENT_LEVEL: &str = "audio-level";
pub const EVENT_ERROR: &str = "recording-error";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordingState {
    Idle,
    Recording,
    Paused,
    /// 停止後、残りの文字起こしを待っている
    Finishing,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStatus {
    pub note_id: Option<String>,
    pub state: RecordingState,
    /// 一時停止を除いた録音時間のうち、`running_since` より前の分
    pub elapsed_ms: i64,
    /// 現在録音中ならその区間の開始時刻。画面側で経過時間を足す
    pub running_since: Option<i64>,
    pub sources: Vec<Speaker>,
}

impl RecordingStatus {
    fn idle() -> Self {
        Self {
            note_id: None,
            state: RecordingState::Idle,
            elapsed_ms: 0,
            running_since: None,
            sources: Vec::new(),
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct LevelEvent {
    speaker: Speaker,
    level: f32,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorEvent {
    note_id: String,
    message: String,
}

struct ActiveRecording {
    note_id: String,
    session: RecordingSession,
    elapsed_before_ms: i64,
    running_since: Option<i64>,
    /// 会議の自動検出で始めた録音なら、その会議アプリ
    auto: Option<MeetingApp>,
}

impl ActiveRecording {
    fn status(&self) -> RecordingStatus {
        RecordingStatus {
            note_id: Some(self.note_id.clone()),
            state: if self.session.is_paused() {
                RecordingState::Paused
            } else {
                RecordingState::Recording
            },
            elapsed_ms: self.elapsed_before_ms,
            running_since: self.running_since,
            sources: self.session.sources().to_vec(),
        }
    }
}

/// 同時に録音できるのは 1 ノートだけ。
#[derive(Default)]
pub struct RecorderState {
    active: tokio::sync::Mutex<Option<ActiveRecording>>,
    finishing: Mutex<Option<String>>,
}

impl RecorderState {
    /// 録音中（または仕上げ中）のノート ID。
    pub async fn busy_note_id(&self) -> Option<String> {
        if let Some(active) = self.active.lock().await.as_ref() {
            return Some(active.note_id.clone());
        }
        self.finishing.lock().unwrap().clone()
    }

    /// 自動検出で始めた録音が続いていれば、そのノートと会議アプリ。
    pub async fn auto_recording(&self) -> Option<(String, MeetingApp)> {
        let active = self.active.lock().await;
        let recording = active.as_ref()?;
        Some((recording.note_id.clone(), recording.auto?))
    }

    pub async fn status(&self) -> RecordingStatus {
        if let Some(active) = self.active.lock().await.as_ref() {
            return active.status();
        }
        match self.finishing.lock().unwrap().clone() {
            Some(note_id) => RecordingStatus {
                note_id: Some(note_id),
                state: RecordingState::Finishing,
                ..RecordingStatus::idle()
            },
            None => RecordingStatus::idle(),
        }
    }
}

/// パイプラインからの結果を DB に保存し、画面に通知する。
struct TauriSink {
    app: AppHandle,
    note_id: String,
}

impl RecorderSink for TauriSink {
    fn on_transcript(&self, speaker: Speaker, started_at: i64, ended_at: i64, text: String) {
        let state = self.app.state::<AppState>();
        let saved = state
            .db()
            .add_segment(&self.note_id, speaker, started_at, ended_at, &text);
        match saved {
            Ok(segment) => {
                let _ = self.app.emit(EVENT_SEGMENT, segment);
            }
            Err(err) => self.on_error(format!("文字起こしを保存できませんでした: {err}")),
        }
    }

    fn on_level(&self, speaker: Speaker, level: f32) {
        let _ = self.app.emit(EVENT_LEVEL, LevelEvent { speaker, level });
    }

    fn on_error(&self, message: String) {
        let _ = self.app.emit(
            EVENT_ERROR,
            ErrorEvent {
                note_id: self.note_id.clone(),
                message,
            },
        );
    }

    /// API キーの誤りでは、このあとの発話も文字起こしできないので、録音そのものを止める
    /// （「録音中」のまま、何も記録されない時間が続かないように）。知らせは、ここで 1 回だけ出す。
    fn on_auth_error(&self, message: String) {
        self.on_error(message.clone());
        let app = self.app.clone();
        let note_id = self.note_id.clone();
        // ここは文字起こしの処理の中なので、止めるのは別のタスクに任せる（停止は、この処理が終わるのを待つ）
        tauri::async_runtime::spawn(async move {
            let Some(stopped) = stop_active(&app, Some(&note_id)).await else {
                return;
            };
            // 自動で始めた録音なら、ポップアップでも知らせる（メインウィンドウを閉じていても分かるように）
            if let Some(meeting) = stopped.auto {
                show_popup(
                    &app,
                    PopupPayload::Error {
                        app: meeting.label().into(),
                        message,
                    },
                );
            }
        });
    }
}

fn emit_status(app: &AppHandle, status: &RecordingStatus) {
    let _ = app.emit(EVENT_STATUS, status.clone());
}

#[tauri::command(async)]
pub fn list_audio_devices() -> CmdResult<Vec<AudioDevice>> {
    Ok(capture::list_input_devices()?)
}

#[tauri::command]
pub fn list_segments(state: State<'_, AppState>, note_id: String) -> CmdResult<Vec<Segment>> {
    Ok(state.db().list_segments(&note_id)?)
}

#[tauri::command]
pub async fn get_recording_status(recorder: State<'_, RecorderState>) -> CmdResult<RecordingStatus> {
    Ok(recorder.status().await)
}

/// 録音を始める（画面からの操作と、会議の自動検出の両方から呼ばれる）。
pub async fn start_for_note(
    app: &AppHandle,
    note_id: String,
    auto: Option<MeetingApp>,
) -> CmdResult<RecordingStatus> {
    let state = app.state::<AppState>();
    let recorder = app.state::<RecorderState>();
    let mut active = recorder.active.lock().await;
    if let Some(current) = active.as_ref() {
        if current.note_id == note_id {
            return Ok(current.status());
        }
        return Err(CommandError(
            "別のノートで録音中です。先にそちらの録音を停止してください".into(),
        ));
    }
    if recorder.finishing.lock().unwrap().is_some() {
        return Err(CommandError(
            "前の録音の文字起こしを仕上げています。少し待ってから開始してください".into(),
        ));
    }

    let (settings, api_key) = {
        let db = state.db();
        db.get_note(&note_id)?;
        (
            db.load_settings()?,
            require_api_key(state.secrets.as_ref(), Provider::OpenAi)?,
        )
    };
    let transcriber = Arc::new(OpenAiTranscriber::new(
        &api_key,
        &settings.transcription_model,
        &settings.language,
    ));
    let sink = Arc::new(TauriSink {
        app: app.clone(),
        note_id: note_id.clone(),
    });
    let session = RecordingSession::start(
        SessionOptions {
            mic_device_id: settings.input_device_id.clone(),
            capture_system_audio: settings.capture_system_audio,
        },
        transcriber,
        sink,
        &tokio::runtime::Handle::current(),
    )?;

    let recording = ActiveRecording {
        note_id,
        session,
        elapsed_before_ms: 0,
        running_since: Some(now_ms()),
        auto,
    };
    let status = recording.status();
    *active = Some(recording);
    emit_status(app, &status);
    Ok(status)
}

#[tauri::command]
pub async fn start_recording(app: AppHandle, note_id: String) -> CmdResult<RecordingStatus> {
    start_for_note(&app, note_id, None).await
}

#[tauri::command]
pub async fn pause_recording(
    app: AppHandle,
    recorder: State<'_, RecorderState>,
) -> CmdResult<RecordingStatus> {
    let mut active = recorder.active.lock().await;
    let recording = active
        .as_mut()
        .ok_or_else(|| CommandError("録音していません".into()))?;
    if let Some(since) = recording.running_since.take() {
        recording.elapsed_before_ms += now_ms() - since;
    }
    recording.session.pause();
    let status = recording.status();
    emit_status(&app, &status);
    Ok(status)
}

#[tauri::command]
pub async fn resume_recording(
    app: AppHandle,
    recorder: State<'_, RecorderState>,
) -> CmdResult<RecordingStatus> {
    let mut active = recorder.active.lock().await;
    let recording = active
        .as_mut()
        .ok_or_else(|| CommandError("録音していません".into()))?;
    if recording.running_since.is_none() {
        recording.running_since = Some(now_ms());
    }
    recording.session.resume();
    let status = recording.status();
    emit_status(&app, &status);
    Ok(status)
}

/// 止めた録音。
pub struct Stopped {
    /// 会議の自動検出で始めた録音なら、その会議アプリ
    pub auto: Option<MeetingApp>,
}

/// 録音を止め、残りの文字起こしが終わるまで待つ。録音していなければ `None`。
/// `only_note` を渡したときは、そのノートの録音だけを止める（その間に始まった別のノートの録音には触れない）。
pub async fn stop_active(app: &AppHandle, only_note: Option<&str>) -> Option<Stopped> {
    let recorder = app.state::<RecorderState>();
    let recording = {
        let mut active = recorder.active.lock().await;
        let matches = active
            .as_ref()
            .is_some_and(|current| only_note.is_none_or(|note_id| note_id == current.note_id));
        if matches {
            active.take()
        } else {
            None
        }
    }?;
    *recorder.finishing.lock().unwrap() = Some(recording.note_id.clone());
    emit_status(app, &recorder.status().await);

    let stopped = Stopped { auto: recording.auto };
    recording.session.stop().await;

    *recorder.finishing.lock().unwrap() = None;
    emit_status(app, &recorder.status().await);
    Some(stopped)
}

/// 録音を止め、残りの文字起こしが終わるまで待ってから、いまの状態を返す。
pub async fn stop_now(app: &AppHandle) -> RecordingStatus {
    stop_active(app, None).await;
    app.state::<RecorderState>().status().await
}

#[tauri::command]
pub async fn stop_recording(app: AppHandle) -> CmdResult<RecordingStatus> {
    Ok(stop_now(&app).await)
}
