//! 録音の状態管理と、画面へのイベント通知。

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use kiroscribe_core::audio::capture::{self, AudioDevice};
use kiroscribe_core::meetings::MeetingApp;
use kiroscribe_core::recorder::RecorderSink;
use kiroscribe_core::recording_check::{CheckAction, RecordingCheck, Signals};
use kiroscribe_core::secrets::{require_api_key, Provider};
use kiroscribe_core::session::{RecordingSession, SessionOptions};
use kiroscribe_core::time::now_ms;
use kiroscribe_core::transcribe::{BoxFuture, OpenAiTranscriber, Transcribe};
use kiroscribe_core::transcript::{Segment, Speaker, TranscriptBreak};
use serde::Serialize;
use serde_json::json;
use tauri::{Emitter, Manager, State};

use crate::background::{popup_recording_stopped, recording_changed, show_popup, PopupPayload};
use crate::error::{CmdResult, CommandError};
use crate::runtime::AppHandle;
use crate::state::AppState;

pub const EVENT_STATUS: &str = "recording-status";
pub const EVENT_SEGMENT: &str = "transcript-segment";
pub const EVENT_LEVEL: &str = "audio-level";
pub const EVENT_ERROR: &str = "recording-error";
pub const EVENT_BREAK: &str = "transcript-break";

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
    /// 自分と相手のどちらかが最後に話した時刻（UNIX ミリ秒。録音の開始・再開の時刻も、話したものとして数える）
    last_speech: Arc<AtomicI64>,
    /// 長い録音で、会議が続いているかを確かめる状態
    check: RecordingCheck,
    /// 文字起こしの回数と時間（デバッグ用のログに書く）
    stats: Arc<TranscriptionStats>,
    /// 文字起こしのあるノートで始めた録音なら、そのときに足した区切りの ID
    resumed_break: Option<i64>,
}

impl ActiveRecording {
    /// `note_id` の録音で、止める時刻が `deadline` の確認を、いま出しているか。
    fn asks(&self, note_id: &str, deadline: i64) -> bool {
        self.note_id == note_id && self.check.deadline() == Some(deadline)
    }

    /// 一時停止していた時間を除いた録音の時間。
    fn elapsed_ms(&self, now_ms: i64) -> i64 {
        self.elapsed_before_ms + self.running_since.map_or(0, |since| (now_ms - since).max(0))
    }

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

/// 確かめた結果（[`RecorderState::check`]）。
pub struct CheckStep {
    pub note_id: String,
    pub auto: Option<MeetingApp>,
    pub action: CheckAction,
    /// 確認を出していれば、答えがないときに止める時刻（UNIX ミリ秒）
    pub deadline: Option<i64>,
}

/// 確認への答えを受け付けた結果（[`RecorderState::keep`]・[`RecorderState::take_for_answer`]）。
pub enum Answer<T> {
    /// その確認を出している録音に、答えを当てはめた
    Applied(T),
    /// その確認はもう有効でない。`note_busy` は、そのノートがいまも録音中・仕上げ中か
    Stale { note_busy: bool },
}

impl RecorderState {
    /// 録音中（または仕上げ中）のノート ID。
    pub async fn busy_note_id(&self) -> Option<String> {
        if let Some(active) = self.active.lock().await.as_ref() {
            return Some(active.note_id.clone());
        }
        self.finishing.lock().unwrap().clone()
    }

    /// 長い録音で、会議が続いているかを確かめる（数秒ごとに呼ぶ）。確認を出す・止める・取り下げるときだけ、
    /// 録音のロックを持ったまま `on_step` を呼ぶ（その間に、録音が止まったり、答えが届いたりしないように）。
    /// 止めるときは、同じロックの中で録音を取り出して返す（仕上げは [`finish_stop`] で行う）。
    pub(crate) async fn check(
        &self,
        meeting_detected: bool,
        now_ms: i64,
        on_step: impl FnOnce(&CheckStep),
    ) -> Option<(CheckStep, Option<TakenRecording>)> {
        let mut active = self.active.lock().await;
        let recording = active.as_mut()?;
        let signals = Signals {
            elapsed_ms: recording.elapsed_ms(now_ms),
            paused: recording.session.is_paused(),
            meeting_detected,
            quiet_ms: now_ms - recording.last_speech.load(Ordering::SeqCst),
        };
        let action = recording.check.tick(&signals, now_ms)?;
        let step = CheckStep {
            note_id: recording.note_id.clone(),
            auto: recording.auto,
            action,
            deadline: recording.check.deadline(),
        };
        let taken = matches!(action, CheckAction::Stop(_)).then(|| self.take(&mut active)).flatten();
        on_step(&step);
        Some((step, taken))
    }

