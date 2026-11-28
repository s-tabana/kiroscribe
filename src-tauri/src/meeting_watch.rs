//! 会議の自動検出 → ノート作成と自動録音 → 会議が終わったら自動停止。

use std::sync::Mutex;
use std::time::Duration;

use chrono::Local;
use kiroscribe_core::audio_sessions;
use kiroscribe_core::debug_log::MAX_VALUES;
use kiroscribe_core::meeting_record::{self, MeetingRecord, MeetingRecorder};
use kiroscribe_core::meeting_rules::Rules;
use kiroscribe_core::meetings::{
    simulated_evaluation, Browsers, Decision, Judge, MeetingApp, MeetingDetector, MeetingEvent, ServiceState, Step,
};
use kiroscribe_core::notes::{Note, NotePatch};
use kiroscribe_core::secrets::{require_api_key, Provider};
use kiroscribe_core::settings::Settings;
use kiroscribe_core::time::now_ms;
use serde_json::{json, Value};
use tauri::{Emitter, Manager};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use crate::background::{show_popup, PopupPayload};
use crate::recording::{start_for_note, stop_now, RecorderState};
use crate::runtime::AppHandle;
use crate::state::AppState;

pub const EVENT_NOTE_CREATED: &str = "note-created";
const POLL_INTERVAL: Duration = Duration::from_millis(kiroscribe_core::meetings::POLL_MS as u64);

/// 会議の検出の状態。
#[derive(Default)]
pub struct MeetingState {
    /// いま続いている会議（自動検出がオフのあいだに始まったものも含む）
    current: Mutex<Option<MeetingApp>>,
    /// 処理を 1 つずつ順に進めるための列
    queue: Mutex<Option<UnboundedSender<Job>>>,
    /// 検出の記録（メモリーの中だけ。利用者が「デバッグ用のログを保存」で書き出す）
    record: Mutex<MeetingRecorder>,
}

impl MeetingState {
    /// いま続いている会議（開始を確定してから、終了を確定するまで）。
    pub fn current_meeting(&self) -> Option<MeetingApp> {
        *self.current.lock().unwrap()
    }
}

/// 順に処理すること。
enum Job {
    /// 会議の開始・終了が確定した（理由は、ログに書く形）
    Event(MeetingEvent, Option<Value>),
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
                    Job::Event(event, reason) => handle_event(&app, event, reason).await,
                    Job::AutoRecordEnabled => on_auto_record_enabled(&app).await,
                }
            }
        });
        sender
    });
    let _ = sender.send(job);
}

/// 確定した会議の開始・終了を、順番待ちの列に入れる（テストで、検出のスレッドの代わりに使う）。
#[cfg(test)]
pub fn submit(app: &AppHandle, event: MeetingEvent) {
    enqueue(app, Job::Event(event, None));
}

/// 判定が確定した開始・終了を、理由と一緒に順番待ちの列に入れる。
fn submit_decision(app: &AppHandle, decision: Decision) {
    let reason = json!({
        "reason": serde_json::to_value(&decision.reason).unwrap_or(Value::Null),
        "why": decision.reason.describe(),
    });
    enqueue(app, Job::Event(decision.event, Some(reason)));
}

/// 設定が保存された。「会議の自動検出」がオフからオンに変わったら、いま続いている会議の録音を始める。
pub fn settings_changed(app: &AppHandle, was_enabled: bool, settings: &Settings) {
    if !was_enabled && settings.auto_record_meetings {
        enqueue(app, Job::AutoRecordEnabled);
    }
}

