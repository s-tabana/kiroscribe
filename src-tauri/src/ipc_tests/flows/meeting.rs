//! 会議の自動検出: 確定した開始・終了を受けたときの、ノートの作成・録音・ポップアップ。
//! 検出そのもの（マイクの使用状況とウィンドウのタイトルからの判定、開始・終了の確定）は kiroscribe-core のテストにある。

use std::time::Duration;

use kiroscribe_core::meetings::{MeetingApp, MeetingEvent};
use serde_json::{json, Value};

use super::super::fixture::{wait_until, Harness};

fn popup_kinds(h: &Harness) -> Vec<Value> {
    h.events("meeting-popup").iter().map(|popup| popup["kind"].clone()).collect()
}

fn note_ids(h: &Harness) -> Vec<String> {
    h.ok("list_notes", json!({}))
        .as_array()
        .unwrap()
        .iter()
        .map(|note| note["id"].as_str().unwrap().to_string())
        .collect()
}

/// 会議が始まるとノートを作って録音し、終わると止める。
// [MTG-011][MTG-016]
#[test]
fn a_meeting_is_recorded_into_a_new_note_and_stopped_when_it_ends() {
    let h = Harness::new();
    h.meeting(MeetingEvent::Started(MeetingApp::Zoom));

    // ノート: 「<会議の名前> <月>/<日> <時>:<分>」。画面には note-created で知らせる（一覧にすぐ加わる）
    let created = h.events("note-created");
    assert_eq!(created.len(), 1);
    let title = created[0]["title"].as_str().unwrap().to_string();
    let (name, date) = title.split_once(' ').unwrap();
    assert_eq!(name, "Zoom");
    let (day, time) = date.split_once(' ').unwrap();
    assert!(day.split('/').count() == 2 && day.split('/').all(|part| part.parse::<u8>().is_ok()), "{title}");
    assert!(time.len() == 5 && time.split(':').all(|part| part.len() == 2 && part.parse::<u8>().is_ok()), "{title}");
    let note_id = created[0]["id"].as_str().unwrap().to_string();
    assert_eq!(note_ids(&h), std::slice::from_ref(&note_id));

    // 録音と文字起こしが始まり、ポップアップで知らせる
    let status = h.ok("get_recording_status", json!({}));
    assert_eq!((&status["state"], &status["noteId"]), (&json!("recording"), &json!(note_id)));
    assert_eq!(h.popup(), json!({ "kind": "recording", "app": "Zoom", "noteId": note_id, "title": title }));
    wait_until("会議の文字起こし", || (!h.segments(&note_id).is_empty()).then_some(()));

    // 別のアプリの会議の終了では止めない
    h.meeting(MeetingEvent::Ended(MeetingApp::Teams));
    assert_eq!(h.recording_state(), "recording");

    // 会議の終了: 「仕上げ中」を知らせて止め、残りの文字起こしが終わったら「停止」を知らせる
    h.meeting(MeetingEvent::Ended(MeetingApp::Zoom));
    assert_eq!(h.recording_state(), "idle");
    assert_eq!(popup_kinds(&h), [json!("recording"), json!("finishing"), json!("stopped")]);
    assert_eq!(h.popup(), json!({ "kind": "stopped", "app": "Zoom", "noteId": note_id, "title": title }));
    let states: Vec<Value> = h.events("recording-status").iter().map(|status| status["state"].clone()).collect();
    assert_eq!(states, [json!("recording"), json!("finishing"), json!("idle")]);
}