    /// 確認に「続ける」と答えた。`deadline` は、答えた確認の、止める時刻（どの確認への答えかを表す）。
    /// その確認をいま出している録音なら、30 分後まで確かめないようにし、録音のロックを持ったまま `on_kept` を呼ぶ。
    pub(crate) async fn keep(
        &self,
        note_id: &str,
        deadline: i64,
        now_ms: i64,
        on_kept: impl FnOnce(Option<MeetingApp>),
    ) -> Answer<()> {
        let mut active = self.active.lock().await;
        match active.as_mut().filter(|recording| recording.asks(note_id, deadline)) {
            Some(recording) => {
                let elapsed = recording.elapsed_ms(now_ms);
                recording.check.keep(elapsed);
                on_kept(recording.auto);
                Answer::Applied(())
            }
            None => Answer::Stale {
                note_busy: self.busy_with(&active, note_id),
            },
        }
    }

    /// 確認に「止める」と答えた。その確認をいま出している録音なら、取り出して返す（仕上げは [`finish_stop`] で行う）。
    pub(crate) async fn take_for_answer(&self, note_id: &str, deadline: i64) -> Answer<TakenRecording> {
        let mut active = self.active.lock().await;
        if active.as_ref().is_some_and(|recording| recording.asks(note_id, deadline)) {
            if let Some(taken) = self.take(&mut active) {
                return Answer::Applied(taken);
            }
        }
        Answer::Stale {
            note_busy: self.busy_with(&active, note_id),
        }
    }

    /// 録音を取り出し、仕上げ中にする（録音のロックを持ったまま呼ぶ。録音中でも仕上げ中でもない間ができないように）。
    fn take(&self, active: &mut Option<ActiveRecording>) -> Option<TakenRecording> {
        let recording = active.take()?;
        *self.finishing.lock().unwrap() = Some(recording.note_id.clone());
        Some(TakenRecording(recording))
    }

    fn busy_with(&self, active: &Option<ActiveRecording>, note_id: &str) -> bool {
        active.as_ref().is_some_and(|recording| recording.note_id == note_id)
            || self.finishing.lock().unwrap().as_deref() == Some(note_id)
    }

    /// 録音を `ms` だけ前に始めたことにする（長い録音を確かめるテスト用。話した時刻は動かさない）。
    #[cfg(test)]
    pub async fn backdate(&self, ms: i64) {
        if let Some(recording) = self.active.lock().await.as_mut() {
            recording.elapsed_before_ms += ms;
        }
    }

    /// 最後に話した時刻を `at_ms` にする（長い録音を確かめるテスト用。先の時刻なら、話し声が戻ったことになる）。
    #[cfg(test)]
    pub async fn set_last_speech(&self, at_ms: i64) {
        if let Some(recording) = self.active.lock().await.as_ref() {
            recording.last_speech.store(at_ms, Ordering::SeqCst);
        }
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

/// 1 回の録音の、文字起こしの回数と時間（デバッグ用のログに書く）。
#[derive(Default)]
struct TranscriptionStats {
    ok: AtomicI64,
    failed: AtomicI64,
    total_ms: AtomicI64,
    max_ms: AtomicI64,
}

/// 文字起こしの成否と時間をログに書く（送った音声と、返ってきた文は書かない）。
struct LoggedTranscriber {
    inner: OpenAiTranscriber,
    app: AppHandle,
    note_id: String,
    stats: Arc<TranscriptionStats>,
}

impl Transcribe for LoggedTranscriber {
    fn transcribe(&self, wav: Vec<u8>, prompt: Option<String>) -> BoxFuture<'_, kiroscribe_core::Result<String>> {
        Box::pin(async move {
            let bytes = wav.len();
            let started = std::time::Instant::now();
            crate::debug::detail(&self.app, "transcription.started", json!({ "noteId": self.note_id, "bytes": bytes }));
            let result = self.inner.transcribe(wav, prompt).await;
            let ms = started.elapsed().as_millis() as i64;
            self.stats.total_ms.fetch_add(ms, Ordering::SeqCst);
            self.stats.max_ms.fetch_max(ms, Ordering::SeqCst);
            match &result {
                Ok(text) => {
                    self.stats.ok.fetch_add(1, Ordering::SeqCst);
                    crate::debug::detail(
                        &self.app,
                        "transcription.finished",
                        json!({ "noteId": self.note_id, "ms": ms, "chars": text.chars().count() }),
                    );
                }
                Err(err) => {
                    self.stats.failed.fetch_add(1, Ordering::SeqCst);
                    crate::debug::error(
                        &self.app,
                        "transcription.failed",
                        json!({ "noteId": self.note_id, "ms": ms, "bytes": bytes, "reason": err.to_string() }),
                    );
                }
            }
            result
        })
    }
}

/// パイプラインからの結果を DB に保存し、画面に通知する。
struct TauriSink {
    app: AppHandle,
    note_id: String,
    last_speech: Arc<AtomicI64>,
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

