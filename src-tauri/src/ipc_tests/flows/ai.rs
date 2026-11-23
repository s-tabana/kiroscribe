//! 議事録の作成と AI チャットのコマンド。

use std::time::Duration;

use kiroscribe_core::transcript::Speaker;
use serde_json::{json, Value};

use super::super::fixture::{wait_until, ClaudeReply, Harness, ANTHROPIC_KEY};

fn set_reply(h: &Harness, reply: ClaudeReply) {
    *h.fixture.api.claude.lock().unwrap() = reply;
}

/// 議事録は、先頭のタイトル行を除いて保存する。Anthropic へ送る内容も確かめる。
// [MIN-006][MIN-007][MIN-014][MIN-017]
#[test]
fn minutes_are_saved_without_the_title_line() {
    let h = Harness::new();
    let untitled = h.note("", "- リリース日を決める");
    set_reply(&h, ClaudeReply::text(&["# 模擬のタイトル\n\n", "## 決定事項\n", "- リリースは 12/5\n"]));
    let note = h.ok("enhance_note", json!({ "noteId": untitled }));
    assert_eq!(note["enhancedNotes"], "## 決定事項\n- リリースは 12/5");
    assert_eq!(note["title"], "模擬のタイトル", "タイトルが空なら、議事録の見出しを付ける");
    assert_eq!(h.ok("get_note", json!({ "id": untitled })), note, "返したノートが保存されている");
    let deltas: Vec<Value> = h.events("enhance-delta");
    assert_eq!(deltas.len(), 3);
    assert!(deltas.iter().all(|delta| delta["noteId"] == untitled.as_str()));

    // Anthropic へ送った内容
    let sent = h.requests("/anthropic/v1/messages");
    let request = sent.last().unwrap();
    assert_eq!(request.method.as_str(), "POST");
    assert_eq!(request.headers.get("x-api-key").unwrap(), ANTHROPIC_KEY);
    assert!(request.headers.get("anthropic-version").is_some());
    assert!(request.headers.get("authorization").is_none());
    let body = h.claude_requests().pop().unwrap();
    assert_eq!(body["model"], h.settings().claude_model.as_str());
    assert_eq!(body["stream"], true);
    assert_eq!(body["max_tokens"], 64_000);
    assert_eq!(body["output_config"]["effort"], "medium");
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    assert!(body["messages"][0]["content"].as_str().unwrap().contains("- リリース日を決める"));
    assert_eq!(kiroscribe_core::claude::REQUEST_TIMEOUT, Duration::from_secs(600));

    // すでにタイトルがあるノートでは、タイトルを変えない
    let titled = h.note("週次定例", "- メモ");
    let note = h.ok("enhance_note", json!({ "noteId": titled }));
    assert_eq!(note["title"], "週次定例");
    assert_eq!(note["enhancedNotes"], "## 決定事項\n- リリースは 12/5");

    // 応答に本文がなければ、エラーにして保存しない（すでにあった議事録は残る）
    set_reply(&h, ClaudeReply::text(&["# タイトルだけ\n"]));
    let err = h.call("enhance_note", json!({ "noteId": titled })).unwrap_err();
    assert_eq!(err, "議事録を作成できませんでした（応答が空でした）。もう一度お試しください");
    assert_eq!(h.ok("get_note", json!({ "id": titled })), note);
    let empty = h.note("", "- メモ");
    assert!(h.call("enhance_note", json!({ "noteId": empty })).is_err());
    let unchanged = h.ok("get_note", json!({ "id": empty }));
    assert_eq!((&unchanged["title"], &unchanged["enhancedNotes"]), (&json!(""), &Value::Null));

    // 途中で失敗したら、何も保存せず、理由を知らせる（すでにあった議事録は残る）
    set_reply(&h, ClaudeReply::status(500));
    let before = h.claude_requests().len();
    let err = h.call("enhance_note", json!({ "noteId": titled })).unwrap_err();
    assert_eq!(err, "Anthropic がエラーを返しました（HTTP 500）: Internal server error");
    assert_eq!(h.claude_requests().len() - before, 4, "一時的な失敗は、最初の 1 回を含めて 4 回まで試す");
    assert_eq!(h.ok("get_note", json!({ "id": titled })), note);
    set_reply(&h, ClaudeReply::status(401));
    let err = h.call("enhance_note", json!({ "noteId": titled })).unwrap_err();
    assert!(err.starts_with("Anthropic の API キーが正しくないか、権限がありません（HTTP 401"), "{err}");
    assert_eq!(h.ok("get_note", json!({ "id": titled })), note);
}

