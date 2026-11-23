//! アプリ内更新のコマンドと、自動の確認・ダウンロード。
//! 模擬の配布元が配るファイルはインストーラではないので、署名が正しくても、インストールの段階で必ず止まる
//! （模擬のランタイムではメインウィンドウが常に「表示中」なので、自動のインストールには進まない）。

use std::time::Duration;

use kiroscribe_core::time::now_ms;
use serde_json::{json, Value};
use tauri::Manager;

use super::super::fixture::{wait_until, ClaudeReply, Harness};
use crate::state::AppState;

fn phases(h: &Harness) -> Vec<Value> {
    h.events("update-status").iter().map(|status| status["phase"].clone()).collect()
}

/// 自動の確認を始める（製品では、起動時に始まる）。
fn start_auto_check(h: &Harness) {
    crate::update::spawn_auto_check(h.app.handle().clone());
}

/// 手動の確認: 見つかれば状態に出し、確認できなければ理由を返す。
// [UPD-006][UPD-020]
#[test]
fn checking_by_hand_reports_the_result_or_the_reason() {
    let h = Harness::new();
    assert_eq!(h.ok("get_update_status", json!({}))["available"], Value::Null);
    let status = h.ok("check_for_update", json!({}));
    assert_eq!(status["available"]["version"], "99.0.0");
    assert_eq!(status["available"]["notes"], "- 模擬サーバーの更新");
    assert_eq!((&status["phase"], &status["ready"]), (&json!("idle"), &json!(false)));
    assert!(status["checkedAt"].as_i64().unwrap() <= now_ms());
    assert_eq!(h.ok("get_update_status", json!({})), status);
    // 自動インストールがオフ（最初の値）なら、見つけても知らせるだけで、ダウンロードしない
    std::thread::sleep(Duration::from_millis(300));
    assert!(h.requests("/update/package").is_empty());
    drop(h);

    let h = Harness::with_endpoint("/update-none/latest.json");
    let err = h.call("check_for_update", json!({})).unwrap_err();
    assert_eq!(
        err,
        "更新の情報を取得できませんでした。まだリリースが公開されていないか、一時的に取得できない状態です"
    );
    assert_eq!(h.ok("get_update_status", json!({}))["available"], Value::Null);
}

/// 自動の確認が失敗しても、画面には何も出さない。
// [UPD-002]
#[test]
fn a_failed_automatic_check_stays_silent() {
    let h = Harness::with_endpoint("/update-none/latest.json");
    start_auto_check(&h);
    wait_until("自動の確認", || (!h.requests("/update-none/latest.json").is_empty()).then_some(()));
    std::thread::sleep(Duration::from_millis(300));
    assert!(h.events("update-status").is_empty(), "失敗を画面に知らせている");
    let status = h.ok("get_update_status", json!({}));
    assert_eq!((&status["available"], &status["checkedAt"]), (&Value::Null, &Value::Null));
}

/// 録音中と AI の作成中は、更新を始めない（画面の確認をすり抜けても、本体が断る）。
// [UPD-008]
#[test]
fn updating_is_refused_while_recording_or_generating() {
    let h = Harness::new();
    h.ok("check_for_update", json!({}));
    let note = h.note("更新のテスト", "- メモ");

    h.ok("start_recording", json!({ "noteId": note }));
    for pause in [false, true] {
        if pause {
            h.ok("pause_recording", json!({}));
        }
        let err = h.call("install_update", json!({})).unwrap_err();
        assert_eq!(err, "録音を停止してから更新してください");
    }
    h.ok("stop_recording", json!({}));

    *h.fixture.api.claude.lock().unwrap() = ClaudeReply::text(&["# タイトル\n\n本文"]).after(Duration::from_millis(1500));
    let generating = h.spawn("enhance_note", json!({ "noteId": note }));
    wait_until("議事録の作成の開始", || (h.claude_requests().len() == 1).then_some(()));
    let err = h.call("install_update", json!({})).unwrap_err();
    assert_eq!(err, "AI が議事録や回答を作成しています。終わってから更新してください");
    generating.join().unwrap().unwrap();

    assert!(h.requests("/update/package").is_empty(), "断ったのにダウンロードしている");
    assert!(phases(&h).iter().all(|phase| phase == "idle"), "{:?}", phases(&h));
}

