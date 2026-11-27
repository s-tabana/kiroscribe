//! 会議の自動検出 → ノート作成と自動録音 → 会議が終わったら自動停止。

use std::sync::Mutex;
use std::time::Duration;

use chrono::Local;
use kiroscribe_core::meeting_record::{MeetingRecord, MeetingRecorder};
use kiroscribe_core::meetings::{
    browser_titles, diagnose, observe_detailed, MeetingApp, MeetingEvent, MeetingTracker, Observation,
};
use kiroscribe_core::mic_store::{self, MicStore};
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
const POLL_INTERVAL: Duration = Duration::from_secs(2);

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
            // 判定の様子を、変わったときと、一定の間隔ごとに書く
            let mut check_log = CheckLog::default();
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
                        let (observation, store) = observe_detailed();
                        let event = tracker.observe(&observation, own_exe.as_deref(), now);
                        app.state::<MeetingState>()
                            .record
                            .lock()
                            .unwrap()
                            .record(now, &observation, event);
                        let check = Check {
                            observation: &observation,
                            store: store.as_ref(),
                            own_exe: own_exe.as_deref(),
                            tracker: &tracker,
                            verbose: crate::debug::verbose(&app),
                        };
                        if let Some(fields) = check_log.next(&check, now) {
                            crate::debug::info(&app, "meeting.check", fields);
                        }
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

fn app_value(app: Option<MeetingApp>) -> Value {
    serde_json::to_value(app).unwrap_or(Value::Null)
}

/// 判定の様子を変わらなくても書き直す間隔（ふだん・「詳しい記録」のとき）。
/// 記録の時刻が止まったまま会議が始まったとき（様子が変わらない）にも、その間の様子が残るようにする。
const CHECK_HEARTBEAT_MS: i64 = 60_000;
const CHECK_HEARTBEAT_VERBOSE_MS: i64 = 15_000;

/// 1 回分の判定の材料。
pub(crate) struct Check<'a> {
    pub observation: &'a Observation,
    /// マイクの使用状況の記録の生の値（Windows 以外では `None`）
    pub store: Option<&'a MicStore>,
    pub own_exe: Option<&'a str>,
    pub tracker: &'a MeetingTracker,
    pub verbose: bool,
}

/// 会議の検出の判定の様子を、ログに書く形にする。なぜ録音を始めた・始めなかったかが後から分かるように、
/// マイクを使っているアプリの名前、見たブラウザのウィンドウの数、判定とその理由、開始・終了の確定の段階、
/// マイクの使用状況の記録（開けたか、会議アプリに見える記録ごとの Stop の値・名前の分類・置き場所の区分）を入れる。
/// ウィンドウのタイトル（メールの件名なども入りうる）は、「詳しい記録」のときだけ入れる。
/// `now` を渡すと、調べるたびに変わる値（Stop からの経過秒、開始の確定を待ち始めてからのミリ秒）も入れる。
pub(crate) fn check_fields(check: &Check<'_>, now: Option<i64>) -> Value {
    let diagnosis = diagnose(check.observation, check.own_exe);
    let phase = check.tracker.phase();
    let mut fields = json!({
        "micInUse": diagnosis.mic_in_use,
        "browserWindows": diagnosis.browser_windows,
        "verdict": diagnosis.verdict,
        "why": diagnosis.verdict.describe(),
        "app": app_value(diagnosis.app),
        "phase": phase.name(),
        "phaseApp": app_value(phase.app()),
        "micStore": check.store.map(|store| mic_store::report(store, now)),
    });
    if let (Some(now), Some(since)) = (now, check.tracker.confirming_since()) {
        fields["confirmingMs"] = json!(now - since);
    }
    if check.verbose {
        fields["browserTitles"] = json!(browser_titles(check.observation, check.own_exe));
    }
    fields
}

/// 判定の様子を書くかどうかを決める: 様子が変わったとき（調べるたびに変わる値は比べない）と、
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
        Some(fields)
    }
}