    fn on_speech(&self, _speaker: Speaker, ended_at: i64) {
        self.last_speech.fetch_max(ended_at, Ordering::SeqCst);
    }

    fn on_level(&self, speaker: Speaker, level: f32) {
        let _ = self.app.emit(EVENT_LEVEL, LevelEvent { speaker, level });
    }

    fn on_error(&self, message: String) {
        crate::debug::error(&self.app, "recording.error", json!({ "noteId": self.note_id, "message": message }));
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
            match stopped.auto {
                // 自動で始めた録音なら、ポップアップでも知らせる（メインウィンドウを閉じていても分かるように）
                Some(meeting) => show_popup(
                    &app,
                    PopupPayload::Error {
                        app: meeting.label().into(),
                        message,
                    },
                ),
                // 手動の録音で、会議が終わったかの確認を出していたら、停止の表示にする（答えられない確認を残さない）
                None => popup_recording_stopped(&app, &stopped.note_id),
            }
        });
    }
}

fn emit_status(app: &AppHandle, status: &RecordingStatus) {
    recording_changed(app, status.state);
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
pub fn list_transcript_breaks(state: State<'_, AppState>, note_id: String) -> CmdResult<Vec<TranscriptBreak>> {
    Ok(state.db().list_breaks(&note_id)?)
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
    let result = start_inner(app, note_id.clone(), auto).await;
    if let Err(err) = &result {
        crate::debug::error(
            app,
            "recording.start_failed",
            json!({ "noteId": note_id, "auto": auto, "reason": err.0 }),
        );
    }
    result
}

async fn start_inner(app: &AppHandle, note_id: String, auto: Option<MeetingApp>) -> CmdResult<RecordingStatus> {
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
    let stats = Arc::new(TranscriptionStats::default());
    let transcriber = Arc::new(LoggedTranscriber {
        inner: OpenAiTranscriber::new(&api_key, &settings.transcription_model, &settings.language),
        app: app.clone(),
        note_id: note_id.clone(),
        stats: stats.clone(),
    });
    let last_speech = Arc::new(AtomicI64::new(now_ms()));
    let sink = Arc::new(TauriSink {
        app: app.clone(),
        note_id: note_id.clone(),
        last_speech: last_speech.clone(),
    });
    // 文字起こしのあるノートで始めるときは、始める位置に区切りを付ける（新しい発話は、この時刻より後に始まる）
    let resumed_break = state.db().add_break_if_resumed(&note_id, now_ms())?;
    let session = match RecordingSession::start(
        SessionOptions {
            mic_device_id: settings.input_device_id.clone(),
            capture_system_audio: settings.capture_system_audio,
        },
        transcriber,
        sink,
        &tokio::runtime::Handle::current(),
    ) {
        Ok(session) => session,
        Err(err) => {
            // 録音を始められなかったときは、区切りを残さない
            if let Some(added) = &resumed_break {
                let _ = state.db().remove_break_if_empty(added.id);
            }
            return Err(err.into());
        }
    };
    if let Some(added) = &resumed_break {
        let _ = app.emit(EVENT_BREAK, added);
    }

    crate::debug::info(
        app,
        "recording.started",
        json!({
            "noteId": note_id,
            "auto": auto,
            "micSelected": settings.input_device_id.is_some(),
            "systemAudio": settings.capture_system_audio,
            "sources": session.sources(),
            "transcriptionModel": settings.transcription_model,
            "language": settings.language,
        }),
    );
    let recording = ActiveRecording {
        note_id,
        session,
        elapsed_before_ms: 0,
        running_since: Some(now_ms()),
        auto,
        last_speech,
        check: RecordingCheck::default(),
        stats,
        resumed_break: resumed_break.map(|added| added.id),
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
    crate::debug::info(&app, "recording.paused", json!({ "noteId": recording.note_id }));
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
        let now = now_ms();
        recording.running_since = Some(now);
        // 一時停止していた間は話せないので、再開した時点を、話したものとして数える
        recording.last_speech.fetch_max(now, Ordering::SeqCst);
    }
    recording.session.resume();
    crate::debug::info(&app, "recording.resumed", json!({ "noteId": recording.note_id }));
    let status = recording.status();
    emit_status(&app, &status);
    Ok(status)
}

/// 止めた録音。
pub struct Stopped {
    pub note_id: String,
    /// 会議の自動検出で始めた録音なら、その会議アプリ
    pub auto: Option<MeetingApp>,
}

/// 止めるために取り出した録音（録音のロックの中で取り出し、仕上げ中にしてある）。
pub(crate) struct TakenRecording(ActiveRecording);

/// 録音を止め、残りの文字起こしが終わるまで待つ。録音していなければ `None`。
/// `only_note` を渡したときは、そのノートの録音だけを止める（その間に始まった別のノートの録音には触れない）。
pub async fn stop_active(app: &AppHandle, only_note: Option<&str>) -> Option<Stopped> {
    let recorder = app.state::<RecorderState>();
    let taken = {
        let mut active = recorder.active.lock().await;
        let matches = active
            .as_ref()
            .is_some_and(|current| only_note.is_none_or(|note_id| note_id == current.note_id));
        if matches {
            recorder.take(&mut active)
        } else {
            None
        }
    }?;
    Some(finish_stop(app, taken).await)
}

/// 取り出した録音を止め、残りの文字起こしが終わるまで待つ。
pub(crate) async fn finish_stop(app: &AppHandle, taken: TakenRecording) -> Stopped {
    let recorder = app.state::<RecorderState>();
    let TakenRecording(recording) = taken;
    emit_status(app, &recorder.status().await);

    let stopped = Stopped {
        note_id: recording.note_id.clone(),
        auto: recording.auto,
    };
    let elapsed_ms = recording.elapsed_ms(now_ms());
    crate::debug::info(
        app,
        "recording.stopping",
        json!({ "noteId": recording.note_id, "auto": recording.auto, "elapsedMs": elapsed_ms }),
    );
    let finishing_since = std::time::Instant::now();
    let stats = recording.stats.clone();
    recording.session.stop().await;
    crate::debug::info(
        app,
        "recording.stopped",
        json!({
            "noteId": stopped.note_id,
            "finishMs": finishing_since.elapsed().as_millis() as i64,
            "transcribed": stats.ok.load(Ordering::SeqCst),
            "transcriptionFailed": stats.failed.load(Ordering::SeqCst),
            "transcriptionTotalMs": stats.total_ms.load(Ordering::SeqCst),
            "transcriptionMaxMs": stats.max_ms.load(Ordering::SeqCst),
        }),
    );
    // 何も話さないまま止めた回の区切りは残さない
    if let Some(break_id) = recording.resumed_break {
        if let Err(err) = app.state::<AppState>().db().remove_break_if_empty(break_id) {
            eprintln!("空の区切りを消せませんでした: {err}");
        }
    }

    *recorder.finishing.lock().unwrap() = None;
    emit_status(app, &recorder.status().await);
    stopped
}

/// 録音を止め、残りの文字起こしが終わるまで待ってから、いまの状態を返す。
pub async fn stop_now(app: &AppHandle) -> RecordingStatus {
    stop_active(app, None).await;
    app.state::<RecorderState>().status().await
}

/// 利用者が録音を止めた（メインウィンドウの「停止」とトレイの「録音を停止」）。
/// ポップアップがその録音を知らせていたら（自動で始めた録音・会議が終わったかの確認）、停止の表示にする。
pub async fn stop_by_user(app: &AppHandle) -> RecordingStatus {
    if let Some(stopped) = stop_active(app, None).await {
        popup_recording_stopped(app, &stopped.note_id);
    }
    app.state::<RecorderState>().status().await
}

#[tauri::command]
pub async fn stop_recording(app: AppHandle) -> CmdResult<RecordingStatus> {
    Ok(stop_by_user(&app).await)
}
