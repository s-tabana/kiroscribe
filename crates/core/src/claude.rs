//! Claude（Anthropic Messages API）のストリーミングクライアント。
//!
//! Rust には公式 SDK がないため、`POST /v1/messages` を SSE で直接呼ぶ。
//! - 既定モデル `claude-opus-5-5`。思考（adaptive thinking）は既定で有効なので指定しない
//! - 安全分類器による拒否に備え、対応モデルではサーバー側フォールバック（`fallbacks: "default"`）を有効にする
//! - 文字が出始める前の 429 / 5xx / 過負荷は再試行する

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{api_error_message, redact_api_keys, CoreError, Result};
use crate::retry::{is_retryable, RetryPolicy};
use crate::sse::SseParser;

pub const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com/v1";
pub const ANTHROPIC_VERSION: &str = "2026-10-21";
pub const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// 1 回の呼び出し（応答の終わりまで）を打ち切るまでの時間
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effort {
    Low,
    Medium,
    High,
}

impl Effort {
    fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
        }
    }
}

#[derive(Debug, Clone)]
pub struct MessageRequest {
    /// 長い会議の文脈を含むため、プロンプトキャッシュの対象にする
    pub system: String,
    pub messages: Vec<Turn>,
    pub max_tokens: u32,
    pub effort: Effort,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamResult {
    pub text: String,
    pub stop_reason: Option<String>,
    /// 実際に応答したモデル（フォールバック時は代わりのモデル）
    pub model: Option<String>,
    pub fell_back: bool,
}

/// 中止の指示を確かめる間隔。
const CANCEL_POLL: Duration = Duration::from_millis(50);

/// `future` を、中止の指示が出るまで進める。中止されたら、その場で `Err(CoreError::Cancelled)` を返す
/// （AI からの次のデータや、再試行の待ち時間の終わりを待たない）。
async fn until_cancelled<T>(cancel: &AtomicBool, future: impl std::future::Future<Output = T>) -> Result<T> {
    tokio::pin!(future);
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(CoreError::Cancelled);
        }
        tokio::select! {
            value = &mut future => return Ok(value),
            () = tokio::time::sleep(CANCEL_POLL) => {}
        }
    }
}

/// サーバー側フォールバックの `"default"` 指定を受け付けるモデル。
pub fn supports_server_fallback(model: &str) -> bool {
    matches!(
        model,
        "claude-fable-5-2" | "claude-fable-5-1" | "claude-opus-5-5" | "claude-opus-5" | "claude-sonnet-5-5"
    )
}

/// 接続先。E2E テスト用のビルド（feature `e2e`）でだけ、模擬サーバーに差し替えられる。
fn default_base_url() -> String {
    #[cfg(feature = "e2e")]
    if let Some(url) = crate::e2e::anthropic_base_url() {
        return url;
    }
    ANTHROPIC_BASE_URL.to_string()
}

/// API に送る資格情報
enum Credential {
    /// 利用者の API キー（`x-api-key` で送る）
    ApiKey(String),
    /// 短い期限のアクセストークン（`Authorization: Bearer` で送る）。
    /// アプリでは使わない。本物の API のテスト（crates/core/tests/live_api.rs）が、CI の ID の連携
    /// （Workload Identity Federation）で得たトークンで問い合わせるためのもの
    AccessToken(String),
}

pub struct ClaudeClient {
    client: reqwest::Client,
    base_url: String,
    credential: Credential,
    model: String,
}

impl ClaudeClient {
    pub fn new(api_key: &str, model: &str) -> Self {
        Self::with_credential(Credential::ApiKey(api_key.to_string()), model)
    }

    /// API キーの代わりに、短い期限のアクセストークンで問い合わせる（[`Credential::AccessToken`]）
    pub fn with_access_token(access_token: &str, model: &str) -> Self {
        Self::with_credential(Credential::AccessToken(access_token.to_string()), model)
    }

    fn with_credential(credential: Credential, model: &str) -> Self {
        Self {
            client: crate::http::client(),
            base_url: default_base_url(),
            credential,
            model: model.to_string(),
        }
    }

