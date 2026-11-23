//! 通信まわりの仕様のテスト（打ち切り・エラーの文・再試行・送り先・API キーを表示しないこと）と、
//! 録音のパイプラインの細かい振る舞い（文脈の長さ・入力の大きさ）。

use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use kiroscribe_core::claude::{ClaudeClient, Effort, MessageRequest, Role, Turn};
use kiroscribe_core::help::HelpClient;
use kiroscribe_core::recorder::{
    run_transcription_worker, AudioFrame, RecorderSink, SourceProcessor, TranscribeJob,
};
use kiroscribe_core::retry::{is_retryable, retry_async, RetryPolicy};
use kiroscribe_core::transcribe::{BoxFuture, OpenAiTranscriber, Transcribe};
use kiroscribe_core::transcript::Speaker;
use kiroscribe_core::{CoreError, Result};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn fast() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 3,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(2),
    }
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

fn sse(events: &[Value]) -> ResponseTemplate {
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {}\n\n", e["type"].as_str().unwrap(), e))
        .collect();
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

fn text_events(text: &str) -> Vec<Value> {
    vec![
        json!({"type": "message_start", "message": {"model": "claude-opus-5-5"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": text}}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
    ]
}

async fn stream(client: &ClaudeClient) -> Result<String> {
    client
        .stream(&request(), &mut |_| {}, &AtomicBool::new(false), &fast())
        .await
        .map(|result| result.text)
}

// ---- 打ち切りと、通信に失敗したときの文 ----

// [TRN-015][MIN-014][HLP-003]
#[test]
fn request_timeouts_are_the_documented_values() {
    assert_eq!(kiroscribe_core::transcribe::REQUEST_TIMEOUT, Duration::from_secs(90));
    assert_eq!(kiroscribe_core::http::CONNECT_TIMEOUT, Duration::from_secs(15));
    assert_eq!(kiroscribe_core::claude::REQUEST_TIMEOUT, Duration::from_secs(600));
    assert_eq!(kiroscribe_core::help::REQUEST_TIMEOUT, Duration::from_secs(10));
}

// [TRN-015][ERR-005]
#[tokio::test]
async fn slow_transcription_times_out_and_is_retried_as_a_network_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"text": "遅い応答"}))
                .set_delay(Duration::from_secs(5)),
        )
        .mount(&server)
        .await;
    let transcriber = OpenAiTranscriber::new("sk-test", "gpt-4o-transcribe", "ja")
        .with_base_url(&server.uri())
        .with_timeout(Duration::from_millis(150));

    let attempts = AtomicUsize::new(0);
    let err = retry_async(
        &fast(),
        || {
            attempts.fetch_add(1, Ordering::SeqCst);
            transcriber.transcribe(b"RIFFfake".to_vec(), None)
        },
        |_, _| {},
    )
    .await
    .unwrap_err();

    assert!(matches!(err, CoreError::Http(_)), "{err:?}");
    assert!(err.to_string().starts_with("応答がなくタイムアウトしました（"), "{err}");
    assert!(is_retryable(&err));
    assert_eq!(attempts.load(Ordering::SeqCst), 3, "打ち切りは再試行の対象");
}

/// いま誰も待ち受けていないポート（接続すると拒否される）。
fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

// [ERR-005]
#[tokio::test]
async fn connection_failure_explains_what_to_check() {
    let base = format!("http://127.0.0.1:{}", closed_port());

    let transcriber = OpenAiTranscriber::new("sk-test", "gpt-4o-transcribe", "ja").with_base_url(&base);
    let err = transcriber.transcribe(vec![0; 4], None).await.unwrap_err();
    assert!(
        err.to_string()
            .starts_with("接続できませんでした。インターネット接続やプロキシの設定を確認してください（"),
        "{err}"
    );
    assert!(is_retryable(&err));

    // Claude の呼び出しでも同じ文になる
    let client = ClaudeClient::new("sk-ant-test", "claude-opus-5-5").with_base_url(&base);
    let err = stream(&client).await.unwrap_err();
    assert!(err.to_string().starts_with("接続できませんでした。"), "{err}");
}