/// 確定した会議の開始・終了を書く。
fn log_event(app: &AppHandle, event: MeetingEvent) {
    match event {
        MeetingEvent::Started(meeting) => crate::debug::info(app, "meeting.started", json!({ "app": meeting })),
        MeetingEvent::Ended(meeting) => crate::debug::info(app, "meeting.ended", json!({ "app": meeting })),
    }
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

/// 確定した会議の開始・終了を処理する。
pub(crate) async fn handle(app: &AppHandle, event: MeetingEvent) {
    log_event(app, event);
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
    let own_exe = std::env::current_exe().ok().map(|p| p.to_string_lossy().to_string());
    app.state::<MeetingState>()
        .record
        .lock()
        .unwrap()
        .export(&app.package_info().version.to_string(), own_exe.as_deref())
}

#[cfg(test)]
mod tests {
    use kiroscribe_core::meetings::{MicUser, WindowInfo};

    use super::*;

    fn observation() -> Observation {
        Observation {
            mic_users: vec![MicUser {
                id: r"C:\Users\山田 太郎\AppData\Local\Google\Chrome\Application\chrome.exe".into(),
                in_use: true,
            }],
            windows: vec![WindowInfo {
                exe: "chrome.exe".into(),
                title: "受信トレイ - yamada@example.com - Gmail - Google Chrome".into(),
            }],
        }
    }

    fn store() -> MicStore {
        MicStore {
            opened: true,
            entries: vec![kiroscribe_core::mic_store::MicEntry {
                id: r"C:\Users\山田 太郎\AppData\Local\slack\app-4.53.292\slack.exe".into(),
                last_start: 134_402_000_000_000_000,
                last_stop: Some(134_402_100_000_000_000),
            }],
        }
    }

    /// ログに書く判定の様子: マイクを使っているアプリの名前、見たウィンドウの数、判定と理由、段階、
    /// マイクの使用状況の記録（開けたか、会議アプリに見える記録の Stop の値と経過秒、名前の分類、置き場所の区分）、
    /// 開始の確定を待ち始めてからのミリ秒。ウィンドウのタイトルは、「詳しい記録」のときだけ入れる。
    /// 利用者の名前を含むフルパスは入れない。
    // [LOG-005][LOG-010]
    #[test]
    fn check_fields_tell_why_and_include_titles_only_when_verbose() {
        let observation = observation();
        let store = store();
        let tracker = MeetingTracker::default();
        let check = |verbose| Check {
            observation: &observation,
            store: Some(&store),
            own_exe: None,
            tracker: &tracker,
            verbose,
        };
        let normal = check_fields(&check(false), Some(kiroscribe_core::mic_store::filetime_to_unix_ms(134_402_160_000_000_000)));
        assert_eq!(normal["micInUse"], json!(["chrome.exe"]));
        assert_eq!(normal["browserWindows"], 1);
        assert_eq!(normal["verdict"], "noMeetingTab");
        assert_eq!(normal["why"], "ブラウザがマイクを使っているが、そのブラウザに会議のタブが見えない");
        assert_eq!(normal["app"], Value::Null);
        assert_eq!(normal["phase"], "idle");
        assert_eq!(normal["micStore"]["opened"], true);
        let slack = &normal["micStore"]["entries"][0];
        assert_eq!(slack["name"], "slack.exe");
        assert_eq!((&slack["nameKind"], &slack["location"]), (&json!("exe"), &json!("AppDataLocal")));
        assert_eq!(slack["stop"], "134402100000000000");
        assert_eq!(slack["sinceStopSec"], 6_000);
        assert_eq!(slack["inUse"], false);
        let text = normal.to_string();
        assert!(!text.contains("山田") && !text.contains("Gmail"), "{text}");

        let verbose = check_fields(&check(true), None);
        assert_eq!(
            verbose["browserTitles"],
            json!(["chrome.exe: 受信トレイ - yamada@example.com - Gmail - Google Chrome"])
        );
        assert!(verbose["micStore"]["entries"][0].get("sinceStopSec").is_none());
    }

    /// 開始の確定を待っている間は、待ち始めてからのミリ秒を書く。
    // [LOG-005]
    #[test]
    fn confirming_time_is_logged() {
        let meeting = Observation {
            mic_users: vec![MicUser { id: r"C:\x\slack.exe".into(), in_use: true }],
            windows: vec![],
        };
        let mut tracker = MeetingTracker::default();
        tracker.observe(&meeting, None, 10_000);
        let check = Check { observation: &meeting, store: None, own_exe: None, tracker: &tracker, verbose: false };
        let fields = check_fields(&check, Some(12_000));
        assert_eq!((&fields["phase"], &fields["confirmingMs"]), (&json!("confirming"), &json!(2_000)));
        assert_eq!(fields["micStore"], Value::Null, "Windows 以外では、記録はない");
    }

    /// 判定の様子は、変わったときと、変わらなくても 60 秒（詳しい記録のときは 15 秒）ごとに書く。
    /// 調べるたびに変わる値（経過秒）だけが変わっても、書き直さない。
    // [LOG-005][LOG-010]
    #[test]
    fn check_log_writes_on_change_and_at_a_heartbeat() {
        let observation = observation();
        let store = store();
        let tracker = MeetingTracker::default();
        let check = |verbose| Check { observation: &observation, store: Some(&store), own_exe: None, tracker: &tracker, verbose };
        let mut log = CheckLog::default();
        let first = log.next(&check(false), 1_000_000).expect("最初は書く");
        assert_eq!(first["changed"], true);
        assert!(log.next(&check(false), 1_002_000).is_none(), "様子が同じなら書かない");
        assert!(log.next(&check(false), 1_059_999).is_none());
        let beat = log.next(&check(false), 1_060_000).expect("60 秒たったら書く");
        assert_eq!(beat["changed"], false);

        let changed = Observation::default();
        let check2 = Check { observation: &changed, store: Some(&store), own_exe: None, tracker: &tracker, verbose: false };
        assert_eq!(log.next(&check2, 1_062_000).unwrap()["changed"], true, "変わったらすぐ書く");

        let mut verbose_log = CheckLog::default();
        verbose_log.next(&check(true), 0);
        assert!(verbose_log.next(&check(true), 14_999).is_none());
        assert!(verbose_log.next(&check(true), 15_000).is_some(), "詳しい記録のときは 15 秒ごと");
    }

    /// 会議アプリに見える記録やブラウザのウィンドウが多く、値の数の上限を超えても、ログのファイルに書いた行には
    /// 判定・理由・段階と、記録を開けたか・記録の件数が残る。
    // [LOG-005][LOG-009]
    #[test]
    fn a_check_with_many_entries_keeps_the_verdict_in_the_log_file() {
        use kiroscribe_core::debug_log::{DebugLog, Level, Source};
        use kiroscribe_core::mic_store::MicEntry;

        let names = ["slack.exe", "ms-teams.exe", "zoom.exe", "discord.exe", "webexmta.exe", "chrome.exe", "msedge.exe"];
        let mut entries: Vec<MicEntry> = (0..12)
            .map(|i| MicEntry {
                id: format!(r"C:\Users\山田 太郎\AppData\Local\app{i}\{}", names[i % names.len()]),
                last_start: 134_402_000_000_000_000 + i as u64,
                last_stop: Some(134_402_100_000_000_000),
            })
            .collect();
        // 会議アプリに見えない記録も、件数には入る
        entries.push(MicEntry { id: r"C:\Program Files\Recorder\rec.exe".into(), last_start: 1, last_stop: Some(2) });
        let store = MicStore { opened: true, entries };
        let observation = Observation {
            mic_users: vec![MicUser { id: r"C:\x\chrome.exe".into(), in_use: true }],
            windows: (0..12)
                .map(|i| WindowInfo { exe: "chrome.exe".into(), title: format!("タブ {i} - Google Chrome") })
                .collect(),
        };
        let tracker = MeetingTracker::default();
        let dir = tempfile::tempdir().unwrap();
        let log = DebugLog::open(dir.path()).unwrap();
        for verbose in [false, true] {
            let check = Check { observation: &observation, store: Some(&store), own_exe: None, tracker: &tracker, verbose };
            let fields = check_fields(&check, Some(kiroscribe_core::mic_store::filetime_to_unix_ms(134_402_160_000_000_000)));
            assert!(fields["micStore"]["entries"].as_array().unwrap().len() >= 10, "{fields}");
            log.write(Level::Info, Source::App, "meeting.check", fields);
        }

        let text = std::fs::read_to_string(&log.files()[0]).unwrap();
        let lines: Vec<Value> = text.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            let fields = &line["fields"];
            assert_eq!(fields["verdict"], "noMeetingTab", "{line}");
            assert_eq!(fields["why"], "ブラウザがマイクを使っているが、そのブラウザに会議のタブが見えない");
            assert_eq!(fields["phase"], "idle");
            assert_eq!(fields["micStore"]["opened"], true);
            assert_eq!(fields["micStore"]["total"], 13);
        }
        assert!(!text.contains("山田"), "{text}");
    }
}