/// 自動検出がオフなら、会議を検出しても何もしない。会議の検出のために、外部とは通信しない。
// [MTG-012][MTG-018]
#[test]
fn nothing_happens_while_automatic_recording_is_off() {
    let h = Harness::new();
    h.save_settings(|settings| settings.auto_record_meetings = false);
    h.meeting(MeetingEvent::Started(MeetingApp::GoogleMeet));
    assert!(note_ids(&h).is_empty(), "ノートを作っている");
    assert_eq!(h.popup(), Value::Null);
    assert!(h.events("note-created").is_empty() && h.events("meeting-popup").is_empty());
    assert_eq!(h.recording_state(), "idle");
    h.meeting(MeetingEvent::Ended(MeetingApp::GoogleMeet));
    assert_eq!(h.popup(), Value::Null);
    assert_eq!(h.request_count(), 0, "会議の検出のために、外部と通信している");

    // 会議の途中でオフにしても、自動で始めて続いている録音は、会議の終了で止まる
    h.save_settings(|settings| settings.auto_record_meetings = true);
    h.meeting(MeetingEvent::Started(MeetingApp::Discord));
    assert_eq!(h.recording_state(), "recording");
    h.save_settings(|settings| settings.auto_record_meetings = false);
    assert_eq!(h.recording_state(), "recording", "オフにしただけでは止めない");
    h.meeting(MeetingEvent::Ended(MeetingApp::Discord));
    assert_eq!(h.recording_state(), "idle");
    assert_eq!(h.popup()["kind"], "stopped");
}

/// すでに録音しているときは、会議の開始を検出しても何もしない。手動の録音は、会議が終わっても止めない。
// [MTG-013][MTG-017]
#[test]
fn a_recording_in_progress_is_left_alone() {
    let h = Harness::new();
    let manual = h.note("手動の録音", "");
    h.ok("start_recording", json!({ "noteId": manual }));

    h.meeting(MeetingEvent::Started(MeetingApp::SlackHuddle));
    assert_eq!(note_ids(&h), std::slice::from_ref(&manual), "会議のノートを作っている");
    assert_eq!(h.popup(), Value::Null);
    assert_eq!(h.ok("get_recording_status", json!({}))["noteId"], manual.as_str());

    // 手動の録音は、会議が終わっても続く
    h.meeting(MeetingEvent::Ended(MeetingApp::SlackHuddle));
    assert_eq!(h.recording_state(), "recording");
    assert_eq!(h.popup(), Value::Null);

    // 停止後の仕上げ中に始まった会議も、録音しない
    *h.fixture.api.transcription_delay.lock().unwrap() = Duration::from_millis(1200);
    wait_until("文字起こし中の発話", || (!h.transcriptions().is_empty()).then_some(()));
    let stopping = h.spawn("stop_recording", json!({}));
    wait_until("仕上げ中の状態", || (h.recording_state() == "finishing").then_some(()));
    h.meeting(MeetingEvent::Started(MeetingApp::Teams));
    assert_eq!(note_ids(&h), std::slice::from_ref(&manual));
    assert_eq!(h.popup(), Value::Null);
    stopping.join().unwrap().unwrap();
    assert_eq!(h.recording_state(), "idle");
    *h.fixture.api.transcription_delay.lock().unwrap() = Duration::ZERO;

    // 自動で始めた録音を利用者が先に止めていたら、会議が終わっても何もしない
    h.meeting(MeetingEvent::Started(MeetingApp::Zoom));
    assert_eq!(h.recording_state(), "recording");
    h.ok("stop_recording", json!({}));
    let manual_again = h.note("会議のあとの手動の録音", "");
    h.ok("start_recording", json!({ "noteId": manual_again }));
    h.clear_events();
    h.meeting(MeetingEvent::Ended(MeetingApp::Zoom));
    assert_eq!(h.ok("get_recording_status", json!({}))["noteId"], manual_again.as_str(), "手動の録音を止めている");
    assert!(h.events("meeting-popup").is_empty(), "会議の終了を知らせている");
}

/// OpenAI の API キーがなければ、ノートを作らずに、ポップアップで知らせる。
// [MTG-014]
#[test]
fn a_meeting_without_an_openai_key_is_reported_instead_of_recorded() {
    let h = Harness::without_keys();
    h.meeting(MeetingEvent::Started(MeetingApp::Teams));
    assert_eq!(
        h.popup(),
        json!({
            "kind": "error",
            "app": "Teams",
            "message": "OpenAI の API キーが設定されていないため、自動で録音できませんでした",
        })
    );
    assert!(note_ids(&h).is_empty(), "録音できないのにノートを作っている");
    assert!(h.events("note-created").is_empty());
    assert_eq!(h.recording_state(), "idle");
}
