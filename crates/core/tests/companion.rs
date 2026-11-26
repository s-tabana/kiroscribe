//! 会議の相棒の結合テスト: 判定と選択を模擬の Anthropic に送り、送り方・打ち切り・再試行を確かめる。
//! 探し先（指定したフォルダと過去のノート）を、実際の索引のファイルで同時に探すことも確かめる。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kiroscribe_core::claude::{ClaudeClient, ANTHROPIC_VERSION};
use kiroscribe_core::companion::{ai, candidates, Assistant, CurrentNote, JudgeInput, SourceKind};
use kiroscribe_core::search::store::NoteDoc;
use kiroscribe_core::search::{IndexState, Indexer, SearchFolder, INDEX_FILE};
use kiroscribe_core::transcript::Speaker;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const KEY: &str = "sk-ant-test";

fn reply(answer: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "type": "message",
        "stop_reason": "end_turn",
        "content": [{ "type": "text", "text": answer.to_string() }],
    }))
}

/// 届いた要求を残し、決まった応答を順に返す。
struct Recorder {
    requests: Arc<std::sync::Mutex<Vec<Request>>>,
    responses: Vec<ResponseTemplate>,
    count: AtomicUsize,
}

impl Respond for Recorder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        self.requests.lock().unwrap().push(request.clone());
        let i = self.count.fetch_add(1, Ordering::SeqCst).min(self.responses.len() - 1);
        self.responses[i].clone()
    }
}

async fn mock(responses: Vec<ResponseTemplate>) -> (MockServer, Arc<std::sync::Mutex<Vec<Request>>>) {
    let server = MockServer::start().await;
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/messages"))
        .respond_with(Recorder { requests: requests.clone(), responses, count: AtomicUsize::new(0) })
        .mount(&server)
        .await;
    (server, requests)
}

fn client(server: &MockServer) -> ClaudeClient {
    ClaudeClient::new(KEY, "claude-haiku-5-5").with_base_url(&server.uri())
}

fn input() -> JudgeInput {
    JudgeInput {
        note_title: "週次定例".into(),
        speaker: Speaker::Them,
        text: "経費精算のマニュアルってどこにありますか".into(),
        previous: vec![(Speaker::Me, "では次の議題です".into())],
    }
}

fn judged() -> Value {
    json!({ "requested": true, "target": "経費精算のマニュアル", "terms": ["経費精算", "マニュアル", "精算"], "period": "なし" })
}

// [CMP-015][CMP-006]
#[tokio::test]
async fn judge_is_sent_without_streaming_in_a_fixed_form() {
    let (server, requests) = mock(vec![reply(judged())]).await;
    let judgement = client(&server).judge(&input()).await.unwrap();
    assert!(judgement.requested);
    assert_eq!(judgement.target, "経費精算のマニュアル");
    assert_eq!(judgement.terms, ["経費精算", "マニュアル", "精算"]);

    let request = &requests.lock().unwrap()[0];
    assert_eq!(request.headers.get("x-api-key").unwrap(), KEY);
    assert_eq!(request.headers.get("anthropic-version").unwrap(), ANTHROPIC_VERSION);
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["model"], "claude-haiku-5-5", "相棒のモデル");
    assert_eq!(body.get("stream"), None, "ストリーミングは使わない");
    assert_eq!(body["max_tokens"], 2_000);
    assert_eq!(body["output_config"]["effort"], "low");
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    assert_eq!(body["output_config"]["format"]["schema"]["required"], json!(["requested", "target", "terms", "period"]));
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    let user = body["messages"][0]["content"].as_str().unwrap();
    assert!(user.contains("ノートの題名: 週次定例"));
    assert!(user.contains("- 自分: では次の議題です"));
    assert!(user.contains("判定する発言: 相手: 経費精算のマニュアルってどこにありますか"));
}

// [CMP-015][CMP-013]
#[tokio::test]
async fn select_is_sent_with_candidates_in_the_send_form() {
    let answer = json!({ "choice": 1, "passage": "申請は月末まで。", "clues": ["経費精算"] });
    let (server, requests) = mock(vec![reply(answer)]).await;
    let candidate = candidates::Candidate {
        kind: SourceKind::Folder,
        title: "経費精算のマニュアル".into(),
        location: "C:\\Users\\tabana\\共有資料\\経理\\経費精算.docx".into(),
        send_location: "共有資料\\経理\\経費精算.docx".into(),
        excerpt: Some("申請は月末まで。".into()),
        part: None,
        no_content: false,
        path: None,
        note_id: None,
    };
    let selection = client(&server).select(&input().text, "経費精算のマニュアル", &[candidate.clone()]).await.unwrap();
    assert_eq!(selection.choice, 1);
    let found = ai::resolve(&selection, &[candidate], &input().text).unwrap();
    assert_eq!(found.passage.as_deref(), Some("申請は月末まで。"));

    let body: Value = serde_json::from_slice(&requests.lock().unwrap()[0].body).unwrap();
    assert_eq!(body["max_tokens"], 4_000);
    assert_eq!(body["output_config"]["format"]["schema"]["required"], json!(["choice", "passage", "clues"]));
    let user = body["messages"][0]["content"].as_str().unwrap();
    assert!(user.contains("場所: 共有資料\\経理\\経費精算.docx"));
    assert!(user.contains("抜粋: 申請は月末まで。"));
    assert!(!user.contains("C:\\Users"), "PC の中のフルパスは送らない");
}