/// 検出を続けるスレッドを起動する。
pub fn spawn(app: AppHandle) {
    let spawned = std::thread::Builder::new()
        .name("kiroscribe-meetings".into())
        .spawn(move || {
            // 検出の道具（UI Automation の部品を含む）は、このスレッドで作り、このスレッドだけで使う
            #[allow(unused_mut)]
            let mut detector = MeetingDetector::new(Browsers::builtin());
            // 判定の様子を、変わったときと、一定の間隔ごとに書く
            let mut check_log = CheckLog::default();
            // E2E テスト用のビルドでだけ、待ち時間を縮め、実際の検出の代わりにテストが決めた状態を使う（src/e2e.rs）
            #[cfg(feature = "e2e")]
            crate::e2e::tune_judge(&mut detector.judge);
            loop {
                std::thread::sleep(poll_interval());
                let now = now_ms();
                #[cfg(feature = "e2e")]
                let simulated = crate::e2e::simulated_meeting();
                #[cfg(not(feature = "e2e"))]
                let simulated: Option<Option<MeetingApp>> = None;
                let decisions = match simulated {
                    Some(meeting) => detector.judge.tick(&simulated_evaluation(meeting), now),
                    None => {
                        let step = detector.step(now);
                        let verbose = crate::debug::verbose(&app);
                        app.state::<MeetingState>().record.lock().unwrap().record(
                            now,
                            &step.evaluation,
                            &step.facts,
                            &step.decisions,
                            verbose,
                        );
                        let check = Check {
                            step: &step,
                            judge: &detector.judge,
                            rules: detector.rules(),
                            browsers: detector.browsers(),
                            verbose,
                        };
                        if let Some(fields) = check_log.next(&check, now) {
                            crate::debug::info(&app, "meeting.check", fields);
                        }
                        step.decisions
                    }
                };
                for decision in decisions {
                    submit_decision(&app, decision);
                }
            }
        });
    if let Err(err) = spawned {
        eprintln!("会議の検出を開始できませんでした: {err}");
    }
}

fn app_value(app: Option<MeetingApp>) -> Value {
    serde_json::to_value(app).unwrap_or(Value::Null)
}

/// 判定の様子を変わらなくても書き直す間隔（ふだん・「詳しい記録」のとき）。
/// 記録の時刻が止まったまま会議が始まったとき（様子が変わらない）にも、その間の様子が残るようにする。
const CHECK_HEARTBEAT_MS: i64 = 60_000;
const CHECK_HEARTBEAT_VERBOSE_MS: i64 = 15_000;

/// 1 回分の判定の材料。
pub(crate) struct Check<'a> {
    pub step: &'a Step,
    pub judge: &'a Judge,
    pub rules: &'a Rules,
    pub browsers: &'a Browsers,
    pub verbose: bool,
}

/// 会議の検出の判定の様子を、ログに書く形にする。なぜ録音を始めた・始めなかったかが後から分かるように、
/// 段階（いまの会議・ほかの会議）、サービスごとの判定と立っている印と文脈、録音・再生のオーディオのセッションの様子を入れる。
/// 窓やタブの題と、当たった部品の名前は、「詳しい記録」のときだけ入れる。利用者の名前を含むフルパスは入れない。
/// `now` を渡すと、比べない値（確定待ちのミリ秒、再生の音の有無、窓の中を調べた様子とかかった時間）も入れる。
/// 参加中のサービスの印は、比べる値には入れない（話すたびに再生の音の印が変わって、書きすぎないため）。
pub(crate) fn check_fields(check: &Check<'_>, now: Option<i64>) -> Value {
    let judge = check.judge;
    let phase = judge.phase();
    let services: Vec<Value> = judge
        .statuses()
        .into_iter()
        .filter(|status| status.state != ServiceState::None)
        .map(|status| {
            let mut service = json!({
                "app": status.app,
                "state": status.state,
                "context": status.context.as_ref().map(|context| context.describe()),
            });
            if now.is_some() || status.state != ServiceState::Joined {
                service["signals"] = json!(status.signals);
            }
            service
        })
        .collect();
    let facts = &check.step.facts;
    let target = |exe: &str| check.rules.is_target(exe, check.browsers);
    let peak = check.rules.audible_peak;
    let mut render = serde_json::to_value(audio_sessions::report(&facts.render, &target, peak, false)).unwrap_or(Value::Null);
    if now.is_none() {
        if let Some(rows) = render.get_mut("sessions").and_then(Value::as_array_mut) {
            for row in rows {
                if let Some(row) = row.as_object_mut() {
                    row.remove("audible");
                }
            }
        }
    }
    let mut fields = json!({
        "phase": phase.name(),
        "phaseApp": app_value(phase.app()),
        "otherMeetings": judge.others(),
        "services": services,
        "capture": audio_sessions::report(&facts.capture, &target, peak, false),
        "render": render,
    });
    if let Some(now) = now {
        if let Some(since) = judge.confirming_since() {
            fields["confirmingMs"] = json!(now - since);
        }
        if let Some(scan) = &facts.scan {
            fields["controls"] = json!({
                "windows": scan.windows.len(),
                "failedWindows": scan.windows.iter().filter(|window| window.error.is_some()).count(),
                "elapsedMs": scan.elapsed_ms,
                "error": scan.error,
            });
        }
    }
    if check.verbose {
        let titles = meeting_record::titles(&check.step.evaluation, facts);
        fields["windowTitles"] = json!(titles.iter().map(|title| format!("{}: {}", title.app, title.title)).collect::<Vec<_>>());
        let names: Vec<String> = facts
            .scan
            .iter()
            .flat_map(|scan| scan.windows.iter())
            .filter(|scanned| titles.iter().any(|title| title.window == scanned.hwnd))
            .flat_map(|scanned| {
                let exe = facts.windows.iter().find(|w| w.hwnd == scanned.hwnd).map(|w| w.exe.clone()).unwrap_or_default();
                scanned.controls.iter().map(move |control| format!("{exe}: {}（{}）", control.name, control.kind.name()))
            })
            .collect();
        fields["controlNames"] = json!(names);
    }
    fields
}

