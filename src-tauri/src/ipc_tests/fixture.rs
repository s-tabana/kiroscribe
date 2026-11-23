//! コマンド層の結合テストで使う土台: 外部の API の模擬サーバー、差し替えの設定、模擬のアプリ。
//!
//! 差し替えの設定（`kiroscribe_core::e2e`）はプロセスに 1 つしか持てないので、模擬サーバーと作業フォルダも
//! プロセスに 1 つだけ用意し（[`fixture`]）、テストは 1 件ずつ順に動かす（[`Harness`] が順番待ちをする）。
//! 外部の API は手元の模擬サーバー（wiremock）、音声は模擬の取り込み（発話に似た合成音）に差し替わる。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use kiroscribe_core::e2e as core;
use kiroscribe_core::meetings::MeetingEvent;
use kiroscribe_core::secrets::{MemorySecretStore, Provider, SecretStore};
use kiroscribe_core::settings::Settings;
use kiroscribe_core::Database;
use serde_json::{json, Value};
use tauri::test::MockRuntime;
use tauri::{Listener, Manager, WebviewWindow, WebviewWindowBuilder};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::{frontend, invoke, mock_app};
use crate::background::POPUP_LABEL;
use crate::state::AppState;

pub(super) const WAIT: Duration = Duration::from_secs(30);
pub(super) const OPENAI_KEY: &str = "sk-test-openai";
pub(super) const ANTHROPIC_KEY: &str = "sk-ant-test";
/// 文字起こしの模擬サーバーが返す文
pub(super) const TRANSCRIPT: &str = "来週の予定を確認します。";

/// Claude の模擬の応答。
#[derive(Debug, Clone)]
pub(super) struct ClaudeReply {
    /// 200 ならストリーミングで `chunks` を返す。それ以外は HTTP のエラー
    pub status: u16,
    pub chunks: Vec<String>,
    /// 応答を返し始めるまでの時間（作成中の状態を保つために使う）
    pub delay: Duration,
}

impl ClaudeReply {
    pub fn text(chunks: &[&str]) -> Self {
        Self {
            status: 200,
            chunks: chunks.iter().map(|chunk| chunk.to_string()).collect(),
            delay: Duration::ZERO,
        }
    }

    pub fn status(status: u16) -> Self {
        Self {
            status,
            chunks: Vec::new(),
            delay: Duration::ZERO,
        }
    }

    pub fn after(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
}

/// 模擬サーバーの応答の決め方。テストの始めに既定へ戻す。
pub(super) struct Api {
    /// 文字起こし API が返す HTTP の状態（`None` なら成功）
    pub transcription_status: Mutex<Option<u16>>,
    /// 文字起こし API が応答するまでの時間
    pub transcription_delay: Mutex<Duration>,
    pub claude: Mutex<ClaudeReply>,
}

impl Api {
    fn new() -> Self {
        Self {
            transcription_status: Mutex::new(None),
            transcription_delay: Mutex::new(Duration::ZERO),
            claude: Mutex::new(Self::default_claude()),
        }
    }

    fn default_claude() -> ClaudeReply {
        ClaudeReply::text(&["# 週次定例\n\n## 決定事項\n", "- リリースは 12/5\n"])
    }