/// 署名の合わない更新は、ダウンロードしてもインストールしない。知らせは残り、もう一度試せる。
// [UPD-010][UPD-015][SEC-010]
#[test]
fn an_update_with_a_wrong_signature_is_not_installed() {
    let h = Harness::new();
    let status = h.ok("check_for_update", json!({}));
    let err = h.call("install_update", json!({})).unwrap_err();
    assert_eq!(err, "ダウンロードしたファイルの署名を確認できなかったため、更新を中止しました");
    assert_eq!(h.requests("/update/package").len(), 1);
    assert!(phases(&h).contains(&json!("downloading")));
    assert!(!phases(&h).contains(&json!("installing")), "署名を確かめる前に、インストールへ進んでいる");
    // 元の状態に戻り、新しいバージョンの知らせは残る（もう一度試せる）
    assert_eq!(h.ok("get_update_status", json!({})), status);
    let err = h.call("install_update", json!({})).unwrap_err();
    assert!(err.contains("署名を確認できなかった"), "{err}");
    assert_eq!(h.requests("/update/package").len(), 2, "失敗したファイルを使い回さず、ダウンロードからやり直す");
    // 更新のための再起動の印を残していない（次の起動を、更新の直後として扱わない）
    let state = h.app.state::<AppState>();
    assert_eq!(crate::update::take_restart(&state.db(), "0.1.0", now_ms()), None);
}

/// 署名が正しくても、ダウンロードの間に録音が始まっていたら、インストールせずに中止する。
// [UPD-011][UPD-015]
#[test]
fn a_recording_started_during_the_download_stops_the_install() {
    let h = Harness::with_endpoint("/update-signed-slow/latest.json");
    h.set_key(kiroscribe_core::secrets::Provider::OpenAi, super::super::fixture::OPENAI_KEY);
    let note = h.note("更新のテスト", "");
    let status = h.ok("check_for_update", json!({}));

    let installing = h.spawn("install_update", json!({}));
    wait_until("ダウンロードの開始", || phases(&h).contains(&json!("downloading")).then_some(()));
    // 更新の進行中は、もう 1 つ始められない
    assert_eq!(h.call("install_update", json!({})).unwrap_err(), "更新を実行中です");
    h.ok("start_recording", json!({ "noteId": note }));
    assert_eq!(installing.join().unwrap().unwrap_err(), "録音を停止してから更新してください");
    assert!(!phases(&h).contains(&json!("installing")));
    assert_eq!(h.recording_state(), "recording", "録音を止めている");
    assert_eq!(h.ok("get_update_status", json!({})), status);
    h.ok("stop_recording", json!({}));

    // 録音していなければ、署名の確認を通ってインストールへ進む（このファイルはインストーラではないので、そこで止まる）
    let err = h.call("install_update", json!({})).unwrap_err();
    assert!(err.starts_with("更新をインストールできませんでした: "), "{err}");
    assert!(phases(&h).contains(&json!("installing")));
    assert_eq!(h.ok("get_update_status", json!({})), status, "失敗しても、新しいバージョンの知らせは残る");
}