fn value_count(value: &Value) -> usize {
    1 + match value {
        Value::Array(items) => items.iter().map(value_count).sum(),
        Value::Object(fields) => fields.values().map(value_count).sum(),
        _ => 0,
    }
}

/// ログの値数の上限に合わせて、一覧の末尾から行単位で減らす（各一覧の先頭は残す）。
fn limit_check_fields(fields: &mut Value) {
    let mut count = value_count(fields);
    for path in ["/controlNames", "/windowTitles", "/render/sessions", "/capture/sessions", "/services"] {
        if let Some(rows) = fields.pointer_mut(path).and_then(Value::as_array_mut) {
            while count > MAX_VALUES && rows.len() > 1 {
                count -= value_count(&rows.pop().unwrap());
            }
        }
    }
    if let Some(services) = fields.pointer_mut("/services").and_then(Value::as_array_mut) {
        for service in services {
            if let Some(rows) = service.get_mut("signals").and_then(Value::as_array_mut) {
                while count > MAX_VALUES && rows.len() > 1 {
                    count -= value_count(&rows.pop().unwrap());
                }
            }
        }
    }
}

/// 判定の様子を書くかどうかを決める: 様子が変わったとき（比べない値は比べない）と、
/// 前に書いてから一定の時間（[`CHECK_HEARTBEAT_MS`]）がたったとき。
#[derive(Default)]
pub(crate) struct CheckLog {
    last_key: Option<Value>,
    last_written_ms: i64,
}

impl CheckLog {
    /// 書くなら、書く値を返す。
    pub(crate) fn next(&mut self, check: &Check<'_>, now: i64) -> Option<Value> {
        let key = check_fields(check, None);
        let heartbeat = if check.verbose { CHECK_HEARTBEAT_VERBOSE_MS } else { CHECK_HEARTBEAT_MS };
        let changed = self.last_key.as_ref() != Some(&key);
        if !changed && now - self.last_written_ms < heartbeat {
            return None;
        }
        self.last_key = Some(key);
        self.last_written_ms = now;
        let mut fields = check_fields(check, Some(now));
        fields["changed"] = json!(changed);
        limit_check_fields(&mut fields);
        Some(fields)
    }
}

/// 確定した会議の開始・終了を書く（判定が確定したものは、理由も書く）。
fn log_event(app: &AppHandle, event: MeetingEvent, reason: Option<Value>) {
    let (name, meeting) = match event {
        MeetingEvent::Started(meeting) => ("meeting.started", meeting),
        MeetingEvent::Ended(meeting) => ("meeting.ended", meeting),
    };
    let mut fields = json!({ "app": meeting });
    if let Some(Value::Object(reason)) = reason {
        for (key, value) in reason {
            fields[key] = value;
        }
    }
    crate::debug::info(app, name, fields);
}

