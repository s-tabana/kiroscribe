//! すべてのコマンドを、画面側が渡す引数の名前のまま、模擬のランタイムの IPC で順に呼ぶ。
//! 返ってきた値とイベントの内容の見本を `src/test/ipc-contract.generated.ts` と比べる。
//!
//! 外部の API は手元の模擬サーバー（wiremock）、音声は模擬の取り込み、会議の検出は `meeting.txt` に差し替える。
//! 見本が変わったとき（コマンドの戻り値を変えたとき）は、次のコマンドで書き直してから `npm run typecheck` を通す:
//! `KIROSCRIBE_UPDATE_IPC_CONTRACT=1 cargo test -p kiroscribe --features e2e ipc_contract`

use std::time::Duration;

use kiroscribe_core::e2e as core;
use kiroscribe_core::secrets::platform_store;
use kiroscribe_core::Database;
use serde_json::{json, Value};
use tauri::test::MockRuntime;
use tauri::{Manager, WebviewWindow, WebviewWindowBuilder};

use super::fixture::{events_named, exclusive, record_events, wait_until, LOG_SUBMISSION_ID};
use super::{frontend, invoke, mock_app, read, repo_root, Frontend, Samples};
use crate::background::POPUP_LABEL;
use crate::state::AppState;

const CONTRACT_FILE: &str = "src/test/ipc-contract.generated.ts";
const UPDATE_ENV: &str = "KIROSCRIBE_UPDATE_IPC_CONTRACT";

/// コマンドを呼び、見本を集める。
struct Session {
    window: WebviewWindow<MockRuntime>,
    frontend: Frontend,
    called: Samples,
    returned: Samples,
    used: Vec<String>,
}

impl Session {
    /// 画面側と同じ引数の名前でコマンドを呼ぶ（`args` のキーが backend.ts と違えば失敗する）。
    fn call(&mut self, command: &str, args: Value) -> Result<Value, String> {
        let binding = self
            .frontend
            .commands
            .iter()
            .find(|binding| binding.command == command)
            .unwrap_or_else(|| panic!("画面側（backend.ts）は {command} を呼んでいません"))
            .clone();
        let mut keys: Vec<&String> = args.as_object().expect("引数はオブジェクト").keys().collect();
        let mut expected: Vec<&String> = binding.arg_keys.iter().collect();
        keys.sort();
        expected.sort();
        assert_eq!(keys, expected, "{command}: このテストの引数の名前が、画面側（backend.ts）と違います");

        self.used.push(command.to_string());
        let result = invoke(&self.window, command, args.clone());
        if let Some(params) = &binding.direct_params {
            let arguments: Vec<Value> = params.iter().map(|param| args[param].clone()).collect();
            self.called.add_arguments(&binding.method, &arguments);
            if let Ok(value) = &result {
                self.returned.add(&binding.method, value);
            }
        }
        // 失敗の理由は、1 つの文字列として画面に渡る（画面は、それをそのままメッセージとして表示する）
        result.map_err(|err| match err {
            Value::String(message) => message,
            other => panic!("{command}: 失敗の理由が文字列ではありません: {other}"),
        })
    }

    fn ok(&mut self, command: &str, args: Value) -> Value {
        self.call(command, args)
            .unwrap_or_else(|err| panic!("{command} が失敗しました: {err}"))
    }

    /// 見本に残さずに呼ぶ（待っているあいだの問い合わせ用。途中の状態が見本に入るかどうかが、その時々で変わらないように）。
    fn peek(&self, command: &str, args: Value) -> Value {
        invoke(&self.window, command, args).unwrap_or_else(|err| panic!("{command} が失敗しました: {err}"))
    }

    /// ポップアップが `kind` の内容になるまで待って、その内容を返す。
    fn wait_for_popup(&mut self, kind: &str) -> Value {
        wait_until(&format!("ポップアップ（{kind}）"), || {
            (self.peek("get_popup_state", json!({}))["kind"] == kind).then_some(())
        });
        self.ok("get_popup_state", json!({}))
    }
}

fn render_samples(samples: &Samples, name: &str, helper: &str, out: &mut String) {
    out.push_str(&format!("\nexport const {name} = {{\n"));
    for (method, samples) in &samples.0 {
        out.push_str(&format!("  {method}: [\n"));
        for sample in samples.lines() {
            out.push_str(&format!("    {sample},\n"));
        }
        out.push_str(&format!("  ] satisfies {helper}<\"{method}\">[],\n"));
    }
    out.push_str("};\n");
}

