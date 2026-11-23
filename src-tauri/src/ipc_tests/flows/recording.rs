//! 録音のコマンド（開始・一時停止・再開・停止）と、設定・API キーとの関わり。

use std::time::Duration;

use kiroscribe_core::secrets::Provider;
use kiroscribe_core::time::now_ms;
use serde_json::{json, Value};
use wiremock::Request;

use super::super::fixture::{wait_until, Harness, ANTHROPIC_KEY, OPENAI_KEY, TRANSCRIPT};

/// multipart/form-data の中の、文字の項目の値。
fn field(request: &Request, name: &str) -> Option<String> {
    let body = String::from_utf8_lossy(&request.body);
    let start = body.find(&format!("name=\"{name}\"\r\n\r\n"))? + name.len() + 11;
    Some(body[start..].split("\r\n").next().unwrap_or_default().to_string())
}

fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request.headers.get(name).and_then(|value| value.to_str().ok())
}

/// 録音の一連の操作と、してはいけない操作の断り方。
// [REC-001][REC-003][REC-004][REC-005][REC-006][REC-007][REC-008][REC-009][REC-010][NOTE-016]
#[test]
fn recording_lifecycle_through_the_commands() {
    let h = Harness::new();
    let note = h.note("週次定例", "");
    let other = h.note("別のノート", "");

    // 録音していないとき: 停止はエラーにせず今の状態を返す。一時停止・再開は断る
    assert_eq!(h.ok("stop_recording", json!({}))["state"], "idle");
    assert_eq!(h.call("pause_recording", json!({})).unwrap_err(), "録音していません");
    assert_eq!(h.call("resume_recording", json!({})).unwrap_err(), "録音していません");
    // 存在しないノートでは始められない
    let missing = h.call("start_recording", json!({ "noteId": "no-such-note" })).unwrap_err();
    assert_eq!(missing, "ノートが見つかりません: no-such-note");
    assert_eq!(h.recording_state(), "idle");

    // 開始: 自分と相手の音源を取り込み、録音中になる
    let before_start = now_ms();
    let started = h.ok("start_recording", json!({ "noteId": note }));
    assert_eq!(started["state"], "recording");
    assert_eq!(started["noteId"], note.as_str());
    assert_eq!(started["sources"], json!(["me", "them"]));
    assert_eq!(started["elapsedMs"], 0);
    let running_since = started["runningSince"].as_i64().expect("録音中は、区間の開始時刻がある");
    assert!(running_since >= before_start);
    assert_eq!(h.events("recording-status").last().unwrap()["state"], "recording");

    // 同じノートでもう一度開始しても、二重には始まらない。別のノートでは始められない
    let again = h.ok("start_recording", json!({ "noteId": note }));
    assert_eq!(again["runningSince"], running_since, "録音を始め直していない");
    assert_eq!(again["sources"], json!(["me", "them"]));
    let refused = h.call("start_recording", json!({ "noteId": other })).unwrap_err();
    assert_eq!(refused, "別のノートで録音中です。先にそちらの録音を停止してください");
    // 録音中のノートは削除できない
    let refused = h.call("delete_note", json!({ "id": note })).unwrap_err();
    assert_eq!(refused, "録音中のノートは削除できません。先に録音を停止してください");
    assert_eq!(h.ok("get_note", json!({ "id": note }))["id"], note.as_str());

    wait_until("最初の発話の文字起こし", || (!h.segments(&note).is_empty()).then_some(()));
    assert_eq!(h.segments(&note)[0]["text"], TRANSCRIPT);

    // 一時停止: 経過時間が止まり、一時停止中の音声はどこにも送らない
    let paused = h.ok("pause_recording", json!({}));
    assert_eq!(paused["state"], "paused");
    assert_eq!(paused["runningSince"], Value::Null);
    let elapsed = paused["elapsedMs"].as_i64().unwrap();
    assert!(elapsed > 0, "一時停止までの録音時間が残る");
    let paused_again = h.ok("pause_recording", json!({}));
    assert_eq!((&paused_again["state"], &paused_again["elapsedMs"]), (&json!("paused"), &json!(elapsed)));
    std::thread::sleep(Duration::from_millis(1500)); // 一時停止した時点までの発話が送られ終わるまで
    let sent_before = h.transcriptions().len();
    std::thread::sleep(Duration::from_millis(3500)); // 模擬の音声の発話 1 回ぶんより長い
    assert_eq!(h.transcriptions().len(), sent_before, "一時停止中の音声を送っている");

    // 再開: 経過時間は一時停止していた時間を除いて続きから。再開後の発話には、再開後の時刻が付く
    let resumed_at = now_ms();
    let resumed = h.ok("resume_recording", json!({}));
    assert_eq!(resumed["state"], "recording");
    assert_eq!(resumed["elapsedMs"], elapsed);
    assert!(resumed["runningSince"].as_i64().unwrap() >= resumed_at);
    let resumed_again = h.ok("resume_recording", json!({}));
    assert_eq!(resumed_again["runningSince"], resumed["runningSince"], "録音中の再開では何も変わらない");
    let known = h.segments(&note).len();
    let fresh = wait_until("再開後の自分の発話", || {
        h.segments(&note).into_iter().skip(known).find(|segment| segment["speaker"] == "me")
    });
    assert!(
        fresh["startedAt"].as_i64().unwrap() >= resumed_at - 400,
        "再開後の発話に、一時停止中の時刻が付いている: {fresh}"
    );

    // 停止: 話している途中の発話も文字起こしに回し、終わるまで「仕上げ中」になる
    // （自分の発話が区切られた直後から 1.2 秒後は、次の発話の途中にあたる）
    std::thread::sleep(Duration::from_millis(1200));
    *h.fixture.api.transcription_delay.lock().unwrap() = Duration::from_millis(1200);
    let sent_before_stop = h.transcriptions().len();
    let stopping = h.spawn("stop_recording", json!({}));
    wait_until("仕上げ中の状態", || (h.recording_state() == "finishing").then_some(()));
    let finishing = h.ok("get_recording_status", json!({}));
    assert_eq!(finishing["noteId"], note.as_str());
    assert_eq!(finishing["runningSince"], Value::Null);
    // 仕上げ中は、次の録音を始められない。一時停止・再開もできない。ノートも消せない
    let refused = h.call("start_recording", json!({ "noteId": other })).unwrap_err();
    assert_eq!(refused, "前の録音の文字起こしを仕上げています。少し待ってから開始してください");
    assert_eq!(h.call("pause_recording", json!({})).unwrap_err(), "録音していません");
    assert_eq!(h.call("resume_recording", json!({})).unwrap_err(), "録音していません");
    let refused = h.call("delete_note", json!({ "id": note })).unwrap_err();
    assert_eq!(refused, "録音中のノートは削除できません。先に録音を停止してください");

    let stopped = stopping.join().unwrap().expect("停止できる");
    assert_eq!(stopped["state"], "idle");
    assert_eq!(stopped["noteId"], Value::Null);
    assert!(
        h.transcriptions().len() > sent_before_stop,
        "話している途中だった発話が、文字起こしに回っていない"
    );
    let states: Vec<Value> = h.events("recording-status").iter().map(|status| status["state"].clone()).collect();
    assert_eq!(&states[states.len() - 2..], [json!("finishing"), json!("idle")], "{states:?}");

    // 止めたあとは、ほかのノートで録音でき、止めたノートは消せる
    assert_eq!(h.ok("delete_note", json!({ "id": note })), Value::Null);
    *h.fixture.api.transcription_delay.lock().unwrap() = Duration::ZERO;
    assert_eq!(h.ok("start_recording", json!({ "noteId": other }))["state"], "recording");
}