/// 会議の開始を受けて、自動で録音を始めなかった理由を書く。
fn log_skipped(app: &AppHandle, meeting: MeetingApp, reason: &str, message: Option<&str>) {
    crate::debug::info(
        app,
        "meeting.auto_record.skipped",
        json!({ "app": meeting, "reason": reason, "message": message }),
    );
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

/// 確定した会議の開始・終了を処理する（テストで、順番待ちの列を通さずに使う）。
#[cfg(test)]
pub(crate) async fn handle(app: &AppHandle, event: MeetingEvent) {
    handle_event(app, event, None).await
}

async fn handle_event(app: &AppHandle, event: MeetingEvent, reason: Option<Value>) {
    log_event(app, event, reason);
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
    crate::debug::info(app, "meeting.auto_record.enabled", json!({ "currentMeeting": current }));
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
        log_skipped(app, meeting, "autoRecordOff", Some("設定の「会議の自動検出」がオフ"));
        return;
    }
    // 手動で録音中なら何もしない
    if app.state::<RecorderState>().busy_note_id().await.is_some() {
        log_skipped(app, meeting, "alreadyRecording", Some("ほかの録音（または仕上げ）の最中"));
        return;
    }
    if require_api_key(state.secrets.as_ref(), Provider::OpenAi).is_err() {
        log_skipped(app, meeting, "noOpenAiKey", Some("OpenAI の API キーが未設定"));
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
            log_skipped(app, meeting, "noteFailed", Some(&message));
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
            crate::debug::info(app, "meeting.auto_record.started", json!({ "app": meeting, "noteId": note.id }));
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
            log_skipped(app, meeting, "startFailed", Some(&err.0));
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
        crate::debug::info(app, "meeting.auto_stop.skipped", json!({ "app": meeting, "reason": "noAutoRecording" }));
        return;
    };
    if auto != meeting {
        crate::debug::info(
            app,
            "meeting.auto_stop.skipped",
            json!({ "app": meeting, "reason": "otherMeeting", "recording": auto }),
        );
        return;
    }
    crate::debug::info(app, "meeting.auto_stop", json!({ "app": meeting, "noteId": note_id }));
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

/// 会議の検出の記録（デバッグ用のログの書き出しに入れる）。
pub fn export_record(app: &AppHandle) -> MeetingRecord {
    app.state::<MeetingState>().record.lock().unwrap().export(&app.package_info().version.to_string())
}


#[cfg(test)]
mod tests {
    use kiroscribe_core::audio_sessions::{AudioSession, AudioSessions, SessionState};
    use kiroscribe_core::meeting_facts::{ControlFact, ControlScan, ControlType, Facts, ScannedWindow, WindowFact};
    use kiroscribe_core::meeting_rules::evaluate;

    use super::*;

    fn session(path: &str, state: SessionState, peak: Option<f32>) -> AudioSession {
        AudioSession { pid: 1200, exe_path: path.into(), package: None, state, peak }
    }

    /// Meet のタブ（題と退出ボタン）と、ブラウザのマイク・再生の音。ほかに、会議でないアプリのセッション。
    fn meeting_facts(peak: f32) -> Facts {
        Facts {
            windows: vec![WindowFact {
                hwnd: 77,
                exe: "chrome.exe".into(),
                class: "Chrome_WidgetWin_1".into(),
                title: "Meet - abc-defg-hij - yamada@example.com - Google Chrome".into(),
                visible: true,
            }],
            capture: AudioSessions {
                available: true,
                endpoints: 1,
                sessions: vec![
                    session(r"C:\Users\山田 太郎\AppData\Local\Google\Chrome\Application\chrome.exe", SessionState::Active, None),
                    session(r"C:\Users\山田 太郎\AppData\Local\Recorder\rec.exe", SessionState::Active, None),
                ],
                ..Default::default()
            },
            render: AudioSessions {
                available: true,
                endpoints: 1,
                sessions: vec![session(r"C:\Users\山田 太郎\AppData\Local\Google\Chrome\Application\chrome.exe", SessionState::Active, Some(peak))],
                ..Default::default()
            },
            scan: Some(ControlScan {
                windows: vec![ScannedWindow {
                    hwnd: 77,
                    controls: vec![ControlFact { kind: ControlType::Button, name: "通話から退出".into() }],
                    seen: 120,
                    error: None,
                }],
                elapsed_ms: 85,
                error: None,
            }),
        }
    }

    fn step(facts: Facts, judge: &mut Judge, now: i64) -> Step {
        let evaluation = evaluate(Rules::builtin(), &facts, &Browsers::builtin());
        let decisions = judge.tick(&evaluation, now);
        Step { facts, evaluation, decisions }
    }

    /// ログに書く判定の様子: 段階、サービスごとの判定と立っている印と文脈、対象のアプリの録音・再生のセッション、
    /// 窓の中を調べた様子。窓の題と部品の名前は、「詳しい記録」のときだけ入れる。利用者の名前を含むフルパスは入れない。
    // [LOG-005][LOG-010]
    #[test]
    fn check_fields_tell_why_and_include_titles_only_when_verbose() {
        let mut judge = Judge::new(Rules::builtin());
        let step = step(meeting_facts(0.2), &mut judge, 0);
        let browsers = Browsers::builtin();
        let check = |verbose| Check { step: &step, judge: &judge, rules: Rules::builtin(), browsers: &browsers, verbose };
        let normal = check_fields(&check(false), Some(1_000));
        assert_eq!((&normal["phase"], &normal["phaseApp"]), (&json!("active"), &json!("googleMeet")));
        assert_eq!(normal["otherMeetings"], json!([]));
        let meet = &normal["services"][0];
        assert_eq!((&meet["app"], &meet["state"], &meet["context"]), (&json!("googleMeet"), &json!("joined"), &json!("chrome.exe の窓")));
        assert_eq!(
            meet["signals"],
            json!(["退出ボタン（強）", "Meet のタブの題（中）", "ブラウザのマイク（弱）", "ブラウザの再生の音（弱）"])
        );
        assert_eq!(normal["capture"]["status"], "ok");
        assert_eq!(normal["capture"]["total"], 2);
        assert_eq!(normal["capture"]["sessions"], json!([{ "exe": "chrome.exe", "state": "active" }]), "対象のアプリのセッションだけ");
        assert_eq!(normal["render"]["sessions"], json!([{ "exe": "chrome.exe", "state": "active", "audible": true }]));
        assert_eq!(normal["controls"], json!({ "windows": 1, "failedWindows": 0, "elapsedMs": 85, "error": null }));
        let text = normal.to_string();
        assert!(!text.contains("山田") && !text.contains("example.com") && !text.contains("通話から退出"), "{text}");

        let verbose = check_fields(&check(true), Some(1_000));
        assert_eq!(verbose["windowTitles"], json!(["chrome.exe: Meet - abc-defg-hij - yamada@example.com - Google Chrome"]));
        assert_eq!(verbose["controlNames"], json!(["chrome.exe: 通話から退出（Button）"]));
    }

    /// 組み合わせで開始の確定を待っている間は、待ち始めてからのミリ秒を書く。
    // [LOG-005]
    #[test]
    fn confirming_time_is_logged() {
        let mut facts = meeting_facts(0.0);
        facts.scan.as_mut().unwrap().windows[0].controls.clear();
        let mut judge = Judge::new(Rules::builtin());
        let step = step(facts, &mut judge, 10_000);
        let browsers = Browsers::builtin();
        let check = Check { step: &step, judge: &judge, rules: Rules::builtin(), browsers: &browsers, verbose: false };
        let fields = check_fields(&check, Some(12_000));
        assert_eq!((&fields["phase"], &fields["confirmingMs"]), (&json!("confirming"), &json!(2_000)));
        assert_eq!(fields["services"][0]["state"], "confirming");
    }

    /// 判定の様子は、変わったときと、変わらなくても 60 秒（詳しい記録のときは 15 秒）ごとに書く。
    /// 参加中に再生の音が鳴ったり止んだりしただけでは、書き直さない。
    // [LOG-005][LOG-010]
    #[test]
    fn check_log_writes_on_change_and_at_a_heartbeat() {
        let browsers = Browsers::builtin();
        let mut judge = Judge::new(Rules::builtin());
        let loud = step(meeting_facts(0.2), &mut judge, 0);
        let mut log = CheckLog::default();
        let first = log.next(&check_of(&loud, &judge, &browsers, false), 1_000_000).expect("最初は書く");
        assert_eq!(first["changed"], true);
        let quiet = step(meeting_facts(0.0), &mut judge, 2_000);
        assert!(log.next(&check_of(&quiet, &judge, &browsers, false), 1_002_000).is_none(), "再生の音の有無だけの変化");
        assert!(log.next(&check_of(&quiet, &judge, &browsers, false), 1_059_999).is_none());
        let beat = log.next(&check_of(&quiet, &judge, &browsers, false), 1_060_000).expect("60 秒たったら書く");
        assert_eq!(beat["changed"], false);

        let left = step(Facts::default(), &mut judge, 4_000);
        assert_eq!(log.next(&check_of(&left, &judge, &browsers, false), 1_062_000).unwrap()["changed"], true, "変わったらすぐ書く");

        let mut verbose_log = CheckLog::default();
        verbose_log.next(&check_of(&left, &judge, &browsers, true), 0);
        assert!(verbose_log.next(&check_of(&left, &judge, &browsers, true), 14_999).is_none());
        assert!(verbose_log.next(&check_of(&left, &judge, &browsers, true), 15_000).is_some(), "詳しい記録のときは 15 秒ごと");
    }

    fn check_of<'a>(step: &'a Step, judge: &'a Judge, browsers: &'a Browsers, verbose: bool) -> Check<'a> {
        Check { step, judge, rules: Rules::builtin(), browsers, verbose }
    }

    /// 対象のアプリのセッションや窓が多く、値の数の上限を超えても、ログのファイルに書いた行には、
    /// 段階と、サービスの判定と、セッションの件数が残る。
    // [LOG-005][LOG-009]
    #[test]
    fn a_check_with_many_sessions_keeps_the_verdict_in_the_log_file() {
        use kiroscribe_core::debug_log::{DebugLog, Level, Source};

        let mut facts = meeting_facts(0.2);
        let names = ["slack.exe", "ms-teams.exe", "zoom.exe", "discord.exe", "chrome.exe", "msedge.exe", "zen.exe", "firefox.exe"];
        for (i, name) in names.iter().cycle().take(20).enumerate() {
            let path = format!(r"C:\Users\山田 太郎\AppData\Local\app{i}\{name}");
            facts.capture.sessions.push(session(&path, SessionState::Inactive, None));
            facts.render.sessions.push(session(&path, SessionState::Active, Some(0.3)));
        }
        let mut judge = Judge::new(Rules::builtin());
        let step = step(facts, &mut judge, 0);
        let browsers = Browsers::builtin();
        let dir = tempfile::tempdir().unwrap();
        let log = DebugLog::open(dir.path()).unwrap();
        for verbose in [false, true] {
            let check = Check { step: &step, judge: &judge, rules: Rules::builtin(), browsers: &browsers, verbose };
            let fields = CheckLog::default().next(&check, 1_000).unwrap();
            assert!(value_count(&fields) <= MAX_VALUES, "{fields}");
            log.write(Level::Info, Source::App, "meeting.check", fields);
        }
        let text = std::fs::read_to_string(&log.files()[0]).unwrap();
        let lines: Vec<Value> = text.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let fields = &line["fields"];
            assert_eq!(fields["phase"], "active", "{line}");
            assert_eq!(fields["services"][0]["app"], "googleMeet");
            assert_eq!(fields["capture"]["total"], 22);
            assert_eq!(fields["render"]["total"], 21);
            assert!(fields["capture"]["sessions"].as_array().is_some_and(|rows| !rows.is_empty()));
        }
        assert!(!text.contains("山田"), "{text}");
    }

    /// 録音のセッションの一覧を読めなかったときは、その理由を書き、ほかの印で判定を続ける。
    // [LOG-005][MTG-020]
    #[test]
    fn check_logs_a_session_read_failure() {
        let mut facts = meeting_facts(0.2);
        facts.capture = audio_sessions::summarize(vec![Err("0x80070005".into())]);
        let mut judge = Judge::new(Rules::builtin());
        let step = step(facts, &mut judge, 0);
        let browsers = Browsers::builtin();
        let check = Check { step: &step, judge: &judge, rules: Rules::builtin(), browsers: &browsers, verbose: false };
        let fields = CheckLog::default().next(&check, 1_000).unwrap();
        assert_eq!(fields["capture"]["status"], "failed");
        assert_eq!(fields["capture"]["failedEndpoints"], 1);
        assert_eq!(fields["capture"]["error"], "0x80070005");
        assert_eq!(fields["phase"], "active", "退出ボタンで判定を続ける");
    }
}
