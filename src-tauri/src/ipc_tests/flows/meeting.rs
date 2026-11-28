//! 会議の自動検出: 確定した開始・終了を受けたときの、ノートの作成・録音・ポップアップ。
//! 検出そのもの（判定表の当てはめと、開始・終了の確定）は kiroscribe-core のテストにある。

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

/// 自動で始めた録音が API キーの誤りで止まったら、ポップアップでも理由を知らせる（ウィンドウを閉じていても分かるように）。
// [TRN-014][POP-006]
#[test]
fn a_rejected_api_key_during_a_meeting_is_reported_in_the_popup() {
    let h = Harness::new();
    *h.fixture.api.transcription_status.lock().unwrap() = Some(401);
    h.meeting(MeetingEvent::Started(MeetingApp::GoogleMeet));
    assert_eq!(h.popup()["kind"], "recording");
    let popup = wait_until("ポップアップでの知らせ", || Some(h.popup()).filter(|popup| popup["kind"] == "error"));
    assert_eq!(popup["app"], "Google Meet");
    let message = popup["message"].as_str().unwrap();
    assert!(message.starts_with("文字起こしを停止しました。API キーを確認してください（"), "{message}");
    wait_until("録音の停止", || (h.recording_state() == "idle").then_some(()));
    assert_eq!(h.events("recording-error").len(), 1);
    // 会議が終わっても、止まった録音については何も知らせない
    h.clear_events();
    h.meeting(MeetingEvent::Ended(MeetingApp::GoogleMeet));
    assert!(h.events("meeting-popup").is_empty());
}

/// 自動で始めた録音をメインウィンドウの「停止」で止めたら、ポップアップも停止の表示に変える。
// [POP-011]
#[test]
fn stopping_from_the_main_window_turns_the_popup_to_stopped() {
    let h = Harness::new();
    h.meeting(MeetingEvent::Started(MeetingApp::Zoom));
    let recording = h.popup();
    assert_eq!(recording["kind"], "recording");
    let note_id = recording["noteId"].as_str().unwrap().to_string();
    // ノートのタイトルを直してから止める（停止の表示には、そのときのタイトルを出す）
    h.ok("update_note", json!({ "id": note_id, "patch": { "title": "顧客との打ち合わせ" } }));

    // メインウィンドウの「停止」と同じコマンド
    assert_eq!(h.ok("stop_recording", json!({}))["state"], "idle");
    let stopped = json!({ "kind": "stopped", "app": "Zoom", "noteId": note_id, "title": "顧客との打ち合わせ" });
    assert_eq!(h.popup(), stopped, "ポップアップが録音中の表示のまま残っている");
    assert_eq!(popup_kinds(&h), [json!("recording"), json!("stopped")]);

    // そのあと別のノートを手動で録音して止めても、ポップアップは変わらない
    let manual = h.note("手動の録音", "");
    h.ok("start_recording", json!({ "noteId": manual }));
    h.ok("stop_recording", json!({}));
    assert_eq!(h.popup(), stopped);
    assert_eq!(popup_kinds(&h).len(), 2);
    // 会議が終わっても、もう何も知らせない
    h.meeting(MeetingEvent::Ended(MeetingApp::Zoom));
    assert_eq!(popup_kinds(&h).len(), 2);
}

/// ノートを作ったあとで録音を始められなかったら、理由を知らせて、作った空のノートは消す。
// [MTG-015]
#[test]
fn the_empty_note_is_removed_when_recording_cannot_start() {
    let h = Harness::new();
    let reason = "マイクが見つかりません。接続と Windows のプライバシー設定（マイクへのアクセス）を確認してください";
    std::fs::write(h.fixture.file("mic-error.txt"), reason).unwrap();
    h.meeting(MeetingEvent::Started(MeetingApp::Zoom));

    let popup = h.popup();
    assert_eq!((&popup["kind"], &popup["app"]), (&json!("error"), &json!("Zoom")));
    assert!(popup["message"].as_str().unwrap().contains(reason), "{popup}");
    assert!(note_ids(&h).is_empty(), "録音できなかった会議の、空のノートが残っている");
    assert!(h.events("note-created").is_empty(), "消すノートを、画面の一覧に加えている");
    assert_eq!(h.recording_state(), "idle");

    // マイクが使えるようになれば、次の会議は録音できる
    std::fs::remove_file(h.fixture.file("mic-error.txt")).unwrap();
    h.meeting(MeetingEvent::Started(MeetingApp::Zoom));
    assert_eq!(h.popup()["kind"], "recording");
    assert_eq!(note_ids(&h).len(), 1);
    assert_eq!(h.events("note-created").len(), 1);
}