/// 保存した設定は、次に始める録音と、次の議事録・AI チャットから使われる（始まっている録音は変えない）。
// [REC-017][SET-005]
#[test]
fn saved_settings_apply_from_the_next_recording_and_ai_request() {
    let h = Harness::new();
    let note = h.note("設定のテスト", "- メモ");
    h.save_settings(|settings| {
        settings.capture_system_audio = false;
        settings.language = "en".into();
        settings.transcription_model = "whisper-1".into();
    });

    // 相手の声を取り込まない設定なら、マイクだけ
    let started = h.ok("start_recording", json!({ "noteId": note }));
    assert_eq!(started["sources"], json!(["me"]));
    wait_until("最初の発話の文字起こし", || (!h.segments(&note).is_empty()).then_some(()));
    let first = &h.transcriptions()[0];
    assert_eq!(field(first, "model").as_deref(), Some("whisper-1"));
    assert_eq!(field(first, "language").as_deref(), Some("en"));

    // 録音の途中で設定を変えても、その録音には効かない
    h.save_settings(|settings| {
        settings.capture_system_audio = true;
        settings.transcription_model = "gpt-4o-mini-transcribe".into();
        settings.claude_model = "claude-haiku-5-5".into();
    });
    let sent = h.transcriptions().len();
    wait_until("設定を変えたあとの発話", || (h.transcriptions().len() > sent).then_some(()));
    assert_eq!(field(h.transcriptions().last().unwrap(), "model").as_deref(), Some("whisper-1"));
    assert_eq!(h.ok("get_recording_status", json!({}))["sources"], json!(["me"]));
    h.ok("stop_recording", json!({}));
    assert!(h.segments(&note).iter().all(|segment| segment["speaker"] == "me"));

    // 次の録音からは、新しい設定
    let sent = h.transcriptions().len();
    assert_eq!(h.ok("start_recording", json!({ "noteId": note }))["sources"], json!(["me", "them"]));
    wait_until("次の録音の発話", || (h.transcriptions().len() > sent).then_some(()));
    assert_eq!(field(h.transcriptions().last().unwrap(), "model").as_deref(), Some("gpt-4o-mini-transcribe"));
    h.ok("stop_recording", json!({}));

    // 議事録と AI チャットも、保存した Claude のモデルを使う
    h.ok("enhance_note", json!({ "noteId": note }));
    h.ok("send_chat_message", json!({ "noteId": note, "content": "決まったことは？" }));
    let requests = h.claude_requests();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request["model"] == "claude-haiku-5-5"), "{requests:?}");
}

