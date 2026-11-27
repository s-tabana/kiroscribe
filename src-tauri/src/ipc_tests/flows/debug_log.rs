//! デバッグ用のログ: 録音・文字起こし・AI・会議の検出・更新の出来事を書くこと、秘密や本文を書かないこと、書き出しの中身。

use std::io::Read;

use kiroscribe_core::meetings::{MeetingApp, MeetingEvent};
use serde_json::{json, Value};

use super::super::fixture::{wait_until, Harness, ANTHROPIC_KEY, OPENAI_KEY, TRANSCRIPT};

const USER_NOTES: &str = "- 社外秘: 新製品の値段は 9800 円";
const QUESTION: &str = "決まったことを教えてください（質問の本文）";

/// 録音の開始・一時停止・再開・停止、文字起こしと AI の処理の成否と時間を書く。
/// API キー、メモ・文字起こし・議事録・チャットの本文は書かない。
// [LOG-006][LOG-007][LOG-009][SEC-002]
#[test]
fn recording_transcription_and_ai_are_logged_without_secrets_or_text() {
    let h = Harness::new();
    let note = h.note("週次定例", USER_NOTES);
    h.record_until_transcribed(&note);
    h.ok("pause_recording", json!({}));
    h.ok("resume_recording", json!({}));
    h.ok("stop_recording", json!({}));
    h.ok("enhance_note", json!({ "noteId": note }));
    h.ok("send_chat_message", json!({ "noteId": note, "content": QUESTION }));
    // 失敗した AI の処理は、理由と一緒に書く
    *h.fixture.api.claude.lock().unwrap() = super::super::fixture::ClaudeReply::status(500);
    assert!(h.call("send_chat_message", json!({ "noteId": note, "content": QUESTION })).is_err());

    let started = h.debug_log_events("recording.started");
    assert_eq!(started.len(), 1);
    assert_eq!(started[0]["fields"]["noteId"], note.as_str());
    assert_eq!(started[0]["fields"]["auto"], Value::Null);
    assert_eq!(started[0]["fields"]["transcriptionModel"], h.settings().transcription_model.as_str());
    assert_eq!(started[0]["level"], "info");
    assert_eq!(started[0]["source"], "app");
    assert_eq!(h.debug_log_events("recording.paused").len(), 1);
    assert_eq!(h.debug_log_events("recording.resumed").len(), 1);
    assert_eq!(h.debug_log_events("recording.stopping").len(), 1);
    let stopped = h.debug_log_events("recording.stopped");
    assert_eq!(stopped.len(), 1);
    let fields = &stopped[0]["fields"];
    assert!(fields["transcribed"].as_i64().unwrap() >= 1, "{fields}");
    assert_eq!(fields["transcriptionFailed"], 0);
    assert!(fields["transcriptionTotalMs"].as_i64().is_some() && fields["finishMs"].as_i64().is_some());
    // 文字起こしの 1 回ごとの結果は、「詳しい記録」のときだけ
    assert!(h.debug_log_events("transcription.finished").is_empty());

    let ai_started: Vec<String> = h
        .debug_log_events("ai.started")
        .iter()
        .map(|line| line["fields"]["kind"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ai_started, vec!["enhance", "chat", "chat"]);
    let finished = h.debug_log_events("ai.finished");
    assert_eq!(finished.len(), 2);
    assert!(finished.iter().all(|line| line["fields"]["ms"].as_i64().is_some()));
    let failed = h.debug_log_events("ai.failed");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["level"], "error");
    assert_eq!(failed[0]["fields"]["kind"], "chat");
    assert!(!failed[0]["fields"]["reason"].as_str().unwrap().is_empty());

    let text = h.debug_log_text();
    for secret in [OPENAI_KEY, ANTHROPIC_KEY, TRANSCRIPT, USER_NOTES, QUESTION, "リリースは 12/5", "社外秘", "9800"] {
        assert!(!text.contains(secret), "ログに「{secret}」が入っている:\n{text}");
    }
}