fn render(called: &Samples, returned: &Samples, emitted: &Samples) -> String {
    let mut out = String::from(
        r#"// このファイルは自動生成です。直接編集しないでください。
//
// Rust のコマンドに実際に渡した引数（called）・実際に返ってきた値（returned）・イベントで送られた値（emitted）の見本です。
// 画面側の型（Backend）と合っていることを、TypeScript のコンパイラ（npm run typecheck）が確かめます。
// 値は形だけを残してあります（数値は 0、文字列は "…"。"recording" のような列挙の値はそのまま）。
//
// Rust 側を変えて内容が変わったときは、次のコマンドで書き直します（src-tauri/src/ipc_tests/scenario.rs）:
//   KIROSCRIBE_UPDATE_IPC_CONTRACT=1 cargo test -p kiroscribe --features e2e ipc_contract

import type { Backend } from "../lib/backend";

type Method = keyof Backend;
/** メソッドの引数（＝コマンドに渡す値。backend.ts が同じ名前のままコマンドに渡している） */
type Called<K extends Method> = Backend[K] extends (...args: infer A) => unknown ? A : never;
/** メソッドの戻り値（＝コマンドの戻り値。値を返さないコマンドは null が返る） */
type Returned<K extends Method> = Backend[K] extends (...args: never[]) => Promise<infer T>
  ? [T] extends [void]
    ? null
    : T
  : never;
/** イベントの購読で、コールバックに渡される値（＝イベントの内容） */
type Emitted<K extends Method> = Backend[K] extends (cb: (payload: infer T) => void) => unknown ? T : never;
"#,
    );
    render_samples(called, "called", "Called", &mut out);
    render_samples(returned, "returned", "Returned", &mut out);
    render_samples(emitted, "emitted", "Emitted", &mut out);
    out
}

/// 見本を保存済みのファイルと比べる（書き直すのは、環境変数 `KIROSCRIBE_UPDATE_IPC_CONTRACT` があるとき）。
fn check_contract_file(rendered: &str) {
    let path = repo_root().join(CONTRACT_FILE);
    let saved = std::fs::read_to_string(&path).unwrap_or_default().replace("\r\n", "\n");
    assert!(
        saved == rendered,
        "コマンドの引数・戻り値・イベントの形が、{CONTRACT_FILE} の見本と違います。\n\
         意図した変更なら、次のコマンドで見本を書き直し、npm run typecheck で画面側の型（src/lib/types.ts）と合うことを確かめてください:\n  \
         {UPDATE_ENV}=1 cargo test -p kiroscribe --features e2e ipc_contract\n\n\
         --- いまの Rust 側から作った見本 ---\n{rendered}"
    );
}

