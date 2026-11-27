//! デバッグ用のログの提出: zip を契約どおりに送ること、
//! 進み具合のイベント、断られたときの文。

use serde_json::{json, Value};

use super::super::fixture::{Harness, LOG_SUBMISSION_ID};

fn configure(h: &Harness, base: &str) {
    let url = format!("{}{base}", h.fixture.server_uri);
    h.save_settings(|settings| {
        settings.log_intake_url = Some(url);
        settings.log_intake_token = Some("kiroscribe-test-token".into());
    });
}

/// zip を契約どおりのヘッダーで送り、提出の番号を返す。導入ごとの ID は、2 回目も同じ値を使う。
// [SET-015][SET-017][IPC-001][IPC-005]
#[test]
fn submits_the_bundle_with_the_contract_headers() {
    let h = Harness::new();
    configure(&h, "/logs/");

    let first = h.ok("submit_debug_logs", json!({}));
    assert_eq!(first, json!({ "id": LOG_SUBMISSION_ID, "receivedAt": "2026-11-27T12:50:30.217Z" }));
    h.ok("submit_debug_logs", json!({}));

    let requests = h.requests("/logs/v1/logs/kiroscribe");
    assert_eq!(requests.len(), 2);
    let header = |i: usize, name: &str| requests[i].headers.get(name).unwrap().to_str().unwrap().to_string();
    assert_eq!(header(0, "content-type"), "application/zip");
    assert_eq!(header(0, "x-log-app-version"), h.ok("get_app_version", json!({})).as_str().unwrap());
    assert_eq!(header(0, "x-log-token"), "kiroscribe-test-token");
    let client_id = header(0, "x-log-client-id");
    assert!(client_id.len() == 36 && client_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'), "{client_id}");
    assert_eq!(header(1, "x-log-client-id"), client_id, "導入ごとの ID は変えない");
    // 本文は「デバッグ用のログを保存…」と同じ zip
    assert!(requests[0].body.starts_with(b"PK\x03\x04"), "zip ではない");
    assert_eq!(header(0, "content-length"), requests[0].body.len().to_string());
    let names: Vec<String> = {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(requests[0].body.clone())).unwrap();
        (0..zip.len()).map(|i| zip.by_index(i).unwrap().name().unwrap().to_string()).collect()
    };
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
    configure(&h, "/logs-busy/");
    let err = h.call("submit_debug_logs", json!({})).unwrap_err();
    assert_eq!(err, "ログを送れませんでした: 提出が多すぎます。1 時間ほど待ってから送ってください（HTTP 429）");
    let failed = h.debug_log_events("log.submit.failed");
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0]["level"], "error");
    assert_eq!(failed[0]["fields"]["message"], Value::String(err));
}