    pub fn with_base_url(mut self, base_url: &str) -> Self {
        self.base_url = base_url.trim_end_matches('/').to_string();
        self
    }

    pub fn request_body(&self, req: &MessageRequest) -> Value {
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|t| json!({ "role": t.role, "content": t.content }))
            .collect();
        let mut body = json!({
            "model": self.model,
            "max_tokens": req.max_tokens,
            "stream": true,
            "system": [{
                "type": "text",
                "text": req.system,
                "cache_control": { "type": "ephemeral" }
            }],
            "messages": messages,
            "output_config": { "effort": req.effort.as_str() },
        });
        if supports_server_fallback(&self.model) {
            body["fallbacks"] = json!("default");
        }
        body
    }

    /// 1 回だけ呼び出し、届いた文字を順に `on_text` へ渡す。
    async fn stream_once(
        &self,
        req: &MessageRequest,
        on_text: &mut (dyn FnMut(&str) + Send),
        cancel: &AtomicBool,
    ) -> Result<StreamResult> {
        let request = self.client.post(format!("{}/messages", self.base_url));
        let request = match &self.credential {
            Credential::ApiKey(key) => request.header("x-api-key", key),
            Credential::AccessToken(token) => request.bearer_auth(token),
        };
        let mut request = request
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .timeout(REQUEST_TIMEOUT)
            .json(&self.request_body(req));
        if supports_server_fallback(&self.model) {
            request = request.header("anthropic-beta", FALLBACK_BETA);
        }
        let response = until_cancelled(cancel, request.send()).await??;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(CoreError::Api {
                service: "Anthropic",
                status: status.as_u16(),
                message: api_error_message(&body),
            });
        }

        let mut result = StreamResult::default();
        let mut parser = SseParser::default();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = until_cancelled(cancel, stream.next()).await? {
            for event in parser.push(&chunk?) {
                let Ok(data) = serde_json::from_str::<Value>(&event.data) else {
                    continue;
                };
                match data["type"].as_str().unwrap_or_default() {
                    "message_start" => {
                        result.model = data["message"]["model"].as_str().map(str::to_string);
                    }
                    "content_block_start" => {
                        if data["content_block"]["type"] == "fallback" {
                            // 拒否されたため別のモデルが続きを書く（途中までの文章はそのまま続く）
                            result.fell_back = true;
                            if let Some(model) = data["content_block"]["to"]["model"].as_str() {
                                result.model = Some(model.to_string());
                            }
                        }
                    }
                    "content_block_delta" => {
                        if data["delta"]["type"] == "text_delta" {
                            if let Some(text) = data["delta"]["text"].as_str() {
                                result.text.push_str(text);
                                on_text(text);
                            }
                        }
                    }
                    "message_delta" => {
                        if let Some(reason) = data["delta"]["stop_reason"].as_str() {
                            result.stop_reason = Some(reason.to_string());
                        }
                    }
                    "error" => {
                        let kind = data["error"]["type"].as_str().unwrap_or_default();
                        let status = match kind {
                            "overloaded_error" => 529,
                            "rate_limit_error" => 429,
                            "api_error" => 500,
                            _ => 400,
                        };
                        return Err(CoreError::Api {
                            service: "Anthropic",
                            status,
                            message: redact_api_keys(
                                data["error"]["message"].as_str().unwrap_or(kind),
                            ),
                        });
                    }
                    _ => {} // ping・thinking などは使わない
                }
            }
        }

        if result.stop_reason.as_deref() == Some("refusal") {
            return Err(CoreError::Refused);
        }
        Ok(result)
    }

    /// 文字が出始める前の一時的なエラーは再試行する。出始めた後のエラーは途中の文章が混ざるため再試行しない。
    pub async fn stream(
        &self,
        req: &MessageRequest,
        on_text: &mut (dyn FnMut(&str) + Send),
        cancel: &AtomicBool,
        policy: &RetryPolicy,
    ) -> Result<StreamResult> {
        let mut attempt = 0;
        loop {
            let mut emitted = false;
            let result = {
                let mut forward = |text: &str| {
                    emitted = true;
                    on_text(text);
                };
                self.stream_once(req, &mut forward, cancel).await
            };
            match result {
                Err(err) if !emitted && attempt + 1 < policy.max_attempts && is_retryable(&err) => {
                    until_cancelled(cancel, tokio::time::sleep(policy.delay_after(attempt))).await?;
                    attempt += 1;
                }
                other => return other,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    use super::*;

    fn sse(events: &[Value]) -> String {
        events
            .iter()
            .map(|e| format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap(), e))
            .collect()
    }

    fn text_stream(chunks: &[&str], stop_reason: &str) -> String {
        let mut events = vec![
            json!({"type": "message_start", "message": {"model": "claude-opus-5-5"}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
        ];
        for chunk in chunks {
            events.push(json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": chunk}}));
        }
        events.push(json!({"type": "content_block_stop", "index": 1}));
        events.push(json!({"type": "message_delta", "delta": {"stop_reason": stop_reason}, "usage": {"output_tokens": 5}}));
        events.push(json!({"type": "message_stop"}));
        sse(&events)
    }

    fn request() -> MessageRequest {
        MessageRequest {
            system: "あなたは議事録係です".into(),
            messages: vec![Turn {
                role: Role::User,
                content: "まとめて".into(),
            }],
            max_tokens: 1000,
            effort: Effort::Medium,
        }
    }

    fn fast() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
        }
    }

    fn sse_response(body: String) -> ResponseTemplate {
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(body)
    }

    // [MIN-014][MIN-015][SEC-004]
    #[tokio::test]
    async fn streams_text_and_sends_expected_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/messages"))
            .and(header("x-api-key", "sk-ant-test"))
            .and(header("anthropic-version", ANTHROPIC_VERSION))
            .and(header("anthropic-beta", FALLBACK_BETA))
            .and(body_partial_json(json!({
                "model": "claude-opus-5-5",
                "stream": true,
                "fallbacks": "default",
                "output_config": {"effort": "medium"},
                "system": [{"type": "text", "text": "あなたは議事録係です", "cache_control": {"type": "ephemeral"}}],
                "messages": [{"role": "user", "content": "まとめて"}]
            })))
            .respond_with(sse_response(text_stream(&["## 決定", "事項\n- 12/5"], "end_turn")))
            .expect(1)
            .mount(&server)
            .await;

        let client = ClaudeClient::new("sk-ant-test", "claude-opus-5-5").with_base_url(&server.uri());
        let mut received = Vec::new();
        let result = client
            .stream(&request(), &mut |t| received.push(t.to_string()), &AtomicBool::new(false), &fast())
            .await
            .unwrap();
        assert_eq!(received, ["## 決定", "事項\n- 12/5"]);
        assert_eq!(result.text, "## 決定事項\n- 12/5");
        assert_eq!(result.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(result.model.as_deref(), Some("claude-opus-5-5"));
        assert!(!result.fell_back);
    }

    /// アクセストークン（本物の API のテストが、CI の ID の連携で得るもの）は `Authorization: Bearer` で送り、
    /// `x-api-key` は付けない。ほかの送り方（本文・ヘッダー）は API キーのときと同じ
    #[tokio::test]
    async fn access_token_is_sent_as_bearer_instead_of_api_key() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/messages"))
            .and(header("authorization", "Bearer sk-ant-oat01-test"))
            .and(header("anthropic-version", ANTHROPIC_VERSION))
            .and(header("anthropic-beta", FALLBACK_BETA))
            .and(body_partial_json(json!({"model": "claude-opus-5-5", "stream": true})))
            .respond_with(sse_response(text_stream(&["了解"], "end_turn")))
            .expect(1)
            .mount(&server)
            .await;

        let client = ClaudeClient::with_access_token("sk-ant-oat01-test", "claude-opus-5-5").with_base_url(&server.uri());
        let result = client.stream(&request(), &mut |_| {}, &AtomicBool::new(false), &fast()).await.unwrap();
        assert_eq!(result.text, "了解");
        let requests = server.received_requests().await.unwrap();
        assert!(requests.iter().all(|r| !r.headers.contains_key("x-api-key")));
    }

    // [MIN-015]
    #[test]
    fn fallbacks_only_for_supported_models() {
        let haiku = ClaudeClient::new("k", "claude-haiku-5-5");
        let body = haiku.request_body(&request());
        assert!(body.get("fallbacks").is_none());
        let opus = ClaudeClient::new("k", "claude-opus-5-5");
        assert_eq!(opus.request_body(&request())["fallbacks"], "default");
    }

    // [MIN-015]
    #[tokio::test]
    async fn mid_stream_fallback_continues_text() {
        let server = MockServer::start().await;
        let body = sse(&[
            json!({"type": "message_start", "message": {"model": "claude-opus-5-5"}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "前半"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "fallback", "from": {"model": "claude-opus-5-5"}, "to": {"model": "claude-opus-4-8"}}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "content_block_start", "index": 2, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 2, "delta": {"type": "text_delta", "text": "後半"}}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
        ]);
        Mock::given(method("POST")).respond_with(sse_response(body)).mount(&server).await;
        let client = ClaudeClient::new("k", "claude-opus-5-5").with_base_url(&server.uri());
        let result = client
            .stream(&request(), &mut |_| {}, &AtomicBool::new(false), &fast())
            .await
            .unwrap();
        assert_eq!(result.text, "前半後半");
        assert!(result.fell_back);
        assert_eq!(result.model.as_deref(), Some("claude-opus-4-8"));
    }

    // [MIN-015]
    #[tokio::test]
    async fn refusal_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(sse_response(text_stream(&[], "refusal")))
            .mount(&server)
            .await;
        let client = ClaudeClient::new("k", "claude-opus-5-5").with_base_url(&server.uri());
        let err = client
            .stream(&request(), &mut |_| {}, &AtomicBool::new(false), &fast())
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::Refused));
    }

    /// 1 回目は過負荷（529）、2 回目は成功
    struct OverloadedOnce(AtomicUsize);

    impl Respond for OverloadedOnce {
        fn respond(&self, _: &Request) -> ResponseTemplate {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(529).set_body_json(json!({
                    "type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}
                }))
            } else {
                sse_response(text_stream(&["OK"], "end_turn"))
            }
        }
    }

    // [MIN-016]
    #[tokio::test]
    async fn retries_before_any_text() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(OverloadedOnce(AtomicUsize::new(0)))
            .expect(2)
            .mount(&server)
            .await;
        let client = ClaudeClient::new("k", "claude-opus-5-5").with_base_url(&server.uri());
        let result = client
            .stream(&request(), &mut |_| {}, &AtomicBool::new(false), &fast())
            .await
            .unwrap();
        assert_eq!(result.text, "OK");
    }

    // [MIN-016][ERR-007]
    #[tokio::test]
    async fn error_event_after_text_is_not_retried() {
        let server = MockServer::start().await;
        let body = sse(&[
            json!({"type": "message_start", "message": {"model": "claude-opus-5-5"}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "途中"}}),
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
        ]);
        Mock::given(method("POST"))
            .respond_with(sse_response(body))
            .expect(1)
            .mount(&server)
            .await;
        let client = ClaudeClient::new("k", "claude-opus-5-5").with_base_url(&server.uri());
        let err = client
            .stream(&request(), &mut |_| {}, &AtomicBool::new(false), &fast())
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::Api { status: 529, .. }));
    }

    // [ERR-003]
    #[tokio::test]
    async fn auth_errors_surface_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "type": "error", "error": {"type": "authentication_error", "message": "invalid x-api-key"}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = ClaudeClient::new("bad", "claude-opus-5-5").with_base_url(&server.uri());
        let err = client
            .stream(&request(), &mut |_| {}, &AtomicBool::new(false), &fast())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Anthropic の API キーが正しくない"));
        assert!(err.to_string().contains("invalid x-api-key"));
    }

    // [MIN-005][CHT-006]
    #[tokio::test]
    async fn cancel_stops_streaming() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(sse_response(text_stream(&["a", "b"], "end_turn")))
            .mount(&server)
            .await;
        let client = ClaudeClient::new("k", "claude-opus-5-5").with_base_url(&server.uri());
        let err = client
            .stream(&request(), &mut |_| {}, &AtomicBool::new(true), &fast())
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::Cancelled));
    }
    /// 中止を指示してから、呼び出しが「中止しました」で戻るまでの時間。
    async fn cancelled_within(
        client: &ClaudeClient,
        policy: &RetryPolicy,
        cancel_after: Duration,
        received: &mut Vec<String>,
    ) -> Duration {
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let canceller = tokio::spawn(async move {
            tokio::time::sleep(cancel_after).await;
            flag.store(true, Ordering::SeqCst);
            std::time::Instant::now()
        });
        let mut on_text = |text: &str| received.push(text.to_string());
        let result = client.stream(&request(), &mut on_text, &cancel, policy).await;
        let returned_at = std::time::Instant::now();
        assert!(matches!(result, Err(CoreError::Cancelled)), "{result:?}");
        returned_at.duration_since(canceller.await.unwrap())
    }

    /// 中止は、AI からの次のデータを待たずに、すぐ効く: 応答が始まるのを待っているあいだでも。
    // [MIN-005][CHT-006]
    #[tokio::test]
    async fn cancel_takes_effect_while_waiting_for_the_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(sse_response(text_stream(&["遅れて届く回答"], "end_turn")).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;
        let client = ClaudeClient::new("k", "claude-opus-5-5").with_base_url(&server.uri());
        let mut received = Vec::new();
        let took = cancelled_within(&client, &fast(), Duration::from_millis(200), &mut received).await;
        assert!(took < Duration::from_millis(500), "中止が効くまでに {took:?} かかった");
        assert!(received.is_empty());
    }

    /// 文章の途中で AI からのデータが止まっているあいだでも、中止はすぐ効く。
    // [MIN-005][CHT-006]
    #[tokio::test]
    async fn cancel_takes_effect_while_the_stream_is_silent() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // 前半を送ったあと、何も送らずに接続を保ち続けるサーバー
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0u8; 16 * 1024];
            let _ = socket.read(&mut buffer).await;
            let events = sse(&[
                json!({"type": "message_start", "message": {"model": "claude-opus-5-5"}}),
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "書きかけ"}}),
            ]);
            let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
            let chunk = format!("{:x}\r\n{events}\r\n", events.len());
            socket.write_all(format!("{head}{chunk}").as_bytes()).await.unwrap();
            socket.flush().await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let client = ClaudeClient::new("k", "claude-opus-5-5").with_base_url(&format!("http://{address}"));
        let mut received = Vec::new();
        let took = cancelled_within(&client, &fast(), Duration::from_millis(300), &mut received).await;
        assert!(took < Duration::from_millis(500), "中止が効くまでに {took:?} かかった");
        assert_eq!(received, ["書きかけ"], "止まる前に届いた分は、画面に渡っている");
    }

    /// 一時的な失敗のあと、再試行を待っているあいだでも、中止はすぐ効く。
    // [MIN-005][CHT-006]
    #[tokio::test]
    async fn cancel_takes_effect_while_waiting_to_retry() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_json(json!({"error": {"type": "api_error", "message": "x"}})))
            .mount(&server)
            .await;
        let client = ClaudeClient::new("k", "claude-opus-5-5").with_base_url(&server.uri());
        let slow_retry = RetryPolicy {
            max_attempts: 4,
            base_delay: Duration::from_secs(5),
            max_delay: Duration::from_secs(5),
        };
        let mut received = Vec::new();
        let took = cancelled_within(&client, &slow_retry, Duration::from_millis(300), &mut received).await;
        assert!(took < Duration::from_millis(500), "中止が効くまでに {took:?} かかった");
        assert_eq!(server.received_requests().await.unwrap().len(), 1, "中止したあとに、もう一度送っている");
    }
}