// [ERR-005]
#[tokio::test]
async fn broken_response_is_reported_as_a_communication_failure() {
    // 接続は受け付けるが、応答を返さずに切るサーバー
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0u8; 1024];
        let _ = stream.read(&mut buffer);
        // 何も返さずに閉じる
    });

    let transcriber = OpenAiTranscriber::new("sk-test", "gpt-4o-transcribe", "ja")
        .with_base_url(&format!("http://127.0.0.1:{port}"));
    let err = transcriber.transcribe(vec![0; 4], None).await.unwrap_err();
    server.join().unwrap();

    assert!(matches!(err, CoreError::Http(_)), "{err:?}");
    assert!(err.to_string().starts_with("通信に失敗しました（"), "{err}");
}

// ---- Claude の応答の途中で届くエラー ----

/// 1 回目はエラーのイベントだけを返し、2 回目からは文章を返す。
struct ErrorEventOnce {
    kind: &'static str,
    calls: AtomicUsize,
}

impl Respond for ErrorEventOnce {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            sse(&[
                json!({"type": "message_start", "message": {"model": "claude-opus-5-5"}}),
                json!({"type": "error", "error": {"type": self.kind, "message": "詳細"}}),
            ])
        } else {
            sse(&text_events("OK"))
        }
    }
}

// [ERR-007]
#[tokio::test]
async fn stream_error_events_map_to_http_statuses() {
    // （エラーの種類, 同じに扱う HTTP の番号, 再試行するか）
    let cases = [
        ("overloaded_error", 529, true),
        ("rate_limit_error", 429, true),
        ("api_error", 500, true),
        ("invalid_request_error", 400, false),
        ("authentication_error", 400, false),
    ];
    for (kind, status, retried) in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/messages"))
            .respond_with(ErrorEventOnce {
                kind,
                calls: AtomicUsize::new(0),
            })
            .mount(&server)
            .await;
        let client = ClaudeClient::new("sk-ant-test", "claude-opus-5-5").with_base_url(&server.uri());
        let result = stream(&client).await;
        let calls = server.received_requests().await.unwrap().len();

        if retried {
            // 文章が届く前なので、試し直して成功する
            assert_eq!(result.unwrap(), "OK", "{kind}");
            assert_eq!(calls, 2, "{kind}");
        } else {
            let err = result.unwrap_err();
            assert!(
                matches!(err, CoreError::Api { service: "Anthropic", status: s, .. } if s == status),
                "{kind}: {err:?}"
            );
            assert_eq!(err.to_string(), "Anthropic がエラーを返しました（HTTP 400）: 詳細", "{kind}");
            assert_eq!(calls, 1, "{kind}: 再試行しない");
        }
    }

    // 再試行を使い切ったときは、対応する番号のエラーになる
    for (kind, status) in [("overloaded_error", 529), ("rate_limit_error", 429), ("api_error", 500)] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(sse(&[json!({"type": "error", "error": {"type": kind, "message": "詳細"}})]))
            .mount(&server)
            .await;
        let client = ClaudeClient::new("sk-ant-test", "claude-opus-5-5").with_base_url(&server.uri());
        let err = stream(&client).await.unwrap_err();
        assert!(
            matches!(err, CoreError::Api { status: s, .. } if s == status),
            "{kind}: {err:?}"
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 3, "{kind}");
    }
}

// ---- 送り先と API キー ----

// [SEC-004][SEC-005]
#[test]
fn api_endpoints_are_fixed_https_hosts() {
    assert_eq!(kiroscribe_core::transcribe::OPENAI_BASE_URL, "https://api.openai.com/v1");
    assert_eq!(kiroscribe_core::claude::ANTHROPIC_BASE_URL, "https://api.anthropic.com/v1");
    assert_eq!(kiroscribe_core::help::RAW_BASE_URL, "https://raw.githubusercontent.com");
    assert_eq!(kiroscribe_core::help::API_BASE_URL, "https://api.github.com");
}

