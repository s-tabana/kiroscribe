//! 長い録音で、会議が続いているかを確かめる: 確認のポップアップ、「止める」「続ける」、答えがないときの停止と知らせ、
//! トレイの「録音を停止」。判定の時間の決まりは kiroscribe-core（recording_check）のテストにある。

use kiroscribe_core::meetings::{MeetingApp, MeetingEvent};
use kiroscribe_core::time::now_ms;
use serde_json::{json, Value};
use tauri::Manager;

use super::super::fixture::{wait_until, Harness};
use crate::background::BackgroundState;
use crate::recording::RecorderState;

const MIN: i64 = 60_000;

impl Harness {
    /// 確かめる処理を 1 回進める（`now` は、その回の時刻）。
    fn check_at(&self, now: i64) {
        let handle = self.app.handle().clone();
        tauri::async_runtime::block_on(async move { crate::recording_check::tick(&handle, now).await });
    }

    /// 録音を `minutes` 分前に始めたことにする（話した時刻は、いまのまま）。
    fn backdate(&self, minutes: i64) {
        let handle = self.app.handle().clone();
        tauri::async_runtime::block_on(async move { handle.state::<RecorderState>().backdate(minutes * MIN).await });
    }

    fn popup_kinds(&self) -> Vec<Value> {
        self.events("meeting-popup").iter().map(|popup| popup["kind"].clone()).collect()
    }

    fn notices(&self) -> Vec<String> {
        self.app.state::<BackgroundState>().notices()
    }
}

/// 手動の録音でも、1 時間を超えて両方が話していなければ「会議は終わりましたか？」と聞く。
/// 「続ける」なら 30 分は聞かず、「止める」なら止めて保存する。
// [REC-023][REC-024][REC-025][POP-014]
#[test]
fn a_long_silent_recording_asks_whether_the_meeting_has_ended() {
    let h = Harness::new();
    let note = h.note("対面の打ち合わせ", "");
    h.ok("start_recording", json!({ "noteId": note }));
    let start = now_ms();

    // 1 時間たつまでは聞かない
    h.check_at(start + 59 * MIN);
    assert_eq!(h.popup(), Value::Null);

    h.check_at(start + 61 * MIN);
    let asked = json!({
        "kind": "confirmEnd",
        "app": "Kiroscribe",
        "noteId": note,
        "title": "対面の打ち合わせ",
        "deadline": start + 63 * MIN,
    });
    assert_eq!(h.popup(), asked);
    assert_eq!(h.recording_state(), "recording", "聞いただけで止めている");

    // 「続ける」: 手動の録音なので、ポップアップは閉じる。30 分たつまで、もう聞かない
    assert_eq!(h.ok("answer_recording_check", json!({ "noteId": note, "keep": true })), Value::Null);
    assert_eq!(h.popup(), Value::Null);
    assert_eq!(h.recording_state(), "recording");
    let kept_at = now_ms();
    h.check_at(kept_at + 29 * MIN);
    assert_eq!(h.popup(), Value::Null, "「続ける」のあと 30 分たたないうちに聞いている");
    h.check_at(kept_at + 31 * MIN);
    assert_eq!(h.popup()["kind"], "confirmEnd");

    // 「止める」: 止めて保存し、停止の表示にする
    assert_eq!(h.ok("answer_recording_check", json!({ "noteId": note, "keep": false })), Value::Null);
    assert_eq!(h.recording_state(), "idle");
    assert_eq!(
        h.popup(),
        json!({ "kind": "stopped", "app": "Kiroscribe", "noteId": note, "title": "対面の打ち合わせ" })
    );
    assert!(h.notices().is_empty(), "利用者が止めたのに、通知を出している");
}

/// 確認に 2 分答えがなければ、録音を止めて保存し、ポップアップと Windows の通知で知らせる。
// [REC-026][POP-015]
#[test]
fn an_unanswered_question_stops_and_saves_the_recording() {
    let h = Harness::new();
    let note = h.note("週次の定例", "");
    h.ok("start_recording", json!({ "noteId": note }));
    wait_until("文字起こし", || (!h.segments(&note).is_empty()).then_some(()));
    let start = now_ms();

    h.check_at(start + 61 * MIN);
    assert_eq!(h.popup()["kind"], "confirmEnd");
    // 答えを待っている間に、タイトルを直した（知らせには、止めたときのタイトルを出す）
    h.ok("update_note", json!({ "id": note, "patch": { "title": "週次の定例（11 月）" } }));
    h.check_at(start + 62 * MIN);
    assert_eq!(h.recording_state(), "recording", "2 分たたないうちに止めている");

    h.check_at(start + 63 * MIN);
    assert_eq!(h.recording_state(), "idle");
    let message = "答えがなかったため、録音を停止しました。「週次の定例（11 月）」に保存しています。";
    assert_eq!(h.notices(), [message]);
    assert_eq!(
        h.popup(),
        json!({ "kind": "stoppedUnanswered", "app": "Kiroscribe", "noteId": note, "title": "週次の定例（11 月）" })
    );
    assert_eq!(h.popup_kinds(), [json!("confirmEnd"), json!("finishing"), json!("stoppedUnanswered")]);
    // 止めるまでの発話は保存されている
    assert!(!h.segments(&note).is_empty());

    // もう録音していないので、そのあとは何もしない
    h.check_at(start + 120 * MIN);
    assert_eq!(h.popup_kinds().len(), 3);
}