/// 「詳しい記録」がオンの間は、文字起こしの 1 回ごとの成否と時間も書く（文字起こしの本文は書かない）。
// [LOG-010][LOG-007][LOG-009]
#[test]
fn verbose_logging_adds_each_transcription() {
    let h = Harness::new();
    h.ok("set_debug_verbose", json!({ "enabled": true }));
    assert_eq!(h.debug_log_events("log.verbose.started").len(), 1);
    let note = h.note("", "");
    h.record_until_transcribed(&note);
    h.ok("stop_recording", json!({}));
    let each = h.debug_log_events("transcription.finished");
    assert!(!each.is_empty());
    assert_eq!(each[0]["level"], "detail");
    assert!(each[0]["fields"]["ms"].as_i64().is_some() && each[0]["fields"]["chars"].as_i64().is_some());
    assert!(!h.debug_log_text().contains(TRANSCRIPT));
    assert_eq!(h.settings_value("debugVerboseUntil").is_some(), true, "期限は、起動し直しても残るよう保存する");

    h.ok("set_debug_verbose", json!({ "enabled": false }));
    assert_eq!(h.debug_log_events("log.verbose.stopped").len(), 1);
    assert_eq!(h.settings_value("debugVerboseUntil"), None);
}

/// 文字起こしに失敗したら、理由と時間を書く（キーは書かない）。
// [LOG-007][LOG-009]
#[test]
fn a_failed_transcription_is_logged_with_the_reason() {
    let h = Harness::new();
    *h.fixture.api.transcription_status.lock().unwrap() = Some(500);
    let note = h.note("", "");
    h.ok("start_recording", json!({ "noteId": note }));
    let failed = wait_until("文字起こしの失敗のログ", || h.debug_log_events("transcription.failed").into_iter().next());
    assert_eq!(failed["level"], "error");
    assert!(failed["fields"]["reason"].as_str().unwrap().contains("500"), "{failed}");
    h.ok("stop_recording", json!({}));
    assert!(!h.debug_log_text().contains(OPENAI_KEY));
}

/// 会議の開始・終了と、自動で録音を始めなかった理由を書く。
// [LOG-005]
#[test]
fn meeting_events_and_why_recording_did_not_start_are_logged() {
    let h = Harness::without_keys();
    h.meeting(MeetingEvent::Started(MeetingApp::SlackHuddle));
    h.meeting(MeetingEvent::Ended(MeetingApp::SlackHuddle));
    h.save_settings(|s| s.auto_record_meetings = false);
    h.meeting(MeetingEvent::Started(MeetingApp::GoogleMeet));
    h.meeting(MeetingEvent::Ended(MeetingApp::GoogleMeet));

    let started: Vec<Value> = h.debug_log_events("meeting.started").iter().map(|l| l["fields"]["app"].clone()).collect();
    assert_eq!(started, vec![json!("slackHuddle"), json!("googleMeet")]);
    assert_eq!(h.debug_log_events("meeting.ended").len(), 2);
    let skipped: Vec<(Value, Value)> = h
        .debug_log_events("meeting.auto_record.skipped")
        .iter()
        .map(|l| (l["fields"]["app"].clone(), l["fields"]["reason"].clone()))
        .collect();
    assert_eq!(
        skipped,
        vec![(json!("slackHuddle"), json!("noOpenAiKey")), (json!("googleMeet"), json!("autoRecordOff"))]
    );
    let stop_skipped = h.debug_log_events("meeting.auto_stop.skipped");
    assert_eq!(stop_skipped[0]["fields"]["reason"], "noAutoRecording");
}

/// 更新の確認の結果（見つけた版、または失敗の理由）を書く。
// [LOG-004]
#[test]
fn update_checks_are_logged() {
    let h = Harness::new();
    h.ok("check_for_update", json!({}));
    let checked = h.debug_log_events("update.checked");
    assert_eq!(checked.len(), 1);
    assert_eq!(checked[0]["fields"]["available"], "99.0.0");
    assert!(checked[0]["fields"]["current"].as_str().is_some());
    drop(h);

    let h = Harness::with_endpoint("/update-none/latest.json");
    assert!(h.call("check_for_update", json!({})).is_err());
    let failed = h.debug_log_events("update.check_failed");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["level"], "error");
    assert!(!failed[0]["fields"]["reason"].as_str().unwrap().is_empty());
}