// [CMP-016]
#[tokio::test]
async fn temporary_failures_are_retried_once() {
    let (server, requests) = mock(vec![ResponseTemplate::new(529), reply(judged())]).await;
    assert!(client(&server).judge(&input()).await.unwrap().requested);
    assert_eq!(requests.lock().unwrap().len(), 2);

    let (server, requests) = mock(vec![ResponseTemplate::new(500)]).await;
    assert!(client(&server).judge(&input()).await.is_err());
    assert_eq!(requests.lock().unwrap().len(), 2, "再試行は 1 回だけ");

    let (server, requests) = mock(vec![ResponseTemplate::new(401)]).await;
    let err = client(&server).judge(&input()).await.unwrap_err();
    assert!(matches!(err, kiroscribe_core::CoreError::Api { status: 401, .. }), "{err}");
    assert_eq!(requests.lock().unwrap().len(), 1, "キーの誤りは再試行しない");
}

// [CMP-016]
#[tokio::test]
async fn judge_gives_up_after_ten_seconds() {
    assert_eq!(ai::JUDGE_TIMEOUT, Duration::from_secs(10));
    assert_eq!(ai::SELECT_TIMEOUT, Duration::from_secs(15));
    let (server, _) = mock(vec![reply(judged()).set_delay(Duration::from_secs(30))]).await;
    let start = Instant::now();
    let err = client(&server).judge(&input()).await.unwrap_err();
    let elapsed = start.elapsed();
    assert!(elapsed >= Duration::from_secs(10) && elapsed < Duration::from_secs(13), "{elapsed:?}");
    assert!(err.to_string().contains("10 秒以内に応答がありませんでした"), "{err}");
}

// [CMP-019]
#[tokio::test]
async fn refused_selection_is_none() {
    let refused = ResponseTemplate::new(200).set_body_json(json!({ "stop_reason": "refusal", "content": [] }));
    let (server, _) = mock(vec![refused]).await;
    assert_eq!(client(&server).judge(&input()).await.unwrap().requested, false);
}

// [CMP-012][IDX-017]
#[test]
fn folders_and_past_notes_are_searched_together() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("共有資料");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("経費精算.txt"), "経費精算の申請は月末まで").unwrap();
    let indexer = Indexer::spawn(dir.path().join(INDEX_FILE), |_| {});
    let folders = vec![SearchFolder::register(&folder).unwrap()];
    indexer.enable(folders.clone());
    let note = |id: &str, title: &str, minutes: &str| NoteDoc {
        note_id: id.into(),
        title: title.into(),
        created_at: 1,
        memo: String::new(),
        minutes: minutes.into(),
        transcript: String::new(),
    };
    indexer.set_notes(Some(vec![note("past", "経理の定例", "## 決定事項\n- 経費精算は新システムで行う"), note("now", "録音中", "経費精算")]));
    let start = Instant::now();
    loop {
        let (hits, notes) = indexer.search(&["経費精算".to_string()], Some("now"), 8);
        if indexer.status().state == IndexState::Ready && hits.len() == 1 && notes.len() == 1 {
            let current = CurrentNote { id: "now".into(), title: "録音中".into(), created_at: 2 };
            let merged = candidates::merge(hits, notes, &folders, None, Some(&current), 3);
            let kinds: Vec<SourceKind> = merged.iter().map(|c| c.kind).collect();
            assert!(kinds.contains(&SourceKind::Folder) && kinds.contains(&SourceKind::Note), "{kinds:?}");
            assert!(merged.iter().all(|c| c.note_id.as_deref() != Some("now")), "録音中のノートは除く");
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "索引ができませんでした");
        std::thread::sleep(Duration::from_millis(50));
    }

    // 過去のノートの索引をやめると、ノートは探さない
    indexer.set_notes(None);
    let start = Instant::now();
    while !indexer.search(&["経費精算".to_string()], None, 8).1.is_empty() {
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(50));
    }
}