    fn reset(&self) {
        *self.transcription_status.lock().unwrap() = None;
        *self.transcription_delay.lock().unwrap() = Duration::ZERO;
        *self.claude.lock().unwrap() = Self::default_claude();
    }
}

struct Transcription(Arc<Api>);

impl Respond for Transcription {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        assert!(String::from_utf8_lossy(&request.body).contains("RIFF"), "WAV が送られていません");
        let delay = *self.0.transcription_delay.lock().unwrap();
        let status = *self.0.transcription_status.lock().unwrap();
        let response = match status {
            Some(status @ (401 | 403)) => ResponseTemplate::new(status)
                .set_body_json(json!({ "error": { "message": "Incorrect API key provided" } })),
            Some(status) => {
                ResponseTemplate::new(status).set_body_json(json!({ "error": { "message": "模擬サーバーのエラー" } }))
            }
            None => ResponseTemplate::new(200).set_body_json(json!({ "text": TRANSCRIPT })),
        };
        response.set_delay(delay)
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

fn claude_stream(chunks: &[String]) -> ResponseTemplate {
    let mut events = vec![
        json!({"type": "message_start", "message": {"model": "claude-opus-5-5"}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
    ];
    for chunk in chunks {
        events.push(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": chunk}}));
    }
    events.push(json!({"type": "content_block_stop", "index": 0}));
    events.push(json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}));
    events.push(json!({"type": "message_stop"}));
    sse(&events)
}

struct Claude(Arc<Api>);

impl Respond for Claude {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let reply = self.0.claude.lock().unwrap().clone();
        let response = match reply.status {
            200 => claude_stream(&reply.chunks),
            401 => ResponseTemplate::new(401)
                .set_body_json(json!({ "type": "error", "error": { "type": "authentication_error", "message": "invalid x-api-key" } })),
            status => ResponseTemplate::new(status)
                .set_body_json(json!({ "type": "error", "error": { "type": "api_error", "message": "Internal server error" } })),
        };
        response.set_delay(reply.delay)
    }
}

/// 模擬の配布元が配る更新のファイル。
fn update_package() -> Vec<u8> {
    vec![7u8; 4096]
}

/// [`update_package`] を、テスト用の鍵（`TEST_UPDATE_PUBKEY`）で、バージョン 99.0.0 として署名したもの
/// （`tauri signer sign --app-version 99.0.0` の出力）。
const PACKAGE_SIGNATURE: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVRcVFHYkZ6R3BNczZ3Y3J5Uk5jR2Ntdkk2cVVNU2hUSHRJOVRhOHhVdFFxeUFCVjJUNjk3dzdrR2dMcTlUUVVpZFJNSzJ0ZHdoaGZ1Q2dkYloxM3V5bmxQYS94dVdoakFnPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzk1NDA1MTcwCWZpbGU6cGFja2FnZS5iaW4JdmVyc2lvbjo5OS4wLjAKbE1mSFMwcUJ1OStiTTg4WWRpUDA2dVBLaU9zMlRkbEs2aWNTbFlwdW5Fa3V2Q29HdTlYcXlySkpiSTdIcmZSUU5LV3RqWGdra0cxSXRVeEp3c0cwRHc9PQo=";

/// 更新の配布元（latest.json）。`package` は、どの環境向けにも同じものを返す。
fn update_manifest(signature: &str, package_url: &str) -> Value {
    let platform = json!({ "signature": signature, "url": package_url });
    let mut platforms = serde_json::Map::new();
    for os in ["linux", "windows", "darwin"] {
        for arch in ["x86_64", "aarch64", "i686", "armv7"] {
            platforms.insert(format!("{os}-{arch}"), platform.clone());
        }
    }
    json!({
        "version": "99.0.0",
        "notes": "- 模擬サーバーの更新",
        "pub_date": "2026-12-01T00:00:00Z",
        "platforms": platforms,
    })
}

async fn start_server(api: Arc<Api>) -> MockServer {
    let server = MockServer::start().await;
    let uri = server.uri();
    Mock::given(method("POST"))
        .and(path("/openai/v1/audio/transcriptions"))
        .respond_with(Transcription(api.clone()))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/anthropic/v1/messages"))
        .respond_with(Claude(api))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/raw/s-tabana/kiroscribe/main/docs/guide.md"))
        .respond_with(ResponseTemplate::new(200).set_body_string("# Kiroscribe の使い方\n\n模擬サーバーの内容です。\n"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/repos/s-tabana/kiroscribe/releases"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
            "tag_name": "v99.0.0",
            "name": "Kiroscribe 99.0.0",
            "published_at": "2026-12-01T00:00:00Z",
            "body": "- 模擬サーバーのリリースノート",
            "html_url": "https://github.com/s-tabana/kiroscribe/releases/tag/v99.0.0",
            "draft": false,
            "prerelease": false
        }])))
        .mount(&server)
        .await;
    // 更新の配布元。配るファイルは、インストーラではないただのバイト列（署名が正しくても、インストールの段階で止まる）
    //   /update/              署名が正しくない（ダウンロードのあと、署名の確認で止まる）
    //   /update-signed/       署名が正しい
    //   /update-signed-slow/  署名が正しく、ダウンロードに時間がかかる
    //   /update-none/         まだリリースがない（404）
    for (name, signature, delay) in [
        ("update", "bm90IGEgc2lnbmF0dXJl", Duration::ZERO),
        ("update-signed", PACKAGE_SIGNATURE, Duration::ZERO),
        ("update-signed-slow", PACKAGE_SIGNATURE, Duration::from_millis(1500)),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/{name}/latest.json")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(update_manifest(signature, &format!("{uri}/{name}/package"))),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/{name}/package")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(update_package()).set_delay(delay))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/update-none/latest.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    server
}

