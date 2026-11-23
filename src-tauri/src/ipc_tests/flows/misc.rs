//! 書き出し・外部ブラウザ・終了など、そのほかのコマンド。

use kiroscribe_core::transcript::Speaker;
use serde_json::{json, Value};

use super::super::fixture::{wait_until, Harness};

/// ファイルへの保存: 文字コードは UTF-8（BOM なし）で、内容は「Markdown をコピー」と同じ。
/// Markdown・テキスト以外の拡張子には書かない。
// [SHR-009][SHR-010][SHR-011]
#[test]
fn markdown_is_saved_as_utf8_only_to_markdown_or_text_files() {
    let h = Harness::new();
    let note = h.note("週次定例", "- リリース日を決める");
    {
        let state = h.state();
        let db = state.db();
        db.save_enhanced_notes(&note, "## 決定事項\n- リリースは 12/5", None).unwrap();
        db.add_segment(&note, Speaker::Them, 1_000, 2_000, "来週の火曜はどうですか").unwrap();
    }
    let options = json!({ "minutes": true, "notes": true, "transcript": true });
    let exported = h.ok("export_markdown", json!({ "noteId": note, "options": options }));
    let markdown = exported["markdown"].as_str().unwrap();
    for part in ["# 週次定例", "リリースは 12/5", "- リリース日を決める", "来週の火曜はどうですか"] {
        assert!(markdown.contains(part), "書き出しに「{part}」がない: {markdown}");
    }
    let dir = h.fixture.file("save-markdown");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // 受け付ける拡張子（大文字小文字は問わない）
    for name in ["議事録.md", "議事録.MD", "議事録.markdown", "議事録.txt", "議事録.Txt"] {
        let path = dir.join(name);
        let saved = h.ok("save_markdown", json!({ "noteId": note, "options": options, "path": path }));
        assert_eq!(saved, Value::Null);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes, markdown.as_bytes(), "{name}: 「Markdown をコピー」と同じ内容を UTF-8 で書く");
        assert!(!bytes.starts_with(&[0xEF, 0xBB, 0xBF]), "{name}: BOM を付けている");
    }

    // ほかの拡張子・拡張子なしには書かない
    for name in ["議事録.exe", "議事録.docx", "議事録", "md", "議事録.md.bak"] {
        let path = dir.join(name);
        let err = h
            .call("save_markdown", json!({ "noteId": note, "options": options, "path": path }))
            .unwrap_err();
        assert_eq!(err, "保存できるのは .md / .markdown / .txt ファイルだけです", "{name}");
        assert!(!path.exists(), "{name}: 断ったのにファイルを書いている");
    }

    // 書けなかったときは、理由を返す
    let path = dir.join("ないフォルダ").join("議事録.md");
    let err = h
        .call("save_markdown", json!({ "noteId": note, "options": options, "path": path }))
        .unwrap_err();
    assert!(err.starts_with("ファイルを保存できませんでした: "), "{err}");
    assert!(!path.exists());
}

/// 既定のブラウザで開くのは https:// の URL だけ。コマンドの失敗は、1 つの文字列で画面に渡る。
// [SEC-008][IPC-006]
#[test]
fn only_https_links_are_opened_in_the_browser() {
    let h = Harness::new();
    let log = h.fixture.file("opened-urls.log");

    let long = format!("https://example.com/{}", "a".repeat(2048));
    for url in [
        "http://example.com/",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "mailto:someone@example.com",
        "https://user:pass@example.com/",
        "https://example.com@evil.example/",
        "https://",
        "https://example.com/\u{0007}",
        "example.com",
        "",
        long.as_str(),
    ] {
        // 画面の invoke の reject の値が、そのまま 1 つの文字列（JSON の文字列）になっている
        let rejected = super::super::invoke(&h.window, "open_external", json!({ "url": url })).unwrap_err();
        assert_eq!(
            rejected,
            Value::String("このリンクは開けません（https:// で始まるリンクだけを開けます）".into()),
            "{url}"
        );
    }
    assert!(!log.exists(), "断った URL を開こうとしている");

    assert_eq!(h.ok("open_external", json!({ "url": "https://example.com/docs?x=1#top" })), Value::Null);
    assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "https://example.com/docs?x=1#top");
}

/// 終了の前に、録音を止めて、最後の発話の文字起こしが終わるのを待つ。
// [TRY-005]
#[test]
fn quitting_stops_the_recording_and_waits_for_the_last_transcript() {
    let h = Harness::new();
    let note = h.note("終了のテスト", "");
    h.record_until_transcribed(&note);
    // 自分の発話が区切られた直後から 1.2 秒後は、次の発話の途中にあたる
    let known = h.segments(&note).len();
    wait_until("次の自分の発話", || {
        h.segments(&note).into_iter().skip(known).find(|segment| segment["speaker"] == "me")
    });
    std::thread::sleep(std::time::Duration::from_millis(1200));
    *h.fixture.api.transcription_delay.lock().unwrap() = std::time::Duration::from_millis(800);
    let sent = h.transcriptions().len();

    let handle = h.app.handle().clone();
    tauri::async_runtime::block_on(async move { crate::background::prepare_quit(&handle).await });

    // 片づけが済んだ時点で、録音は止まり、話している途中だった発話まで保存されている
    assert_eq!(h.recording_state(), "idle");
    assert!(h.transcriptions().len() > sent, "話している途中だった発話を、文字起こしに回していない");
    assert_eq!(
        h.segments(&note).len(),
        h.transcriptions().len(),
        "送った発話の文字起こしが保存される前に、終了へ進んでいる"
    );
    use tauri::Manager;
    assert!(h.app.state::<crate::background::BackgroundState>().is_quitting());
}

/// トレイのメニューの「会議を検出したら自動で録音」は、設定を入れ替えて保存し、開いている画面に知らせる。
// [TRY-004]
#[test]
fn the_tray_toggle_saves_the_setting_and_tells_the_windows() {
    let h = Harness::new();
    assert!(h.settings().auto_record_meetings, "最初はオン");
    h.save_settings(|settings| settings.my_name = "田中".into());

    crate::background::toggle_auto_record(h.app.handle()).unwrap();
    assert!(!h.settings().auto_record_meetings, "オフにして保存する");
    assert_eq!(h.ok("get_settings", json!({}))["autoRecordMeetings"], false);
    // 開いている設定画面が追従できるよう、保存した設定をそのまま知らせる
    let told = h.events("settings-changed");
    assert_eq!(told.len(), 1, "設定画面の「設定を保存」では知らせない（知らせるのは、画面の外で変わったとき）");
    assert_eq!(told[0], h.ok("get_settings", json!({})));
    assert_eq!(told[0]["myName"], "田中", "ほかの設定は変えない");

    crate::background::toggle_auto_record(h.app.handle()).unwrap();
    assert!(h.settings().auto_record_meetings);
    assert_eq!(h.events("settings-changed").len(), 2);
    assert_eq!(h.events("settings-changed")[1]["autoRecordMeetings"], true);
}