/// この録音の間に検出していた会議が消えていれば、話し声があっても聞く。会議が続いていて話し声があれば、聞かない。
// [REC-024][MTG-017]
#[test]
fn a_meeting_that_is_no_longer_detected_is_asked_about() {
    let h = Harness::new();
    let note = h.note("手動で録音した会議", "");
    h.ok("start_recording", json!({ "noteId": note }));
    // 手動で録音している間に、会議を検出した（手動の録音なので、会議の開始・終了では何もしない）
    h.meeting(MeetingEvent::Started(MeetingApp::GoogleMeet));
    h.check_at(now_ms());

    // 会議が続いていて、いま話している: 1 時間を超えても聞かない
    h.backdate(61);
    h.check_at(now_ms());
    assert_eq!(h.popup(), Value::Null);

    // 会議の終了を確定した（手動の録音は止めない）。5 分後に確かめたとき、話し声があっても聞く
    h.meeting(MeetingEvent::Ended(MeetingApp::GoogleMeet));
    assert_eq!(h.recording_state(), "recording");
    h.backdate(5);
    h.check_at(now_ms());
    assert_eq!(h.popup()["kind"], "confirmEnd");
    assert_eq!(h.popup()["noteId"], note.as_str());
}

/// 自動で始めた録音では、見出しに会議の名前を出す。「続ける」と答えたら、録音中の表示に戻す。
// [POP-014][REC-025]
#[test]
fn keeping_an_automatic_recording_returns_the_popup_to_recording() {
    let h = Harness::new();
    h.meeting(MeetingEvent::Started(MeetingApp::Zoom));
    let recording = h.popup();
    assert_eq!(recording["kind"], "recording");
    let note = recording["noteId"].as_str().unwrap().to_string();

    h.check_at(now_ms() + 61 * MIN);
    let asked = h.popup();
    assert_eq!((&asked["kind"], &asked["app"], &asked["noteId"]), (&json!("confirmEnd"), &json!("Zoom"), &json!(note)));

    h.ok("answer_recording_check", json!({ "noteId": note, "keep": true }));
    assert_eq!(h.popup(), recording);
    assert_eq!(h.recording_state(), "recording");
}

/// 確認を出している録音を、メインウィンドウやトレイで止めたら、停止の表示にする。
/// 答える前に録音が止まっていたら、「続ける」を押しても停止の表示にする。
// [POP-014][TRY-011]
#[test]
fn stopping_elsewhere_turns_the_question_to_stopped() {
    let h = Harness::new();
    let note = h.note("確認中に止める", "");
    h.ok("start_recording", json!({ "noteId": note }));
    h.check_at(now_ms() + 61 * MIN);
    assert_eq!(h.popup()["kind"], "confirmEnd");
    h.ok("stop_recording", json!({}));
    assert_eq!(h.popup()["kind"], "stopped");

    // トレイの「録音を停止」
    let second = h.note("トレイで止める", "");
    h.ok("start_recording", json!({ "noteId": second }));
    h.check_at(now_ms() + 61 * MIN);
    assert_eq!(h.popup()["noteId"], second.as_str());
    crate::background::on_tray_menu(h.app.handle(), "stop-recording");
    wait_until("トレイからの停止", || (h.recording_state() == "idle").then_some(()));
    wait_until("停止の表示", || (h.popup()["kind"] == "stopped").then_some(()));
    assert_eq!(h.popup()["noteId"], second.as_str());

    // 答える前に止まっていた録音
    h.ok("answer_recording_check", json!({ "noteId": second, "keep": true }));
    assert_eq!(h.popup()["kind"], "stopped");
    assert!(h.notices().is_empty());
}

/// トレイの「録音を停止」は、手動の録音も、自動で始めた録音も止める（自動の録音なら、ポップアップも停止の表示にする）。
// [TRY-011]
#[test]
fn the_tray_stops_any_recording() {
    let h = Harness::new();
    let note = h.note("手動の録音", "");
    h.ok("start_recording", json!({ "noteId": note }));
    crate::background::on_tray_menu(h.app.handle(), "stop-recording");
    wait_until("トレイからの停止", || (h.recording_state() == "idle").then_some(()));
    assert_eq!(h.popup(), Value::Null, "手動の録音なのに、ポップアップを出している");

    h.meeting(MeetingEvent::Started(MeetingApp::Teams));
    assert_eq!(h.recording_state(), "recording");
    crate::background::on_tray_menu(h.app.handle(), "stop-recording");
    wait_until("トレイからの停止", || (h.recording_state() == "idle").then_some(()));
    wait_until("停止の表示", || (h.popup()["kind"] == "stopped").then_some(()));

    // 録音していないときに選んでも、何も起きない
    crate::background::on_tray_menu(h.app.handle(), "stop-recording");
    assert_eq!(h.recording_state(), "idle");
}