// [SEC-004]
#[tokio::test]
async fn each_key_goes_only_to_its_own_service_and_github_gets_none() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"text": "こんにちは"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/messages"))
        .respond_with(sse(&text_events("OK")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/s-tabana/kiroscribe/main/docs/guide.md"))
        .respond_with(ResponseTemplate::new(200).set_body_string("# 使い方"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/s-tabana/kiroscribe/releases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"tag_name": "v0.3.0", "name": "0.3.0", "body": "本文", "published_at": null, "html_url": null}
        ])))
        .mount(&server)
        .await;

    OpenAiTranscriber::new("sk-openai-KEY", "gpt-4o-transcribe", "ja")
        .with_base_url(&server.uri())
        .transcribe(vec![0; 4], None)
        .await
        .unwrap();
    stream(&ClaudeClient::new("sk-ant-KEY", "claude-opus-5-5").with_base_url(&server.uri()))
        .await
        .unwrap();
    let help = HelpClient::new().with_base_urls(&server.uri(), &server.uri());
    help.fetch_guide().await.unwrap();
    help.fetch_releases().await.unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 4);
    for request in &requests {
        let everything = format!(
            "{} {:?} {}",
            request.url,
            request.headers,
            String::from_utf8_lossy(&request.body)
        );
        let has_openai = everything.contains("sk-openai-KEY");
        let has_anthropic = everything.contains("sk-ant-KEY");
        match request.url.path() {
            "/audio/transcriptions" => {
                assert!(has_openai && !has_anthropic);
                assert!(request.headers.get("x-api-key").is_none());
            }
            "/messages" => {
                assert!(has_anthropic && !has_openai);
                assert!(request.headers.get("authorization").is_none());
            }
            // GitHub への問い合わせには、どちらのキーも付けない
            _ => {
                assert!(!has_openai && !has_anthropic, "{}", request.url);
                assert!(request.headers.get("authorization").is_none());
                assert!(request.headers.get("x-api-key").is_none());
            }
        }
    }
}

#[derive(Default)]
struct Collect {
    transcripts: Mutex<Vec<String>>,
    levels: Mutex<Vec<(Speaker, f32)>>,
    errors: Mutex<Vec<String>>,
}

impl RecorderSink for Collect {
    fn on_transcript(&self, _: Speaker, _: i64, _: i64, text: String) {
        self.transcripts.lock().unwrap().push(text);
    }
    fn on_level(&self, speaker: Speaker, level: f32) {
        self.levels.lock().unwrap().push((speaker, level));
    }
    fn on_error(&self, message: String) {
        self.errors.lock().unwrap().push(message);
    }
}

fn job(started_at: i64) -> TranscribeJob {
    TranscribeJob {
        speaker: Speaker::Me,
        started_at,
        ended_at: started_at + 1_000,
        wav: b"RIFFfake".to_vec(),
    }
}

const LEAKED_KEY: &str = "sk-proj-KIROSCRIBETESTabcd";

fn assert_no_key(text: &str) {
    assert!(!text.contains("sk-proj"), "キーの断片が出ています: {text}");
    assert!(!text.contains("TESTabcd"), "キーの断片が出ています: {text}");
}

// [SEC-014]
#[tokio::test]
async fn key_fragments_in_api_errors_do_not_reach_messages_or_logs() {
    let server = MockServer::start().await;
    let openai_message = format!("Incorrect API key provided: {LEAKED_KEY}. You can find your API key at https://platform.openai.com/account/api-keys.");
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": {"message": openai_message, "type": "invalid_request_error", "code": "invalid_api_key"}
        })))
        .mount(&server)
        .await;

    // 文字起こしのエラー: 表示する文（Display）にも、ログに出る形（Debug）にも出ない
    let transcriber = Arc::new(
        OpenAiTranscriber::new(LEAKED_KEY, "gpt-4o-transcribe", "ja").with_base_url(&server.uri()),
    );
    let err = transcriber.transcribe(vec![0; 4], None).await.unwrap_err();
    assert_no_key(&err.to_string());
    assert_no_key(&format!("{err:?}"));
    assert_eq!(
        err.to_string(),
        "OpenAI の API キーが正しくないか、権限がありません（HTTP 401: Incorrect API key provided: （キーは表示しません）. \
You can find your API key at https://platform.openai.com/account/api-keys.）。設定画面でキーを確認してください"
    );

    // 録音中の知らせ（画面の上部に出る文）にも出ない
    let sink = Arc::new(Collect::default());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tx.send(job(1_700_000_000_000)).unwrap();
    drop(tx);
    run_transcription_worker(rx, transcriber, sink.clone(), fast()).await;
    let errors = sink.errors.lock().unwrap().clone();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].starts_with("文字起こしを停止しました。API キーを確認してください（"), "{}", errors[0]);
    assert_no_key(&errors[0]);

    // Claude: HTTP のエラーの本文と、応答の途中で届くエラー
    let claude_http = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string(format!("invalid key {LEAKED_KEY}")))
        .mount(&claude_http)
        .await;
    let claude_stream = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(&[json!({
            "type": "error",
            "error": {"type": "invalid_request_error", "message": format!("bad credential {LEAKED_KEY}")}
        })]))
        .mount(&claude_stream)
        .await;
    for server in [&claude_http, &claude_stream] {
        let client = ClaudeClient::new(LEAKED_KEY, "claude-opus-5-5").with_base_url(&server.uri());
        let err = stream(&client).await.unwrap_err();
        assert!(err.to_string().contains("（キーは表示しません）"), "{err}");
        assert_no_key(&err.to_string());
        assert_no_key(&format!("{err:?}"));
    }
}

