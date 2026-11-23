//! OpenAI の音声文字起こし API（`POST /v1/audio/transcriptions`）。

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::Deserialize;

use crate::error::{api_error_message, CoreError, Result};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 文字起こしエンジン。テストでは偽物に差し替える。
pub trait Transcribe: Send + Sync {
    /// 16kHz モノラル WAV を文字にする。`prompt` は直前の文脈（表記ゆれ・固有名詞の手がかり）。
    fn transcribe(&self, wav: Vec<u8>, prompt: Option<String>) -> BoxFuture<'_, Result<String>>;
}

pub const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
/// 1 回の送信を打ち切るまでの時間
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);

pub struct OpenAiTranscriber {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
    language: Option<String>,
    timeout: Duration,
}

#[derive(Deserialize)]
struct TranscriptionResponse {
    text: String,
}

impl OpenAiTranscriber {
    /// `language` が "auto" または空なら言語を指定せず自動判定させる。
    pub fn new(api_key: &str, model: &str, language: &str) -> Self {
        let language = Some(language.trim())
            .filter(|l| !l.is_empty() && *l != "auto")
            .map(str::to_string);
        Self {
            client: crate::http::client(),
            base_url: default_base_url(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            language,
            timeout: REQUEST_TIMEOUT,
        }
    }

    pub fn with_base_url(mut self, base_url: &str) -> Self {
        self.base_url = base_url.trim_end_matches('/').to_string();
        self
    }

    /// 1 回の送信を打ち切るまでの時間を変える（テスト用。既定は [`REQUEST_TIMEOUT`]）。
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    async fn request(&self, wav: Vec<u8>, prompt: Option<String>) -> Result<String> {
        let file = reqwest::multipart::Part::bytes(wav)
            .file_name("audio.wav")
            .mime_str("audio/wav")?;
        let mut form = reqwest::multipart::Form::new()
            .part("file", file)
            .text("model", self.model.clone())
            .text("response_format", "json");
        if let Some(language) = &self.language {
            form = form.text("language", language.clone());
        }
        if let Some(prompt) = prompt.filter(|p| !p.trim().is_empty()) {
            form = form.text("prompt", prompt);
        }

        let response = self
            .client
            .post(format!("{}/audio/transcriptions", self.base_url))
            .bearer_auth(&self.api_key)
            .multipart(form)
            .timeout(self.timeout)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        if !status.is_success() {
            return Err(CoreError::Api {
                service: "OpenAI",
                status: status.as_u16(),
                message: api_error_message(&body),
            });
        }
        let parsed: TranscriptionResponse = serde_json::from_str(&body)?;
        Ok(parsed.text)
    }
}

impl Transcribe for OpenAiTranscriber {
    fn transcribe(&self, wav: Vec<u8>, prompt: Option<String>) -> BoxFuture<'_, Result<String>> {
        Box::pin(self.request(wav, prompt))
    }
}

/// 接続先。E2E テスト用のビルド（feature `e2e`）でだけ、模擬サーバーに差し替えられる。
fn default_base_url() -> String {
    #[cfg(feature = "e2e")]
    if let Some(url) = crate::e2e::openai_base_url() {
        return url;
    }
    OPENAI_BASE_URL.to_string()
}

/// 無音や雑音に対して音声認識モデルが出しがちな定型文。これだけの結果は捨てる。
const HALLUCINATIONS: &[&str] = &[
    "ご視聴ありがとうございました",
    "ご視聴ありがとうございます",
    "チャンネル登録よろしくお願いします",
    "チャンネル登録お願いします",
    "最後までご視聴いただきありがとうございました",
    "thankyouforwatching",
    "thanksforwatching",
];

/// 文字起こし結果を整え、意味のないものは `None` にする。
pub fn clean_transcript(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let normalized: String = trimmed
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    if normalized.is_empty() || HALLUCINATIONS.contains(&normalized.as_str()) {
        return None;
    }
    Some(trimmed.to_string())
}

/// 直前の発話の末尾を、次のリクエストの文脈として渡す長さに切り詰める。
pub fn context_tail(previous: &str, max_chars: usize) -> String {
    let count = previous.chars().count();
    previous.chars().skip(count.saturating_sub(max_chars)).collect()
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    // [TRN-008][TRN-009][SEC-004]
    #[tokio::test]
    async fn sends_multipart_and_returns_text() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/audio/transcriptions"))
            .and(header("authorization", "Bearer sk-test"))
            .and(body_string_contains("gpt-4o-transcribe"))
            .and(body_string_contains("name=\"language\""))
            .and(body_string_contains("前回の続き"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": "こんにちは"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let t = OpenAiTranscriber::new("sk-test", "gpt-4o-transcribe", "ja")
            .with_base_url(&server.uri());
        let text = t
            .transcribe(b"RIFFfake".to_vec(), Some("前回の続き".into()))
            .await
            .unwrap();
        assert_eq!(text, "こんにちは");

        let requests = server.received_requests().await.unwrap();
        let body = String::from_utf8_lossy(&requests[0].body);
        assert!(body.contains("filename=\"audio.wav\""));
        assert!(body.contains("audio/wav"));
    }

    // [TRN-009][TRN-010]
    #[tokio::test]
    async fn auto_language_is_not_sent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": "hi"})))
            .mount(&server)
            .await;
        let t = OpenAiTranscriber::new("k", "whisper-1", "auto").with_base_url(&server.uri());
        t.transcribe(vec![0; 4], None).await.unwrap();
        let requests = server.received_requests().await.unwrap();
        let body = String::from_utf8_lossy(&requests[0].body);
        assert!(!body.contains("name=\"language\""));
        assert!(!body.contains("name=\"prompt\""));
    }

    // [ERR-003]
    #[tokio::test]
    async fn api_errors_carry_status_and_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_json(serde_json::json!({
                "error": {"message": "Incorrect API key provided", "type": "invalid_request_error"}
            })))
            .mount(&server)
            .await;
        let t = OpenAiTranscriber::new("bad", "gpt-4o-transcribe", "ja").with_base_url(&server.uri());
        let err = t.transcribe(vec![0; 4], None).await.unwrap_err();
        match err {
            CoreError::Api {
                service,
                status,
                message,
            } => {
                assert_eq!(service, "OpenAI");
                assert_eq!(status, 401);
                assert_eq!(message, "Incorrect API key provided");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    // [TRN-012]
    #[test]
    fn hallucinations_and_empty_results_are_dropped() {
        assert_eq!(clean_transcript("  "), None);
        assert_eq!(clean_transcript("。、…"), None);
        assert_eq!(clean_transcript("ご視聴ありがとうございました。"), None);
        assert_eq!(clean_transcript("Thank you for watching!"), None);
        assert_eq!(
            clean_transcript(" 来週までに見積もりを出します。 ").as_deref(),
            Some("来週までに見積もりを出します。")
        );
    }

    // [TRN-010]
    #[test]
    fn context_tail_keeps_last_chars() {
        assert_eq!(context_tail("あいうえお", 3), "うえお");
        assert_eq!(context_tail("abc", 10), "abc");
    }
}