/// 自動検出をオンにした時点で会議中なら、その会議の録音を始める。利用者が録音を止めたあとは、勝手に始め直さない。
// [MTG-019]
#[test]
fn turning_automatic_recording_on_during_a_meeting_starts_recording() {
    let h = Harness::new();
    h.save_settings(|settings| settings.auto_record_meetings = false);
    h.meeting(MeetingEvent::Started(MeetingApp::GoogleMeet));
    assert_eq!(h.recording_state(), "idle");

    // 会議の途中でオンにする → その会議の録音が始まる
    h.save_settings(|settings| settings.auto_record_meetings = true);
    wait_until("会議の録音の開始", || (h.recording_state() == "recording").then_some(()));
    let popup = wait_until("ポップアップでの知らせ", || Some(h.popup()).filter(|popup| popup["kind"] == "recording"));
    assert_eq!(popup["app"], "Google Meet");
    assert_eq!(note_ids(&h).len(), 1);
    assert!(h.events("note-created")[0]["title"].as_str().unwrap().starts_with("Google Meet "));

    // 自動で始めた録音を利用者が止めたあとは、同じ会議が続いていても、設定を保存し直しても、始め直さない
    h.ok("stop_recording", json!({}));
    h.save_settings(|settings| settings.my_name = "田中".into());
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(h.recording_state(), "idle", "止めた録音を、勝手に始め直している");
    assert_eq!(note_ids(&h).len(), 1);

    // 手動の録音を止めたあとも、続いている会議を勝手に録音し始めない
    let manual = h.note("手動の録音", "");
    h.ok("start_recording", json!({ "noteId": manual }));
    h.ok("stop_recording", json!({}));
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(h.recording_state(), "idle", "手動の録音を止めた直後に、会議の録音を始めている");
    assert_eq!(note_ids(&h).len(), 2);

    // オフにしてオンにし直すと（利用者が、いまの会議を録音したいと示した）、録音を始める
    h.save_settings(|settings| settings.auto_record_meetings = false);
    h.save_settings(|settings| settings.auto_record_meetings = true);
    wait_until("会議の録音の開始", || (h.recording_state() == "recording").then_some(()));
    assert_eq!(note_ids(&h).len(), 3);
    h.ok("stop_recording", json!({}));

    // 会議が終わったあとでオンにしても、何も始めない
    h.meeting(MeetingEvent::Ended(MeetingApp::GoogleMeet));
    h.save_settings(|settings| settings.auto_record_meetings = false);
    h.save_settings(|settings| settings.auto_record_meetings = true);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(h.recording_state(), "idle");
    assert_eq!(note_ids(&h).len(), 3);
}

/// 会議が終わってすぐ別のアプリの会議が始まったら、前の録音を止めて、新しいノートで録音を始める。
/// 前の録音の仕上げ（残りの文字起こし）に時間がかかっていても、次の会議の録音を取りこぼさない。
// [MTG-010]
#[test]
fn a_meeting_in_another_app_is_recorded_into_a_new_note() {
    let h = Harness::new();
    h.meeting(MeetingEvent::Started(MeetingApp::GoogleMeet));
    let first = note_ids(&h)[0].clone();
    wait_until("最初の会議の文字起こし", || (!h.segments(&first).is_empty()).then_some(()));

    // 会議の検出は、前の会議の終了と、次の会議の開始を、続けて知らせる。前の録音の仕上げには時間がかかる
    *h.fixture.api.transcription_delay.lock().unwrap() = Duration::from_millis(1200);
    h.submit_meeting(MeetingEvent::Ended(MeetingApp::GoogleMeet));
    h.submit_meeting(MeetingEvent::Started(MeetingApp::Zoom));

    let second = wait_until("次の会議のノート", || note_ids(&h).into_iter().find(|id| *id != first));
    wait_until("次の会議の録音", || {
        let status = h.ok("get_recording_status", json!({}));
        (status["state"] == "recording" && status["noteId"] == second.as_str()).then_some(())
    });
    assert_eq!(note_ids(&h), [second.clone(), first.clone()], "会議ごとに、別のノートに分ける");
    let titles: Vec<String> = h
        .events("note-created")
        .iter()
        .map(|note| note["title"].as_str().unwrap().split(' ').next().unwrap().to_string())
        .collect();
    assert_eq!(titles, ["Google", "Zoom"]);
    // 前の録音は、仕上げまで終わっている（順に処理している）
    assert_eq!(
        popup_kinds(&h),
        [json!("recording"), json!("finishing"), json!("stopped"), json!("recording")]
    );
    assert_eq!(h.popup()["app"], "Zoom");
    *h.fixture.api.transcription_delay.lock().unwrap() = Duration::ZERO;

    // 新しい会議の発話は、新しいノートにだけ入る
    let known = h.segments(&first).len();
    wait_until("次の会議の文字起こし", || (!h.segments(&second).is_empty()).then_some(()));
    assert_eq!(h.segments(&first).len(), known, "次の会議の発話が、前の会議のノートに入っている");
    h.meeting(MeetingEvent::Ended(MeetingApp::Zoom));
    assert_eq!(h.recording_state(), "idle");
}

/// 設定で足したブラウザは、保存した時から会議の検出の対象になり、変わったことをログに書く。
// [SET-021][MTG-002][LOG-005]
#[test]
fn browsers_added_in_settings_are_used_by_the_detection_right_away() {
    use tauri::Manager;
    let h = Harness::new();
    let state = || h.app.state::<crate::meeting_watch::MeetingState>().browsers();
    assert!(state().extra().is_empty(), "最初は組み込みの一覧だけ");

    let saved = h.save_settings(|settings| settings.extra_browsers = vec![" NewBrowser ".into(), "kiroscribe.exe".into()]);
    assert_eq!(saved["extraBrowsers"], json!(["newbrowser.exe"]), "実行ファイルの名前にそろえて保存する");
    assert!(state().contains("newbrowser.exe") && state().contains("chrome.exe"));
    let logged = h.debug_log_events("meeting.browsers_changed");
    assert_eq!(logged.len(), 1);
    assert_eq!(logged[0]["fields"]["extraBrowsers"], json!(["newbrowser.exe"]));

    // ほかの設定だけを変えたときは、書かない
    h.save_settings(|settings| settings.my_name = "田中".into());
    assert_eq!(h.debug_log_events("meeting.browsers_changed").len(), 1);

    h.save_settings(|settings| settings.extra_browsers.clear());
    assert!(!state().contains("newbrowser.exe"));
    assert_eq!(h.debug_log_events("meeting.browsers_changed").len(), 2);
}