/// 自動インストールの設定: オフなら知らせるだけ。オンにすると、すぐダウンロードして署名を確かめておく。
/// オフに戻すと、ダウンロード済みのファイルを捨てて、知らせるだけに戻る。
// [UPD-020][UPD-021][UPD-022][UPD-026][IPC-008]
#[test]
fn automatic_download_follows_the_setting_without_a_restart() {
    let h = Harness::with_endpoint("/update-signed/latest.json");
    assert!(!h.settings().auto_update, "最初はオフ");
    start_auto_check(&h);
    wait_until("自動の確認で見つかる", || {
        h.events("update-status").into_iter().find(|status| status["available"]["version"] == "99.0.0")
    });
    std::thread::sleep(Duration::from_millis(300));
    assert!(h.requests("/update-signed/package").is_empty(), "オフなのにダウンロードしている");
    assert_eq!(h.ok("get_update_status", json!({}))["ready"], false);

    // オンにして保存すると、利用者の操作なしでダウンロードし、署名を確かめておく
    h.clear_events();
    let saved = h.save_settings(|settings| settings.auto_update = true);
    assert_eq!(saved["autoUpdate"], true);
    wait_until("ダウンロードと署名の確認", || (h.ok("get_update_status", json!({}))["ready"] == true).then_some(()));
    assert_eq!(h.requests("/update-signed/package").len(), 1);
    assert!(phases(&h).contains(&json!("downloading")), "ダウンロードの間の進み具合を知らせる: {:?}", phases(&h));
    // メインウィンドウを表示している間は、インストールへ進まない
    std::thread::sleep(Duration::from_millis(300));
    let status = h.ok("get_update_status", json!({}));
    assert_eq!((&status["phase"], &status["ready"]), (&json!("idle"), &json!(true)));
    assert!(!phases(&h).contains(&json!("installing")));
    // 「更新を確認」をもう一度押しても、同じバージョンならダウンロードし直さない
    h.ok("check_for_update", json!({}));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(h.requests("/update-signed/package").len(), 1);

    // オフに戻すと、待つのをやめて、知らせるだけに戻る
    let saved = h.save_settings(|settings| settings.auto_update = false);
    assert_eq!(saved["autoUpdate"], false);
    let status = h.ok("get_update_status", json!({}));
    assert_eq!(status["ready"], false);
    assert_eq!(status["available"]["version"], "99.0.0", "新しいバージョンの知らせは残す");
    assert_eq!(h.events("update-status").last().unwrap()["ready"], false);
}

/// 自動インストールがオンでも、署名の合わない更新は持たない。そのことは画面に知らせず、すぐには試し直さない。
// [UPD-021][UPD-027]
#[test]
fn an_automatic_download_with_a_wrong_signature_is_dropped_silently() {
    let h = Harness::new();
    h.save_settings(|settings| settings.auto_update = true);
    start_auto_check(&h);
    wait_until("自動のダウンロード", || (!h.requests("/update/package").is_empty()).then_some(()));
    wait_until("ダウンロードの後始末", || (h.events("update-status").last()?["phase"] == "idle").then_some(()));
    let status = h.ok("get_update_status", json!({}));
    assert_eq!(status["ready"], false, "署名の合わないファイルを持っている");
    assert_eq!(status["available"]["version"], "99.0.0", "新しいバージョンの知らせ（「更新」ボタン）は残す");
    assert!(!phases(&h).contains(&json!("installing")));
    // 失敗は、更新の状態のほかには何も知らせない。次に確認できるまで、自動では試し直さない
    assert!(h.events("recording-error").is_empty());
    h.save_settings(|settings| settings.my_name = "田中".into());
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(h.requests("/update/package").len(), 1);
    // 利用者がボタンを押して更新することはできる（署名が合わないので、理由を知らせて止まる）
    assert!(h.call("install_update", json!({})).unwrap_err().contains("署名を確認できなかった"));
}

/// 自動で更新したあとの知らせは、メインウィンドウを表示しているときに 1 回だけ渡す。
// [IPC-008]
#[test]
fn the_notice_after_an_automatic_update_is_handed_over_once() {
    let h = Harness::new();
    assert_eq!(h.ok("take_update_notice", json!({})), Value::Null, "知らせるものがなければ null");
    {
        // 自動で 0.0.9 から更新して起動し直した直後の状態（模擬のアプリのバージョンは 0.1.0）
        let state = h.app.state::<AppState>();
        let db = state.db();
        crate::update::mark_auto_restart(&db, "0.0.9", "0.1.0", now_ms());
        assert_eq!(crate::update::take_restart(&db, "0.1.0", now_ms()), Some(crate::update::Restart::Auto));
    }
    assert_eq!(h.ok("take_update_notice", json!({})), "0.1.0");
    assert_eq!(h.ok("take_update_notice", json!({})), Value::Null, "2 回目は渡さない");

    // 閉じていたメインウィンドウを表示したときは、画面に知らせる（画面は、そこで知らせを受け取りに来る）
    assert!(h.events("main-shown").is_empty());
    h.ok("open_settings_from_popup", json!({}));
    assert_eq!(h.events("main-shown"), [Value::Null]);
}