/// 同じノートの議事録は同時に 1 つだけ。中止すると止まり、何も保存しない。
// [MIN-005][MIN-008]
#[test]
fn minutes_run_one_at_a_time_and_can_be_cancelled() {
    let h = Harness::new();
    let note = h.note("中止のテスト", "- メモ");
    let other = h.note("別のノート", "- メモ");
    set_reply(&h, ClaudeReply::text(&["# タイトル\n\n本文"]).after(Duration::from_millis(2500)));

    let running = h.spawn("enhance_note", json!({ "noteId": note }));
    wait_until("作成の開始", || (h.claude_requests().len() == 1).then_some(()));
    // 作成している間は、同じノートではもう始められない。別のノートや AI チャットは始められる
    let busy = h.call("enhance_note", json!({ "noteId": note })).unwrap_err();
    assert_eq!(busy, "このノートの議事録を作成中です");
    let elsewhere = h.spawn("enhance_note", json!({ "noteId": other }));
    let chat = h.spawn("send_chat_message", json!({ "noteId": note, "content": "決まったことは？" }));
    wait_until("別のノートの作成と AI チャットの開始", || (h.claude_requests().len() == 3).then_some(()));

    // 中止: エラーの文は「中止しました」（画面は、これを知らせとして出さない）。何も保存しない
    assert_eq!(h.ok("cancel_ai", json!({ "noteId": note, "kind": "enhance" })), Value::Null);
    assert_eq!(running.join().unwrap().unwrap_err(), "中止しました");
    assert_eq!(h.ok("get_note", json!({ "id": note }))["enhancedNotes"], Value::Null);
    // ほかの作成は、中止されずに終わる
    assert_eq!(elsewhere.join().unwrap().unwrap()["enhancedNotes"], "本文");
    assert_eq!(chat.join().unwrap().unwrap()["role"], "assistant");

    // 終わったあと（中止・失敗のあとも）は、また始められる
    set_reply(&h, ClaudeReply::text(&["# タイトル\n\n作り直した本文"]));
    assert_eq!(h.ok("enhance_note", json!({ "noteId": note }))["enhancedNotes"], "作り直した本文");
    // 動いていないものの中止は、何も起こさない
    assert_eq!(h.ok("cancel_ai", json!({ "noteId": note, "kind": "enhance" })), Value::Null);
}

/// AI チャット: ノートの内容を根拠として渡し、質問と回答を履歴に残す。
// [CHT-008][CHT-009]
#[test]
fn chat_sends_the_note_as_context_and_keeps_the_history() {
    let h = Harness::new();
    let note = h.note("週次定例", "- リリース日を決める");
    {
        let state = h.state();
        let db = state.db();
        db.save_enhanced_notes(&note, "## 決定事項\n- リリースは 12/5", None).unwrap();
        db.add_segment(&note, Speaker::Them, 1_000, 2_000, "来週の火曜はどうですか").unwrap();
    }

    // 空の質問は送らない
    for empty in ["", "  \n "] {
        let err = h.call("send_chat_message", json!({ "noteId": note, "content": empty })).unwrap_err();
        assert_eq!(err, "質問を入力してください");
    }
    assert!(h.claude_requests().is_empty());

    set_reply(&h, ClaudeReply::text(&["リリースは ", "12/5 です。\n"]));
    let answer = h.ok("send_chat_message", json!({ "noteId": note, "content": " 決まったことは？ " }));
    assert_eq!((&answer["role"], &answer["content"]), (&json!("assistant"), &json!("リリースは 12/5 です。")));
    assert_eq!(h.events("chat-delta").len(), 2);
    let history = h.ok("list_chat_messages", json!({ "noteId": note }));
    let turns: Vec<(&str, &str)> = history
        .as_array()
        .unwrap()
        .iter()
        .map(|message| (message["role"].as_str().unwrap(), message["content"].as_str().unwrap()))
        .collect();
    assert_eq!(turns, [("user", "決まったことは？"), ("assistant", "リリースは 12/5 です。")]);

    // AI へ送った内容: ノートのタイトル・メモ・議事録・文字起こしが根拠（システムプロンプト）に入る
    let body = h.claude_requests().pop().unwrap();
    let system = body["system"][0]["text"].as_str().unwrap();
    for part in ["週次定例", "- リリース日を決める", "リリースは 12/5", "来週の火曜はどうですか"] {
        assert!(system.contains(part), "根拠に「{part}」がない");
    }
    assert_eq!(body["messages"], json!([{ "role": "user", "content": "決まったことは？" }]));
    assert_eq!(body["output_config"]["effort"], "low");
    assert_eq!(body["max_tokens"], 16_000);
    assert_eq!(body["model"], h.settings().claude_model.as_str());

    // 2 つ目の質問には、それまでの会話が付く
    h.ok("send_chat_message", json!({ "noteId": note, "content": "担当は？" }));
    let body = h.claude_requests().pop().unwrap();
    assert_eq!(body["messages"].as_array().unwrap().len(), 3);

    // 回答が空だったら、エラーにして回答は保存しない
    set_reply(&h, ClaudeReply::text(&["  \n"]));
    let err = h.call("send_chat_message", json!({ "noteId": note, "content": "もう一度" })).unwrap_err();
    assert_eq!(err, "回答が空でした。もう一度お試しください");
    let history = h.ok("list_chat_messages", json!({ "noteId": note }));
    assert!(history.as_array().unwrap().iter().all(|message| message["content"] != ""));
    assert_eq!(history.as_array().unwrap().iter().filter(|message| message["role"] == "assistant").count(), 2);
}

/// AI チャットの回答は中止でき、中止した回答は保存しない。
// [CHT-006]
#[test]
fn a_chat_answer_can_be_cancelled() {
    let h = Harness::new();
    let note = h.note("週次定例", "- メモ");
    set_reply(&h, ClaudeReply::text(&["途中までの回答"]).after(Duration::from_millis(2500)));
    let running = h.spawn("send_chat_message", json!({ "noteId": note, "content": "決まったことは？" }));
    wait_until("回答の作成の開始", || (h.claude_requests().len() == 1).then_some(()));
    let busy = h.call("send_chat_message", json!({ "noteId": note, "content": "もう 1 つ" })).unwrap_err();
    assert_eq!(busy, "回答を作成中です");

    h.ok("cancel_ai", json!({ "noteId": note, "kind": "chat" }));
    assert_eq!(running.join().unwrap().unwrap_err(), "中止しました");
    let history = h.ok("list_chat_messages", json!({ "noteId": note }));
    assert!(
        history.as_array().unwrap().iter().all(|message| message["role"] != "assistant"),
        "中止した回答が保存されている: {history}"
    );
}