/// テストの実行ファイルの隣（target の中）に作る作業フォルダ。毎回作り直す。
fn work_dir() -> PathBuf {
    let exe = std::env::current_exe().expect("テストの実行ファイルの場所が分かりません");
    let dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("target の場所が分かりません")
        .join("ipc-contract-work");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// このテストのプロセス全体で 1 つだけ用意するもの（差し替えの設定はプロセスに 1 つしか持てないため）。
pub(super) struct Fixture {
    pub dir: PathBuf,
    pub server_uri: String,
    pub api: Arc<Api>,
    runtime: tokio::runtime::Runtime,
    server: MockServer,
}

impl Fixture {
    /// 模擬サーバーがこれまでに受け取ったリクエストの数。
    pub fn request_count(&self) -> usize {
        self.all_requests().len()
    }

    fn all_requests(&self) -> Vec<Request> {
        self.runtime
            .block_on(self.server.received_requests())
            .expect("模擬サーバーがリクエストを記録していません")
    }

    /// `skip` 件目より後に受け取ったリクエストのうち、パスが `suffix` で終わるもの。
    pub fn requests_after(&self, skip: usize, suffix: &str) -> Vec<Request> {
        self.all_requests()
            .into_iter()
            .skip(skip)
            .filter(|request| request.url.path().ends_with(suffix))
            .collect()
    }

    /// 作業フォルダの中のファイル（`meeting.txt`・`mic-error.txt` など、実行中に読まれるもの）。
    pub fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

pub(super) fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let api = Arc::new(Api::new());
        let server = runtime.block_on(start_server(api.clone()));
        let server_uri = server.uri();
        let dir = work_dir();
        let config = core::Config {
            openai_base_url: Some(format!("{server_uri}/openai/v1")),
            anthropic_base_url: Some(format!("{server_uri}/anthropic/v1")),
            github_raw_base_url: Some(format!("{server_uri}/raw")),
            github_api_base_url: Some(format!("{server_uri}/api")),
            update_endpoint: Some(format!("{server_uri}/update/latest.json")),
            update_pubkey: None,
            audio: core::AudioConfig {
                mic: Some("speech".into()),
                system: Some("speech".into()),
            },
            timing: core::Timing {
                meeting_poll_ms: Some(50),
                meeting_start_confirm_ms: Some(200),
                meeting_end_grace_ms: Some(400),
                update_first_check_ms: Some(50),
                retry_base_delay_ms: Some(10),
            },
        };
        assert!(
            core::install(dir.clone(), config),
            "差し替えの設定を、ほかのテストが先に読み込みました（設定を使うテストは、最初に fixture() を呼ぶ）"
        );
        Fixture {
            dir,
            server_uri,
            api,
            runtime,
            server,
        }
    })
}

/// 土台を使うテストを 1 件ずつ順に動かすための順番待ち。持っているあいだ、ほかのテストは土台を使わない。
/// 取ったときに、模擬サーバーの応答と、作業フォルダの中の実行中に読まれるファイルを既定へ戻す。
pub(super) fn exclusive() -> (&'static Fixture, MutexGuard<'static, ()>) {
    static LOCK: Mutex<()> = Mutex::new(());
    let fixture = fixture();
    // 先のテストが失敗（パニック）していても、次のテストは動かす
    let guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    fixture.api.reset();
    for name in ["meeting.txt", "mic-error.txt", "opened-urls.log"] {
        let _ = std::fs::remove_file(fixture.file(name));
    }
    (fixture, guard)
}