// ---- 録音のパイプライン ----

/// 呼ばれるたびに、決まった長さの文を返す偽の文字起こし。渡された文脈を記録する。
struct LongTexts {
    prompts: Mutex<Vec<Option<String>>>,
}

impl Transcribe for LongTexts {
    fn transcribe(&self, _: Vec<u8>, prompt: Option<String>) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            let mut prompts = self.prompts.lock().unwrap();
            prompts.push(prompt);
            // 1 回目は「あ」× 150、2 回目は「い」× 150 …
            let letter = ['あ', 'い', 'う'][prompts.len() - 1];
            Ok(letter.to_string().repeat(150))
        })
    }
}

// [TRN-010]
#[tokio::test]
async fn context_passed_to_the_next_utterance_is_the_last_200_chars() {
    let transcriber = Arc::new(LongTexts {
        prompts: Mutex::new(Vec::new()),
    });
    let sink = Arc::new(Collect::default());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    for i in 0..3 {
        tx.send(job(1_700_000_000_000 + i * 2_000)).unwrap();
    }
    drop(tx);
    run_transcription_worker(rx, transcriber.clone(), sink.clone(), fast()).await;

    let prompts = transcriber.prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), 3);
    assert_eq!(prompts[0], None, "最初の発話では送らない");
    assert_eq!(prompts[1].as_deref(), Some("あ".repeat(150).as_str()));
    // 2 件ぶん（300 文字）のうち、末尾の 200 文字だけ
    let expected = format!("{}{}", "あ".repeat(50), "い".repeat(150));
    assert_eq!(prompts[2].as_deref(), Some(expected.as_str()));
    assert_eq!(sink.transcripts.lock().unwrap().len(), 3);
}

// [TRN-013][ERR-006][MIN-016]
#[test]
fn default_retry_policy_is_four_attempts_starting_at_one_second() {
    let policy = RetryPolicy::default();
    assert_eq!(policy.max_attempts, 4);
    assert_eq!(policy.base_delay, Duration::from_secs(1));
    assert_eq!(policy.max_delay, Duration::from_secs(8));
    // 4 回試すあいだに待つのは 1 秒・2 秒・4 秒
    let waits: Vec<u64> = (0..policy.max_attempts - 1)
        .map(|failure| policy.delay_after(failure).as_secs())
        .collect();
    assert_eq!(waits, [1, 2, 4]);
}

const RATE: u32 = 16_000;

/// 話し声に近い信号（200ms 発声 + 100ms 弱音の繰り返し）
fn speechlike(seconds: f32) -> Vec<f32> {
    (0..(RATE as f32 * seconds) as usize)
        .map(|i| {
            let s = 0.3 * (2.0 * std::f32::consts::PI * 200.0 * i as f32 / RATE as f32).sin();
            if (i as u64 * 1000 / RATE as u64) % 300 < 200 { s } else { s * 0.02 }
        })
        .collect()
}

// [REC-012]
#[test]
fn input_level_is_reported_about_every_100ms_and_drops_to_zero_on_pause_and_stop() {
    let paused = Arc::new(AtomicBool::new(false));
    let sink = Arc::new(Collect::default());
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let mut processor = SourceProcessor::new(Speaker::Them, RATE, paused.clone(), tx, sink.clone());

    let mut clock = 1_700_000_000_000_i64;
    let mut play = |processor: &mut SourceProcessor, audio: &[f32]| {
        for block in audio.chunks(RATE as usize / 100) {
            clock += 10;
            processor.process(&AudioFrame {
                captured_at: clock,
                samples: block.to_vec(),
            });
        }
    };

    play(&mut processor, &speechlike(1.0));
    {
        let levels = sink.levels.lock().unwrap();
        // 1 秒で約 10 回
        assert!((9..=11).contains(&levels.len()), "{}", levels.len());
        assert!(levels.iter().all(|(speaker, _)| *speaker == Speaker::Them));
        assert!(levels.iter().any(|(_, level)| *level > 0.1));
    }

    // 一時停止: 0 を 1 回だけ知らせ、その間の音声では知らせない
    paused.store(true, Ordering::SeqCst);
    play(&mut processor, &speechlike(0.5));
    {
        let mut levels = sink.levels.lock().unwrap();
        assert_eq!(levels.last(), Some(&(Speaker::Them, 0.0)));
        assert_eq!(levels.iter().filter(|(_, level)| *level == 0.0).count(), 1);
        levels.clear();
    }

    // 再開すると、また知らせる
    paused.store(false, Ordering::SeqCst);
    play(&mut processor, &speechlike(0.5));
    assert!(sink.levels.lock().unwrap().iter().any(|(_, level)| *level > 0.1));

    // 停止: 最後に 0 を知らせる
    processor.finish();
    assert_eq!(sink.levels.lock().unwrap().last(), Some(&(Speaker::Them, 0.0)));
}