/// API キーがなければ始まらず、保存すればアプリを起動し直さなくても使われる。キーの値は画面に返さない。
// [REC-002][MIN-003][CHT-004][KEY-001][KEY-006][SEC-001]
#[test]
fn api_keys_are_required_and_used_as_soon_as_they_are_saved() {
    let h = Harness::without_keys();
    let note = h.note("キーのテスト", "- メモ");

    // 未設定: 録音・議事録・AI チャットは始まらず、理由を知らせる
    let refused = h.call("start_recording", json!({ "noteId": note })).unwrap_err();
    assert_eq!(refused, "API キーが設定されていません（OpenAI）。設定画面から入力してください");
    assert_eq!(h.recording_state(), "idle");
    let refused = h.call("enhance_note", json!({ "noteId": note })).unwrap_err();
    assert_eq!(refused, "API キーが設定されていません（Anthropic）。設定画面から入力してください");
    assert_eq!(h.ok("get_note", json!({ "id": note }))["enhancedNotes"], Value::Null);
    let refused = h
        .call("send_chat_message", json!({ "noteId": note, "content": "決まったことは？" }))
        .unwrap_err();
    assert_eq!(refused, "API キーが設定されていません（Anthropic）。設定画面から入力してください");
    assert_eq!(h.ok("list_chat_messages", json!({ "noteId": note })), json!([]), "送れなかった質問は履歴に残さない");
    assert!(h.transcriptions().is_empty() && h.claude_requests().is_empty());

    // 保存: 前後の空白を除いて、その場で保存する。画面に返すのは「設定済みかどうか」と保存先の説明だけ
    let status = h.ok("set_api_key", json!({ "provider": "openai", "value": format!("  {OPENAI_KEY}\n") }));
    assert_eq!((&status["openai"], &status["anthropic"]), (&json!(true), &json!(false)));
    let mut keys: Vec<&String> = status.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["anthropic", "openai", "storage"]);
    assert!(status["storage"].is_string());
    let status = h.ok("set_api_key", json!({ "provider": "anthropic", "value": ANTHROPIC_KEY }));
    assert_eq!(status["anthropic"], true);
    for shown in [status, h.ok("get_api_key_status", json!({})), h.ok("get_settings", json!({}))] {
        let text = shown.to_string();
        assert!(!text.contains(OPENAI_KEY) && !text.contains(ANTHROPIC_KEY), "キーの値を画面に返している: {text}");
    }
    assert_eq!(h.secrets().get(Provider::OpenAi).unwrap().as_deref(), Some(OPENAI_KEY));

    // 起動し直さなくても、次の録音・議事録・AI チャットから使われる
    h.record_until_transcribed(&note);
    h.ok("stop_recording", json!({}));
    let sent = h.transcriptions();
    assert_eq!(header(&sent[0], "authorization"), Some(format!("Bearer {OPENAI_KEY}").as_str()));
    h.ok("enhance_note", json!({ "noteId": note }));
    h.ok("send_chat_message", json!({ "noteId": note, "content": "決まったことは？" }));
    let sent = h.requests("/anthropic/v1/messages");
    assert_eq!(sent.len(), 2);
    assert!(sent.iter().all(|request| header(request, "x-api-key") == Some(ANTHROPIC_KEY)));

    // 削除すると、次の操作からまた断る
    let status = h.ok("set_api_key", json!({ "provider": "openai", "value": "" }));
    assert_eq!(status["openai"], false);
    assert!(h.call("start_recording", json!({ "noteId": note })).unwrap_err().contains("（OpenAI）"));
}

