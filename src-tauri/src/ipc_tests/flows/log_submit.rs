//! デバッグ用のログの提出: zip を契約どおりに送ること、
//! 進み具合のイベント、断られたときの文、送り先を画面から変えられないこと。

use serde_json::{json, Value};

use super::super::fixture::{Harness, ANTHROPIC_KEY, LOG_SUBMISSION_ID, OPENAI_KEY};

/// zip を契約どおりのヘッダーで送り、提出の番号を返す。導入ごとの ID は、2 回目も同じ値を使う。
// [SET-015][SET-017][SEC-015][IPC-001][IPC-005]
#[test]
fn submits_the_bundle_with_the_contract_headers() {
    let h = Harness::new();

    let first = h.ok("submit_debug_logs", json!({}));
    assert_eq!(first, json!({ "id": LOG_SUBMISSION_ID, "receivedAt": "2026-11-27T12:50:30.217Z" }));
    h.ok("submit_debug_logs", json!({}));

    let requests = h.requests("/logs/v1/logs/kiroscribe");
    assert_eq!(requests.len(), 2);
    let header = |i: usize, name: &str| requests[i].headers.get(name).unwrap().to_str().unwrap().to_string();
    assert_eq!(header(0, "content-type"), "application/zip");
    assert_eq!(header(0, "x-log-app-version"), h.ok("get_app_version", json!({})).as_str().unwrap());
    assert_eq!(header(0, "x-log-token"), kiroscribe_core::e2e::LOG_INTAKE_TOKEN);
    let client_id = header(0, "x-log-client-id");
    assert!(client_id.len() == 36 && client_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'), "{client_id}");
    assert_eq!(header(1, "x-log-client-id"), client_id, "導入ごとの ID は変えない");
    // 本文は「デバッグ用のログを保存…」と同じ zip
    assert!(requests[0].body.starts_with(b"PK\x03\x04"), "zip ではない");
    assert_eq!(header(0, "content-length"), requests[0].body.len().to_string());
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(requests[0].body.clone())).unwrap();
    let mut names = Vec::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).unwrap();
        let mut text = String::new();
        std::io::Read::read_to_string(&mut entry, &mut text).unwrap_or_default();
        assert!(!text.contains(OPENAI_KEY) && !text.contains(ANTHROPIC_KEY), "{} に API キーが入っている", entry.name().unwrap());
        names.push(entry.name().unwrap().to_string());
    }
    assert!(names.iter().any(|name| name == "environment.json"), "{names:?}");

    // 進み具合は、送った分と全体の大きさ（バイト）。最後は全体に届く
    let progress = h.events("log-submit-progress");
    let last = progress.last().expect("進み具合のイベントがない");
    assert_eq!(last["sent"], last["total"]);
    assert_eq!(last["total"].as_u64().unwrap() as usize, requests[1].body.len());

    let done = h.debug_log_events("log.submit.done");
    assert_eq!(done.len(), 2);
    assert_eq!(done[0]["fields"]["id"], LOG_SUBMISSION_ID);
}

/// 入口が断ったら、入口の説明を添えた文を返し、ログにも書く。
// [SET-018][IPC-006]
#[test]
fn rejection_from_the_intake_becomes_the_message() {
    let h = Harness::new();
    *h.fixture.api.log_intake_status.lock().unwrap() = Some(429);
    let err = h.call("submit_debug_logs", json!({})).unwrap_err();
    assert_eq!(err, "ログを送れませんでした: 提出が多すぎます。1 時間ほど待ってから送ってください（HTTP 429）");
    let failed = h.debug_log_events("log.submit.failed");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["level"], "error");
    assert_eq!(failed[0]["fields"]["message"], Value::String(err));
}

/// 入口がリダイレクトを返しても、ほかの場所には送らない。
// [SEC-015]
#[test]
fn redirects_from_the_intake_are_not_followed() {
    let h = Harness::new();
    *h.fixture.api.log_intake_status.lock().unwrap() = Some(307);
    let err = h.call("submit_debug_logs", json!({})).unwrap_err();
    assert!(err.contains("HTTP 307"), "{err}");
    assert!(h.requests("/redirected/v1/logs/kiroscribe").is_empty(), "リダイレクト先に送った");
    assert_eq!(h.requests("/logs/v1/logs/kiroscribe").len(), 1);
}

/// 画面から設定に送り先や提出用の値を書いても、送り先は変わらず、設定にも残らない。
// [SEC-015][SET-019]
#[test]
fn the_screen_cannot_change_the_destination() {
    let h = Harness::new();
    let mut settings = h.ok("get_settings", json!({}));
    assert!(settings.get("logIntakeUrl").is_none() && settings.get("logIntakeToken").is_none(), "{settings}");
    settings["logIntakeUrl"] = json!(format!("{}/evil", h.fixture.server_uri));
    settings["logIntakeToken"] = json!("changed-token");
    let saved = h.ok("save_settings", json!({ "settings": settings }));
    assert!(saved.get("logIntakeUrl").is_none() && saved.get("logIntakeToken").is_none(), "{saved}");

    h.ok("submit_debug_logs", json!({}));
    assert!(h.requests("/evil/v1/logs/kiroscribe").is_empty(), "画面が書いた送り先に送った");
    let requests = h.requests("/logs/v1/logs/kiroscribe");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].headers.get("x-log-token").unwrap().to_str().unwrap(),
        kiroscribe_core::e2e::LOG_INTAKE_TOKEN
    );
}
