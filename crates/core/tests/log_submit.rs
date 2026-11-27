//! デバッグ用のログの提出（`log_submit`）を、契約どおりに振る舞う偽の入口を相手に確かめるテスト。

use std::sync::{Arc, Mutex};

use kiroscribe_core::log_submit::{
    client, submit_logs, Intake, Progress, Submission, CLIENT_ID_HEADER, TOKEN_HEADER, VERSION_HEADER,
};
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn zip(size: usize) -> Vec<u8> {
    let mut bytes: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
    bytes[..4].copy_from_slice(b"PK\x03\x04");
    bytes
}

fn recorder() -> (Progress, Arc<Mutex<Vec<(u64, u64)>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    (Arc::new(move |sent, total| sink.lock().unwrap().push((sent, total))), seen)
}

async fn send(server: &MockServer, body: Vec<u8>) -> kiroscribe_core::Result<Submission> {
    let (progress, _) = recorder();
    submit_logs(
        &client(),
        &Intake::local(&server.uri(), "kiroscribe-token").unwrap(),
        "0.4.1",
        "3f6c0a5e-0000-4000-8000-000000000000",
        body,
        progress,
    )
    .await
}

// [SET-017]
#[tokio::test]
async fn sends_the_zip_as_is_and_returns_the_submission() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/logs/kiroscribe"))
        .and(header("content-type", "application/zip"))
        .and(header(VERSION_HEADER, "0.4.1"))
        .and(header(CLIENT_ID_HEADER, "3f6c0a5e-0000-4000-8000-000000000000"))
        .and(header(TOKEN_HEADER, "kiroscribe-token"))
        .respond_with(
            ResponseTemplate::new(201)
                .set_body_json(json!({ "id": "KS-20261127-7F3A", "receivedAt": "2026-11-27T12:50:30.217Z" })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let body = zip(200_000);
    let (progress, seen) = recorder();
    let submission = submit_logs(
        &client(),
        &Intake::local(&format!("{}/", server.uri()), "kiroscribe-token").unwrap(),
        "0.4.1",
        "3f6c0a5e-0000-4000-8000-000000000000",
        body.clone(),
        progress,
    )
    .await
    .unwrap();
    assert_eq!(
        submission,
        Submission { id: "KS-20261127-7F3A".into(), received_at: "2026-11-27T12:50:30.217Z".into() }
    );

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests[0].body, body, "zip をそのまま送る");
    assert_eq!(requests[0].headers.get("content-length").unwrap(), "200000");

    // 進み具合は、送った分が増えていき、最後は全体に届く
    let seen = seen.lock().unwrap().clone();
    assert!(seen.len() >= 3, "{seen:?}");
    assert!(seen.windows(2).all(|pair| pair[0].0 < pair[1].0));
    assert!(seen.iter().all(|&(_, total)| total == 200_000));
    assert_eq!(seen.last().unwrap().0, 200_000);
}

// [SET-018]
#[tokio::test]
async fn rejections_show_the_intake_explanation() {
    let cases = [
        (
            429,
            json!({ "error": "rate_limited", "message": "提出が多すぎます。1 時間ほど待ってから送ってください" }),
            "ログを送れませんでした: 提出が多すぎます。1 時間ほど待ってから送ってください（HTTP 429）",
        ),
        (
            413,
            json!({ "error": "too_large" }),
            "ログが大きすぎるため、送り先が受け付けませんでした（上限は 20 MB）。「デバッグ用のログを保存…」で保存して、別の方法で渡してください",
        ),
        (
            500,
            json!({}),
            "ログの送り先がエラーを返しました（HTTP 500）。しばらく待ってから、もう一度お試しください",
        ),
    ];
    for (status, body, expected) in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(&server)
            .await;
        let err = send(&server, zip(100)).await.unwrap_err();
        assert_eq!(err.to_string(), expected, "HTTP {status}");
    }
}

// [SET-018]
#[tokio::test]
async fn unreadable_success_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(201).set_body_string("<html>"))
        .mount(&server)
        .await;
    let err = send(&server, zip(100)).await.unwrap_err();
    assert_eq!(err.to_string(), "ログは送りましたが、送り先の応答を読めませんでした");
}

// [SET-018]
#[tokio::test]
async fn unreachable_intake_says_to_check_the_connection() {
    // 使っていないポートを取ってから閉じ、つながらない送り先にする
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let (progress, _) = recorder();
    let err = submit_logs(
        &client(),
        &Intake::local(&format!("http://127.0.0.1:{port}"), "t").unwrap(),
        "0.4.1",
        "client",
        zip(100),
        progress,
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string()
            .starts_with("ログを送れませんでした。送り先につながりませんでした。インターネットに接続しているか"),
        "{err}"
    );
}

/// 入口がリダイレクトを返しても従わず、ほかの場所には送らない。
// [SEC-015][SET-018]
#[tokio::test]
async fn redirects_are_not_followed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/logs/kiroscribe"))
        .respond_with(ResponseTemplate::new(308).insert_header("location", format!("{}/elsewhere", server.uri())))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/elsewhere"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": "KS-ELSEWHERE", "receivedAt": "" })))
        .expect(0)
        .mount(&server)
        .await;
    let message = send(&server, zip(100)).await.unwrap_err().to_string();
    assert!(message.contains("HTTP 308"), "{message}");
    assert!(message.contains("最新版に更新"), "{message}");
    assert!(message.contains("デバッグ用のログを保存"), "{message}");
}