// [IPC-002][IPC-003][IPC-004][IPC-005][IPC-006][IPC-008]
#[test]
fn ipc_contract_matches_the_frontend() {
    let (fixture, _serial) = exclusive();
    let data_dir = core::data_dir(&fixture.dir);
    std::fs::create_dir_all(&data_dir).unwrap();

    // 製品の起動時の処理（lib.rs の setup）と同じ状態を、模擬のランタイムの上に用意する
    let (app, window) = mock_app(&format!("{}/update/latest.json", fixture.server_uri));
    let db = Database::open(&data_dir.join("kiroscribe.db")).unwrap();
    app.manage(AppState::new(db, platform_store(&data_dir).unwrap()));
    // ポップアップのウィンドウは先に作っておく。模擬のランタイムはモニターの情報を返せないので、
    // ウィンドウを作る経路（表示位置の計算）は通せない。ここでは内容の差し替え（イベント）だけを確かめる
    WebviewWindowBuilder::new(&app, POPUP_LABEL, Default::default())
        .build()
        .expect("模擬のポップアップを作れません");

    let frontend = frontend();
    let events = record_events(&app);
    let mut s = Session {
        window,
        frontend,
        called: Samples::default(),
        returned: Samples::default(),
        used: Vec::new(),
    };
    let meeting_file = fixture.dir.join("meeting.txt");
    crate::meeting_watch::spawn(app.handle().clone());

    // ---- 会議の検出（API キーがまだない）: 録音せず、ポップアップで知らせる ----
    assert_eq!(s.ok("get_popup_state", json!({})), Value::Null);
    std::fs::write(&meeting_file, "googleMeet").unwrap();
    let popup = s.wait_for_popup("error");
    assert_eq!(popup["app"], "Google Meet");
    assert!(popup["message"].as_str().unwrap().contains("API キー"), "{popup}");
    assert_eq!(s.ok("list_notes", json!({})), json!([]), "録音できないときはノートを作らない");
    std::fs::remove_file(&meeting_file).unwrap();
    std::thread::sleep(Duration::from_millis(900)); // 会議の終了が確定するまで（猶予 0.4 秒）

    // ---- ノート ----
    let note = s.ok("create_note", json!({}));
    let id = note["id"].as_str().unwrap().to_string();
    assert_eq!(note["enhancedNotes"], Value::Null);
    let patch = json!({ "title": "週次定例", "userNotes": "- リリース日を決める" });
    let updated = s.ok("update_note", json!({ "id": id, "patch": patch }));
    assert_eq!(updated["title"], "週次定例");
    assert_eq!(s.ok("get_note", json!({ "id": id }))["userNotes"], "- リリース日を決める");
    let list = s.ok("list_notes", json!({}));
    assert_eq!(list[0]["id"], id.as_str());
    assert_eq!(list[0]["hasEnhanced"], false);

    // ---- 設定・API キー ----
    let settings = s.ok("get_settings", json!({}));
    assert_eq!(s.ok("save_settings", json!({ "settings": settings })), settings);
    let status = s.ok("get_api_key_status", json!({}));
    assert_eq!((&status["openai"], &status["anthropic"]), (&json!(false), &json!(false)));
    let status = s.ok("set_api_key", json!({ "provider": "openai", "value": "sk-test-openai" }));
    assert_eq!(status["openai"], true);
    let status = s.ok("set_api_key", json!({ "provider": "anthropic", "value": "sk-ant-test" }));
    assert_eq!(status["anthropic"], true);
    assert!(!status.to_string().contains("sk-ant-test"), "キーの値は画面に返さない");

    // ---- トレイのメニューでの切り替え（開いている設定画面に、変わった設定を知らせる）----
    crate::background::toggle_auto_record(app.handle()).unwrap();
    crate::background::toggle_auto_record(app.handle()).unwrap();
    assert_eq!(events_named(&events, "settings-changed").last(), Some(&settings));

    // ---- 録音（模擬の音声 → 文字起こし） ----
    let devices = s.ok("list_audio_devices", json!({}));
    assert_eq!(devices[0]["isDefault"], true);
    assert_eq!(s.ok("list_segments", json!({ "noteId": id })), json!([]));
    assert_eq!(s.ok("get_recording_status", json!({}))["state"], "idle");
    let status = s.ok("start_recording", json!({ "noteId": id }));
    assert_eq!(status["state"], "recording");
    assert_eq!(status["sources"], json!(["me", "them"]));
    let err = s.call("delete_note", json!({ "id": id })).unwrap_err();
    assert!(err.contains("録音中のノートは削除できません"), "{err}");
    wait_until("自分と相手の発話の文字起こし", || {
        let segments = events_named(&events, "transcript-segment");
        let has = |speaker: &str| segments.iter().any(|segment| segment["speaker"] == speaker);
        (has("me") && has("them")).then_some(())
    });
    assert_eq!(s.ok("pause_recording", json!({}))["state"], "paused");
    assert_eq!(s.ok("get_recording_status", json!({}))["runningSince"], Value::Null);
    assert_eq!(s.ok("resume_recording", json!({}))["state"], "recording");
    assert_eq!(s.ok("stop_recording", json!({}))["state"], "idle");
    let segments = s.ok("list_segments", json!({ "noteId": id }));
    assert!(segments.as_array().unwrap().len() >= 2, "{segments}");
    assert_eq!(segments[0]["text"], "来週の予定を確認します。");
    assert!(!events_named(&events, "audio-level").is_empty());
    let states: Vec<Value> = events_named(&events, "recording-status")
        .iter()
        .map(|status| status["state"].clone())
        .collect();
    for state in ["recording", "paused", "finishing", "idle"] {
        assert!(states.contains(&json!(state)), "recording-status に {state} がありません: {states:?}");
    }

    // ---- 同じノートで録音を再開すると、区切りが付く ----
    assert_eq!(s.ok("list_transcript_breaks", json!({ "noteId": id })), json!([]));
    let known = segments.as_array().unwrap().len();
    assert_eq!(s.ok("start_recording", json!({ "noteId": id }))["state"], "recording");
    let added = wait_until("区切りの知らせ", || events_named(&events, "transcript-break").into_iter().next());
    assert_eq!(added["round"], 2);
    wait_until("再開後の発話", || {
        let segments = s.peek("list_segments", json!({ "noteId": id }));
        (segments.as_array().unwrap().len() > known).then_some(())
    });
    assert_eq!(s.ok("stop_recording", json!({}))["state"], "idle");
    assert_eq!(s.ok("list_transcript_breaks", json!({ "noteId": id })), json!([added]));

    // ---- 議事録・チャット ----
    assert_eq!(s.ok("cancel_ai", json!({ "noteId": id, "kind": "enhance" })), Value::Null);
    let enhanced = s.ok("enhance_note", json!({ "noteId": id }));
    assert_eq!(enhanced["enhancedNotes"], "## 決定事項\n- リリースは 12/5");
    assert_eq!(enhanced["title"], "週次定例");
    let deltas = events_named(&events, "enhance-delta");
    assert_eq!(deltas.len(), 2, "届いた文字を順に画面へ送る: {deltas:?}");
    assert_eq!(deltas[0], json!({ "noteId": id, "delta": "# 週次定例\n\n## 決定事項\n" }));
    assert_eq!(s.ok("list_notes", json!({}))[0]["hasEnhanced"], true);

    assert_eq!(s.ok("list_chat_messages", json!({ "noteId": id })), json!([]));
    let answer = s.ok("send_chat_message", json!({ "noteId": id, "content": "決まったことは？" }));
    assert_eq!(answer["role"], "assistant");
    assert!(!events_named(&events, "chat-delta").is_empty());
    let history = s.ok("list_chat_messages", json!({ "noteId": id }));
    assert_eq!(history[0]["role"], "user");
    assert_eq!(history[0]["content"], "決まったことは？");
    assert_eq!(history[1]["role"], "assistant");
    assert_eq!(s.ok("clear_chat_messages", json!({ "noteId": id })), Value::Null);
    assert_eq!(s.ok("list_chat_messages", json!({ "noteId": id })), json!([]));

    // ---- 検索（メモの本文・議事録・文字起こしも対象） ----
    assert_eq!(s.ok("search_notes", json!({ "query": "リリース日" })), json!([id]), "メモの本文");
    assert_eq!(s.ok("search_notes", json!({ "query": "12/5" })), json!([id]), "議事録");
    assert_eq!(s.ok("search_notes", json!({ "query": "来週の予定" })), json!([id]), "文字起こし");
    assert_eq!(s.ok("search_notes", json!({ "query": "どこにもない言葉" })), json!([]));

    // ---- 書き出し ----
    let options = json!({ "minutes": true, "notes": true, "transcript": true });
    let exported = s.ok("export_markdown", json!({ "noteId": id, "options": options }));
    let markdown = exported["markdown"].as_str().unwrap();
    assert!(markdown.contains("リリースは 12/5") && markdown.contains("来週の予定を確認します。"), "{markdown}");
    assert!(exported["fileName"].as_str().unwrap().ends_with(".md"));
    let out = fixture.dir.join("書き出し.md");
    let saved = s.ok("save_markdown", json!({ "noteId": id, "options": options, "path": out }));
    assert_eq!(saved, Value::Null);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), markdown);
    let refused = s
        .call("save_markdown", json!({ "noteId": id, "options": options, "path": fixture.dir.join("x.exe") }))
        .unwrap_err();
    assert!(refused.contains(".md / .markdown / .txt"), "{refused}");

    // ---- デバッグ用のログ（詳しい記録の切り替え・画面の出来事・zip の保存）[LOG-010][LOG-011][MTG-026] ----
    let status = s.ok("get_debug_log_status", json!({}));
    assert_eq!(status["verboseUntil"], Value::Null);
    let on = s.ok("set_debug_verbose", json!({ "enabled": true }));
    assert!(on["verboseUntil"].as_i64().unwrap() > kiroscribe_core::time::now_ms());
    let off = s.ok("set_debug_verbose", json!({ "enabled": false }));
    assert_eq!(off["verboseUntil"], Value::Null);
    let written = s.ok(
        "write_debug_log",
        json!({ "level": "error", "event": "ui.command_failed", "fields": { "command": "x", "message": "y" } }),
    );
    assert_eq!(written, Value::Null);
    let bundle_path = fixture.dir.join("デバッグ用のログ.zip");
    let files = s.ok("save_debug_bundle", json!({ "path": bundle_path }));
    assert!(files.as_u64().unwrap() >= 4, "{files}");
    let bundle = std::fs::read(&bundle_path).unwrap();
    assert!(bundle.starts_with(b"PK"), "zip になっていない");
    let refused = s
        .call("save_debug_bundle", json!({ "path": fixture.dir.join("ログ.json") }))
        .unwrap_err();
    assert_eq!(refused, "保存できるのは .zip ファイルだけです");

    // ---- デバッグ用のログの提出（偽の入口に送る）[SET-015][SET-017] ----
    let submission = s.ok("submit_debug_logs", json!({ "requestId": "contract" }));
    assert_eq!(submission["id"], LOG_SUBMISSION_ID);
    let progress = wait_until("提出の進み具合", || events_named(&events, "log-submit-progress").into_iter().last());
    assert_eq!(progress["requestId"], "contract");
    assert!(progress["total"].as_u64().unwrap() > 0, "{progress}");

    // ---- 文字起こし API がキーを拒否したら、画面に知らせる ----
    *fixture.api.transcription_status.lock().unwrap() = Some(401);
    let second = s.ok("create_note", json!({}))["id"].as_str().unwrap().to_string();
    s.ok("start_recording", json!({ "noteId": second }));
    let error = wait_until("文字起こしの失敗の知らせ", || events_named(&events, "recording-error").into_iter().next());
    assert_eq!(error["noteId"], second.as_str());
    assert!(error["message"].as_str().unwrap().contains("API キーを確認してください"), "{error}");
    // キーの誤りでは、録音も自動で止まる
    wait_until("録音の停止", || (s.peek("get_recording_status", json!({}))["state"] == "idle").then_some(()));
    assert_eq!(s.ok("delete_note", json!({ "id": second })), Value::Null);
    *fixture.api.transcription_status.lock().unwrap() = None;

    // ---- 会議の検出（API キーあり）: ノートを作って録音し、会議が終わったら止める ----
    std::fs::write(&meeting_file, "zoom").unwrap();
    let created = wait_until("会議のノートの作成", || events_named(&events, "note-created").into_iter().next());
    let meeting_note = created["id"].as_str().unwrap().to_string();
    assert!(created["title"].as_str().unwrap().starts_with("Zoom "), "{created}");
    let popup = s.wait_for_popup("recording");
    assert_eq!(popup["noteId"], meeting_note.as_str());
    assert_eq!(s.ok("get_recording_status", json!({}))["noteId"], meeting_note.as_str());
    wait_until("会議の文字起こし", || {
        let segments = s.peek("list_segments", json!({ "noteId": meeting_note }));
        (!segments.as_array().unwrap().is_empty()).then_some(())
    });
    std::fs::remove_file(&meeting_file).unwrap();
    let popup = s.wait_for_popup("stopped");
    assert_eq!(popup["noteId"], meeting_note.as_str());
    assert_eq!(s.ok("get_recording_status", json!({}))["state"], "idle");
    let kinds: Vec<Value> = events_named(&events, "meeting-popup")
        .iter()
        .map(|payload| payload["kind"].clone())
        .collect();
    assert_eq!(
        kinds,
        [json!("error"), json!("recording"), json!("finishing"), json!("stopped")],
        "ポップアップの移り変わり"
    );

    assert_eq!(s.ok("open_note_from_popup", json!({ "noteId": meeting_note, "enhance": true })), Value::Null);
    assert_eq!(
        events_named(&events, "open-note"),
        [json!({ "noteId": meeting_note, "enhance": true })]
    );
    assert_eq!(s.ok("open_settings_from_popup", json!({})), Value::Null);
    assert_eq!(events_named(&events, "open-settings").len(), 1);
    assert_eq!(s.ok("close_popup", json!({})), Value::Null);

    // ---- 長い録音で、会議が終わったかを聞く [REC-025][REC-026] ----
    let long = s.ok("create_note", json!({}))["id"].as_str().unwrap().to_string();
    s.ok("start_recording", json!({ "noteId": long }));
    let handle = s.window.app_handle().clone();
    let check_at = |minutes: i64| {
        let handle = handle.clone();
        let now = kiroscribe_core::time::now_ms() + minutes * 60_000;
        tauri::async_runtime::block_on(async move { crate::recording_check::tick(&handle, now).await });
    };
    check_at(61);
    let asked = s.wait_for_popup("confirmEnd");
    assert_eq!(asked["noteId"], long.as_str());
    let answer = json!({ "noteId": long, "deadline": asked["deadline"], "keep": true });
    assert_eq!(s.ok("answer_recording_check", answer), Value::Null);
    check_at(31);
    let asked = s.wait_for_popup("confirmEnd");
    check_at(34);
    assert_eq!(s.wait_for_popup("stoppedUnanswered")["noteId"], long.as_str());
    // 止まったあとに答えても、何も変わらない
    let late = json!({ "noteId": long, "deadline": asked["deadline"], "keep": false });
    assert_eq!(s.ok("answer_recording_check", late), Value::Null);
    assert_eq!(s.ok("get_popup_state", json!({}))["kind"], "stoppedUnanswered");
    assert_eq!(s.ok("delete_note", json!({ "id": long })), Value::Null);
    assert_eq!(s.ok("close_popup", json!({})), Value::Null);

    // ---- ヘルプ ----
    assert_eq!(s.ok("get_app_version", json!({})), "0.1.0", "模擬のアプリのバージョン");
    let guide = s.ok("get_guide", json!({}));
    assert_eq!(guide["source"], "network");
    assert!(guide["markdown"].as_str().unwrap().contains("模擬サーバーの内容です"));
    let notes = s.ok("get_release_notes", json!({}));
    assert_eq!(notes["source"], "network");
    assert_eq!(notes["releases"][0]["version"], "99.0.0");
    assert_eq!(s.ok("open_external", json!({ "url": "https://example.com/docs" })), Value::Null);
    assert_eq!(read(&fixture.dir.join("opened-urls.log")).trim(), "https://example.com/docs");
    let refused = s.call("open_external", json!({ "url": "file:///etc/passwd" })).unwrap_err();
    assert!(refused.contains("このリンクは開けません"), "{refused}");

    // ---- 更新 ----
    assert_eq!(s.ok("take_update_notice", json!({})), Value::Null, "自動で更新していなければ、知らせるものはない");
    assert_eq!(s.ok("get_update_status", json!({}))["available"], Value::Null);
    let status = s.ok("check_for_update", json!({}));
    assert_eq!(status["available"]["version"], "99.0.0");
    assert_eq!(status["available"]["notes"], "- 模擬サーバーの更新");
    assert_eq!(s.ok("get_update_status", json!({})), status);
    // 模擬サーバーの署名は正しくないので、ダウンロードのあと署名の確認で止まる（インストールはしない）
    let err = s.call("install_update", json!({})).unwrap_err();
    assert!(err.contains("署名を確認できなかった"), "{err}");
    let phases: Vec<Value> = events_named(&events, "update-status")
        .iter()
        .map(|status| status["phase"].clone())
        .collect();
    assert!(phases.contains(&json!("downloading")), "{phases:?}");
    assert_eq!(phases.last(), Some(&json!("idle")), "中止したら元に戻る");

    // ---- 見本を作る（書き直しのときは、下の確認より先に書く。足りない呼び出しがあっても、型の確認は先に進められる）----
    let mut emitted = Samples::default();
    for binding in s.frontend.events.iter().filter(|binding| binding.passes_payload) {
        for payload in events_named(&events, &binding.event) {
            emitted.add(&binding.method, &payload);
        }
    }
    let rendered = render(&s.called, &s.returned, &emitted);
    let updating = std::env::var_os(UPDATE_ENV).is_some();
    if updating {
        std::fs::write(repo_root().join(CONTRACT_FILE), &rendered).unwrap();
        eprintln!("{CONTRACT_FILE} を書き直しました。npm run typecheck で、画面側の型と合うことを確かめてください");
    }

    // ---- 画面側が使うコマンドとイベントを、すべて確かめたこと ----
    for binding in &s.frontend.commands {
        assert!(
            s.used.contains(&binding.command),
            "このテストが {} を呼んでいません。コマンドを足したら、ここにも呼び出しを足してください",
            binding.command
        );
    }
    for binding in &s.frontend.events {
        assert!(
            !events_named(&events, &binding.event).is_empty(),
            "このテストでは {} が送られませんでした。イベントを足したら、ここにも送られる操作を足してください",
            binding.event
        );
    }
    if !updating {
        check_contract_file(&rendered);
    }
}