/// 文字起こしを保存できなかったときは、理由を知らせる。
// [TRN-017]
#[test]
fn a_transcript_that_cannot_be_saved_is_reported() {
    let h = Harness::new();
    let note = h.note("消えるノート", "");
    h.ok("start_recording", json!({ "noteId": note }));
    // 録音中にノートがなくなった（コマンドは録音中の削除を断るので、データベースを直接書き換える）
    h.state().db().delete_note(&note).unwrap();
    let error = wait_until("保存の失敗の知らせ", || h.events("recording-error").into_iter().next());
    assert_eq!(error["noteId"], note.as_str());
    let message = error["message"].as_str().unwrap();
    assert!(message.starts_with("文字起こしを保存できませんでした: "), "{message}");
    assert!(h.events("transcript-segment").is_empty(), "保存できなかった発話を画面に送っている");
    assert_eq!(h.ok("stop_recording", json!({}))["state"], "idle");
}

/// API キーが誤っている（401・403）ときは、文字起こしだけでなく録音も止め、知らせは 1 回だけ出す。
// [TRN-014]
#[test]
fn a_rejected_api_key_stops_the_recording_with_a_single_notice() {
    for status in [401, 403] {
        let h = Harness::new();
        *h.fixture.api.transcription_status.lock().unwrap() = Some(status);
        let note = h.note("キーの誤り", "");
        let started = h.ok("start_recording", json!({ "noteId": note }));
        assert_eq!(started["sources"], json!(["me", "them"]));

        // 最初の発話が拒否されたら、録音が止まる（利用者が「停止」を押さなくても、録音していない状態に戻る）
        let error = wait_until("キーの誤りの知らせ", || h.events("recording-error").into_iter().next());
        assert_eq!(error["noteId"], note.as_str());
        let message = error["message"].as_str().unwrap();
        assert!(message.starts_with("文字起こしを停止しました。API キーを確認してください（"), "{message}");
        assert!(message.contains(&format!("HTTP {status}")), "{message}");
        wait_until("録音の停止", || (h.recording_state() == "idle").then_some(()));
        let states: Vec<Value> = h.events("recording-status").iter().map(|status| status["state"].clone()).collect();
        assert_eq!(states, [json!("recording"), json!("finishing"), json!("idle")]);

        // 自分と相手の両方の発話が拒否されても、知らせは 1 回だけ。止まったあとは、何も送らない
        let sent = h.transcriptions().len();
        std::thread::sleep(Duration::from_millis(3500));
        assert_eq!(h.events("recording-error").len(), 1, "{:?}", h.events("recording-error"));
        assert_eq!(h.transcriptions().len(), sent, "録音を止めたあとも、音声を送っている");
        assert!(sent <= 2, "拒否されたあとも、発話を送り続けている（{sent} 回）");
        assert!(h.segments(&note).is_empty());

        // キーを直せば、また録音できる
        *h.fixture.api.transcription_status.lock().unwrap() = None;
        h.record_until_transcribed(&note);
        assert_eq!(h.events("recording-error").len(), 1);
    }
}