// ---- API キーの誤りで文字起こしを止めるとき ----

/// いつも HTTP 401 を返す偽の文字起こし。呼ばれた回数を数える。
#[derive(Default)]
struct Unauthorized {
    calls: AtomicUsize,
}

impl Transcribe for Unauthorized {
    fn transcribe(&self, _: Vec<u8>, _: Option<String>) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(CoreError::Api {
                service: "OpenAI",
                status: 401,
                message: "Incorrect API key provided".into(),
            })
        })
    }
}

/// キーの誤りの知らせを、ほかの知らせと分けて受け取る側（録音を止める役目のアプリに当たる）。
#[derive(Default)]
struct AuthAware {
    auth_errors: Mutex<Vec<String>>,
    other_errors: Mutex<Vec<String>>,
}

impl RecorderSink for AuthAware {
    fn on_transcript(&self, _: Speaker, _: i64, _: i64, _: String) {}
    fn on_level(&self, _: Speaker, _: f32) {}
    fn on_error(&self, message: String) {
        self.other_errors.lock().unwrap().push(message);
    }
    fn on_auth_error(&self, message: String) {
        self.auth_errors.lock().unwrap().push(message);
    }
}

// [TRN-014]
#[tokio::test]
async fn a_wrong_api_key_is_reported_once_per_recording_across_both_sources() {
    use kiroscribe_core::recorder::run_transcription_worker_shared;

    let transcriber = Arc::new(Unauthorized::default());
    let sink = Arc::new(AuthAware::default());
    // 同じ録音の、自分（マイク）と相手（PC の音声）のワーカー
    let auth_failed = Arc::new(AtomicBool::new(false));
    let mut workers = Vec::new();
    for speaker in [Speaker::Me, Speaker::Them] {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        for i in 0..3 {
            tx.send(TranscribeJob {
                speaker,
                ..job(1_700_000_000_000 + i * 2_000)
            })
            .unwrap();
        }
        drop(tx);
        workers.push(tokio::spawn(run_transcription_worker_shared(
            rx,
            transcriber.clone(),
            sink.clone(),
            fast(),
            auth_failed.clone(),
        )));
    }
    for worker in workers {
        worker.await.unwrap();
    }

    // 知らせは、音源ごとではなく 1 回だけ
    let auth_errors = sink.auth_errors.lock().unwrap().clone();
    assert_eq!(auth_errors.len(), 1, "{auth_errors:?}");
    assert!(auth_errors[0].starts_with("文字起こしを停止しました。API キーを確認してください（"));
    assert!(sink.other_errors.lock().unwrap().is_empty());
    // 再試行せず、以降の発話は送らない（先に送り始めていた分を除く）
    assert!(transcriber.calls.load(Ordering::SeqCst) <= 2);
    assert!(auth_failed.load(Ordering::SeqCst));
}

// [TRN-014]
#[tokio::test]
async fn a_wrong_api_key_is_shown_like_other_problems_unless_the_receiver_handles_it() {
    // on_auth_error を持たない受け取り側では、ほかの問題と同じ知らせになる
    let transcriber = Arc::new(Unauthorized::default());
    let sink = Arc::new(Collect::default());
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    for i in 0..3 {
        tx.send(job(1_700_000_000_000 + i * 2_000)).unwrap();
    }
    drop(tx);
    run_transcription_worker(rx, transcriber.clone(), sink.clone(), fast()).await;

    assert_eq!(sink.errors.lock().unwrap().len(), 1);
    assert_eq!(transcriber.calls.load(Ordering::SeqCst), 1);
}