pub(super) fn wait_until<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let started = Instant::now();
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(started.elapsed() < WAIT, "待っても起きませんでした: {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub(super) type Events = Arc<Mutex<Vec<(String, Value)>>>;

pub(super) fn events_named(events: &Events, name: &str) -> Vec<Value> {
    events
        .lock()
        .unwrap()
        .iter()
        .filter(|(event, _)| event == name)
        .map(|(_, payload)| payload.clone())
        .collect()
}

/// 画面が待ち受けるすべてのイベントを記録する。
pub(super) fn record_events(app: &tauri::App<MockRuntime>) -> Events {
    let events: Events = Arc::default();
    for binding in frontend().events {
        let (name, events) = (binding.event.clone(), events.clone());
        app.listen_any(binding.event.clone(), move |event| {
            let payload = serde_json::from_str(event.payload()).expect("イベントの内容が JSON ではありません");
            events.lock().unwrap().push((name.clone(), payload));
        });
    }
    events
}

/// テスト 1 件ぶんの模擬のアプリ: 製品と同じコマンドと状態を、模擬のランタイムの上に用意したもの。
/// データベースはメモリの上、API キーはメモリの上のストアに置く（テストごとに別）。
pub(super) struct Harness {
    pub fixture: &'static Fixture,
    pub app: tauri::App<MockRuntime>,
    pub window: WebviewWindow<MockRuntime>,
    events: Events,
    first_request: usize,
    // 最後に手放す（アプリを片づけ終えるまで、次のテストを始めない）
    _serial: MutexGuard<'static, ()>,
}

impl Harness {
    /// API キーが 2 つとも保存済みのアプリ。
    pub fn new() -> Self {
        let harness = Self::without_keys();
        harness.set_key(Provider::OpenAi, OPENAI_KEY);
        harness.set_key(Provider::Anthropic, ANTHROPIC_KEY);
        harness
    }

    /// API キーをまだ保存していないアプリ。
    pub fn without_keys() -> Self {
        Self::with_endpoint("/update/latest.json")
    }

    /// 更新の配布元（模擬サーバーの中のパス）を指定して作る。
    pub fn with_endpoint(update_path: &str) -> Self {
        let (fixture, serial) = exclusive();
        let (app, window) = mock_app(&format!("{}{update_path}", fixture.server_uri));
        app.manage(AppState::new(
            Database::open_in_memory().unwrap(),
            Box::new(MemorySecretStore::default()),
        ));
        // ポップアップのウィンドウは先に作っておく。模擬のランタイムはモニターの情報を返せないので、
        // ウィンドウを作る経路（表示位置の計算）は通せない。ここでは内容の差し替え（イベント）だけを確かめる
        WebviewWindowBuilder::new(&app, POPUP_LABEL, Default::default())
            .build()
            .expect("模擬のポップアップを作れません");
        let events = record_events(&app);
        Harness {
            fixture,
            first_request: fixture.request_count(),
            app,
            window,
            events,
            _serial: serial,
        }
    }

    pub fn state(&self) -> tauri::State<'_, AppState> {
        self.app.state::<AppState>()
    }

    pub fn secrets(&self) -> &dyn SecretStore {
        // AppState はアプリが持ち続けるので、このテストのあいだ有効
        let state: &AppState = self.app.state::<AppState>().inner();
        state.secrets.as_ref()
    }

    pub fn set_key(&self, provider: Provider, value: &str) {
        kiroscribe_core::secrets::set_api_key(self.secrets(), provider, value).unwrap();
    }

    /// 画面の `invoke(command, args)` と同じ経路でコマンドを呼ぶ。失敗の理由は 1 つの文字列で返る。
    pub fn call(&self, command: &str, args: Value) -> Result<Value, String> {
        invoke(&self.window, command, args).map_err(|err| match err {
            Value::String(message) => message,
            other => panic!("{command}: 失敗の理由が文字列ではありません: {other}"),
        })
    }

    pub fn ok(&self, command: &str, args: Value) -> Value {
        self.call(command, args)
            .unwrap_or_else(|err| panic!("{command} が失敗しました: {err}"))
    }

    /// 別のスレッドでコマンドを呼ぶ（終わるまで時間のかかるコマンドの途中で、ほかの操作をするため）。
    pub fn spawn(&self, command: &'static str, args: Value) -> std::thread::JoinHandle<Result<Value, String>> {
        let window = self.window.clone();
        std::thread::spawn(move || {
            invoke(&window, command, args).map_err(|err| match err {
                Value::String(message) => message,
                other => other.to_string(),
            })
        })
    }

    /// タイトルとメモを付けたノートを作って、ID を返す。
    pub fn note(&self, title: &str, user_notes: &str) -> String {
        let id = self.ok("create_note", json!({}))["id"].as_str().unwrap().to_string();
        let patch = json!({ "title": title, "userNotes": user_notes });
        self.ok("update_note", json!({ "id": id, "patch": patch }));
        id
    }

    pub fn settings(&self) -> Settings {
        self.state().db().load_settings().unwrap()
    }

    /// 設定を変えて、画面の「設定を保存」と同じコマンドで保存する。
    pub fn save_settings(&self, change: impl FnOnce(&mut Settings)) -> Value {
        let mut settings = self.settings();
        change(&mut settings);
        self.ok("save_settings", json!({ "settings": settings }))
    }

    pub fn events(&self, name: &str) -> Vec<Value> {
        events_named(&self.events, name)
    }

    /// これまでに記録したイベントを捨てる（ここから先に送られたものだけを調べたいとき）。
    pub fn clear_events(&self) {
        self.events.lock().unwrap().clear();
    }

    pub fn recording_state(&self) -> Value {
        self.ok("get_recording_status", json!({}))["state"].clone()
    }

    pub fn popup(&self) -> Value {
        self.ok("get_popup_state", json!({}))
    }

    pub fn segments(&self, note_id: &str) -> Vec<Value> {
        self.ok("list_segments", json!({ "noteId": note_id })).as_array().unwrap().clone()
    }

    /// このテストが始まってから、模擬サーバーが受け取ったリクエストの数（宛先を問わない）。
    pub fn request_count(&self) -> usize {
        self.fixture.request_count() - self.first_request
    }

    /// このテストが始まってから、模擬サーバーが受け取ったリクエストのうち、パスが `suffix` で終わるもの。
    pub fn requests(&self, suffix: &str) -> Vec<Request> {
        self.fixture.requests_after(self.first_request, suffix)
    }

    /// このテストが始まってから、模擬サーバーが受け取った文字起こしのリクエスト。
    pub fn transcriptions(&self) -> Vec<Request> {
        self.fixture.requests_after(self.first_request, "/audio/transcriptions")
    }

    /// このテストが始まってから、模擬サーバーが受け取った Claude へのリクエスト（本文の JSON）。
    pub fn claude_requests(&self) -> Vec<Value> {
        self.fixture
            .requests_after(self.first_request, "/anthropic/v1/messages")
            .iter()
            .map(|request| serde_json::from_slice(&request.body).expect("Claude へのリクエストが JSON ではありません"))
            .collect()
    }

    /// 録音を始めて、発話が 1 つ以上文字起こしされるまで待つ。
    pub fn record_until_transcribed(&self, note_id: &str) {
        self.ok("start_recording", json!({ "noteId": note_id }));
        wait_until("最初の発話の文字起こし", || (!self.segments(note_id).is_empty()).then_some(()));
    }

    /// 会議の検出の結果（開始・終了）を、検出のスレッドと同じ入口に渡す。
    pub fn meeting(&self, event: MeetingEvent) {
        let handle = self.app.handle().clone();
        tauri::async_runtime::block_on(async move { crate::meeting_watch::handle(&handle, event).await });
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // 録音が残っていたら止める（音声のスレッドと文字起こしを、次のテストに持ち越さない）
        let handle = self.app.handle().clone();
        tauri::async_runtime::block_on(async move {
            crate::recording::stop_now(&handle).await;
        });
    }
}