/// 画面の出来事は、画面のものと分かる名前（ui.）で書く。値の中の API キーは伏せる。
// [LOG-008][LOG-009]
#[test]
fn ui_events_are_logged_with_their_source() {
    let h = Harness::new();
    h.ok(
        "write_debug_log",
        json!({ "level": "error", "event": "command_failed", "fields": { "command": "set_api_key", "message": "bad sk-live-ABCDEF123456" } }),
    );
    h.ok("write_debug_log", json!({ "level": "info", "event": "ui.loaded", "fields": { "view": "main" } }));
    let failed = h.debug_log_events("ui.command_failed");
    assert_eq!(failed.len(), 1);
    assert_eq!((&failed[0]["level"], &failed[0]["source"]), (&json!("error"), &json!("ui")));
    assert_eq!(failed[0]["fields"]["command"], "set_api_key");
    assert!(!h.debug_log_text().contains("ABCDEF123456"));
    assert_eq!(h.debug_log_events("ui.loaded")[0]["source"], "ui");
}

/// 書き出した zip には、ログ・会議の検出の記録・環境の情報が入り、API キーと本文は入らない。
// [LOG-011][MTG-026][SEC-002][LOG-009]
#[test]
fn the_saved_bundle_has_logs_record_and_environment_but_no_secrets() {
    let h = Harness::new();
    let note = h.note("週次定例", USER_NOTES);
    h.record_until_transcribed(&note);
    h.ok("stop_recording", json!({}));
    h.save_settings(|s| s.my_name = "山田".into());

    let out = h.fixture.dir.join("debug-bundle.zip");
    let _ = std::fs::remove_file(&out);
    let files = h.ok("save_debug_bundle", json!({ "path": out }));
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
    assert_eq!(files.as_u64().unwrap() as usize, zip.len());
    let names: Vec<String> = (0..zip.len()).map(|i| zip.by_index(i).unwrap().name().unwrap().to_string()).collect();
    for name in ["README.txt", "environment.json", "meeting-detection.json"] {
        assert!(names.contains(&name.to_string()), "{names:?}");
    }
    assert!(names.iter().any(|n| n.starts_with("logs/kiroscribe-") && n.ends_with(".log")), "{names:?}");

    let mut all = String::new();
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).unwrap();
        file.read_to_string(&mut all).unwrap();
    }
    for secret in [OPENAI_KEY, ANTHROPIC_KEY, TRANSCRIPT, USER_NOTES, "山田"] {
        assert!(!all.contains(secret), "zip に「{secret}」が入っている");
    }
    let mut env = String::new();
    zip.by_name("environment.json").unwrap().read_to_string(&mut env).unwrap();
    let env: Value = serde_json::from_str(&env).unwrap();
    assert_eq!(env["appVersion"], h.ok("get_app_version", json!({})));
    assert_eq!(env["settings"]["openaiKeyConfigured"], true);
    assert_eq!(env["settings"]["myNameSet"], true);
    assert!(all.contains("recording.started"), "ログのファイルが入っていない");
    // 書き出しを始めたことも書く
    assert_eq!(h.debug_log_events("log.bundle.saving").len(), 1);
}

/// 起動（アプリと OS のバージョン）と終了を書く。
// [LOG-003]
#[test]
fn start_and_exit_are_logged() {
    let h = Harness::new();
    crate::log_started(h.app.handle(), true);
    crate::on_run_event(h.app.handle(), &tauri::RunEvent::Exit);
    let started = h.debug_log_events("app.started");
    assert_eq!(started.len(), 1);
    assert_eq!(started[0]["fields"]["appVersion"], h.ok("get_app_version", json!({})));
    assert!(!started[0]["fields"]["os"].as_str().unwrap().is_empty());
    // Windows のビルド番号（Windows 以外では null）
    assert_eq!(started[0]["fields"]["osBuild"].is_string(), cfg!(windows), "{}", started[0]);
    assert_eq!(started[0]["fields"]["autostart"], true);
    assert_eq!(h.debug_log_events("app.exit").len(), 1);
}
