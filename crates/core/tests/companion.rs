//! 会議の相棒の結合テスト: 判定と選択を模擬の Anthropic に送り、送り方・打ち切り・再試行を確かめる。
//! 探し先（指定したフォルダと過去のノート）を、実際の索引のファイルで同時に探すことも確かめる。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kiroscribe_core::claude::{ClaudeClient, ANTHROPIC_VERSION};
use kiroscribe_core::companion::{ai, candidates, Assistant, CurrentNote, JudgeInput, SourceKind};
use kiroscribe_core::search::store::NoteDoc;
use kiroscribe_core::search::{IndexState, Indexer, NoteSource, SearchFolder, INDEX_FILE};
use kiroscribe_core::transcript::Speaker;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const KEY: &str = "sk-ant-test";

/// テスト用のノートの原本。
#[derive(Default)]
struct MemoryNotes(std::sync::Mutex<Vec<NoteDoc>>);

impl MemoryNotes {
    fn with(notes: Vec<NoteDoc>) -> Arc<Self> {
        Arc::new(Self(std::sync::Mutex::new(notes)))
    }

    fn put(&self, note: NoteDoc) {
        let mut notes = self.0.lock().unwrap();
        notes.retain(|n| n.note_id != note.note_id);
        notes.push(note);
    }
}

impl NoteSource for MemoryNotes {
    fn note_ids(&self) -> kiroscribe_core::Result<Vec<String>> {
        Ok(self.0.lock().unwrap().iter().map(|n| n.note_id.clone()).collect())
    }

    fn note(&self, note_id: &str) -> kiroscribe_core::Result<Option<NoteDoc>> {
        Ok(self.0.lock().unwrap().iter().find(|n| n.note_id == note_id).cloned())
    }
}

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
        sent_excerpt: None,
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
    indexer.set_note_source(MemoryNotes::with(vec![note("past", "経理の定例", "## 決定事項\n- 経費精算は新システムで行う"), note("now", "録音中", "経費精算")]));
    indexer.set_notes(true);
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
    indexer.set_notes(false);
    let start = Instant::now();
    while !indexer.search(&["経費精算".to_string()], None, 8).1.is_empty() {
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// 削除したノートと録音中のノートは、すぐに検索から外す。索引に書けずに消せなかったノートは、消し直す。
/// 消す前に読んだ古い中身が、あとから届いても入れない。
// [NOTE-015][IDX-016][IDX-017]
#[test]
fn deleted_and_recording_notes_leave_search_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(INDEX_FILE);
    let intervals = kiroscribe_core::search::Intervals { purge_retry: Duration::from_millis(200), ..Default::default() };
    let indexer = Indexer::spawn_with_intervals(path.clone(), kiroscribe_core::search::walk::Limits::default(), intervals, |_| {});
    let note = |id: &str| NoteDoc { note_id: id.into(), title: "定例".into(), created_at: 1, memo: "予算".into(), minutes: String::new(), transcript: String::new() };
    let source = MemoryNotes::with(vec![note("a"), note("b")]);
    indexer.set_note_source(source.clone());
    indexer.enable(Vec::new());
    indexer.set_notes(true);
    let found = || {
        let mut ids: Vec<String> = indexer.search(&["予算".to_string()], None, 8).1.into_iter().map(|h| h.note_id).collect();
        ids.sort();
        ids
    };
    let start = Instant::now();
    while found() != ["a", "b"] {
        assert!(start.elapsed() < Duration::from_secs(20), "{:?}", found());
        std::thread::sleep(Duration::from_millis(20));
    }

    indexer.set_recording_note(Some("b"));
    assert_eq!(found(), ["a"], "録音中のノートは、呼び出し側が除かなくても出さない");
    indexer.set_recording_note(None);

    // 索引への書き込みをふさいだまま削除する
    let blocker = rusqlite::Connection::open(&path).unwrap();
    blocker.execute_batch("BEGIN EXCLUSIVE").unwrap();
    indexer.note_removed("a");
    assert_eq!(found(), ["b"], "索引から消し終える前に、検索から外す");
    std::thread::sleep(Duration::from_secs(7));
    blocker.execute_batch("ROLLBACK").unwrap();
    let stored = || kiroscribe_core::search::SearchIndex::open_existing(&path).unwrap().note_ids().unwrap();
    let start = Instant::now();
    while stored().contains(&"a".to_string()) {
        assert!(start.elapsed() < Duration::from_secs(20), "消せなかったノートを消し直す");
        std::thread::sleep(Duration::from_millis(50));
    }

    // 削除したノートの保存の知らせが、あとから届いても入れない
    indexer.note_saved("a");
    source.put(NoteDoc { memo: "予算と経費".into(), ..note("b") });
    indexer.note_saved("b");
    let start = Instant::now();
    while indexer.search(&["経費".to_string()], None, 8).1.is_empty() {
        assert!(start.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!stored().contains(&"a".to_string()));
    assert_eq!(found(), ["b"]);
}

/// 「前回」のノートは、一致した語の少ないノートでも、件数を絞る前に上げる（索引の検索の上限で落とさない）。
// [IDX-018][CMP-012]
#[test]
fn previous_meeting_survives_the_candidate_limit() {
    let dir = tempfile::tempdir().unwrap();
    let indexer = Indexer::spawn(dir.path().join(INDEX_FILE), |_| {});
    indexer.enable(Vec::new());
    let source = MemoryNotes::with(Vec::new());
    indexer.set_note_source(source.clone());
    let note = |id: &str, title: &str, created_at: i64, memo: &str| NoteDoc {
        note_id: id.into(),
        title: title.into(),
        created_at,
        memo: memo.into(),
        minutes: String::new(),
        transcript: String::new(),
    };
    for i in 0..9 {
        source.put(note(&format!("n{i}"), "採用面談", 1_000 + i, "予算 経費 精算"));
    }
    source.put(note("prev", "週次定例（11月20日）", 500, "予算"));
    indexer.set_notes(true);
    let current = CurrentNote { id: "now".into(), title: "週次定例（11月27日）".into(), created_at: 10_000 };
    let terms = vec!["予算".to_string(), "経費".to_string(), "精算".to_string()];
    let start = Instant::now();
    while indexer.search(&terms, None, 20).1.len() < 10 {
        assert!(start.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(20));
    }
    let found = candidates::search(&indexer, &terms, Some(candidates::Period::Previous), &[], Some(&current), 20_000);
    assert_eq!(found.len(), candidates::MAX_CANDIDATES);
    assert_eq!(found[0].note_id.as_deref(), Some("prev"), "題名の似た前回のノートが最も上");
    assert_eq!(found[1].note_id.as_deref(), Some("n8"), "ほかは新しい順");
}
