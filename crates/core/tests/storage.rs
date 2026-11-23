//! 保存まわりの仕様のテスト（ファイルのデータベース、画面と共通の入力例、API キーを書き残さないこと）。

use std::path::Path;

use chrono::{Local, TimeZone};
use kiroscribe_core::claude::Role;
use kiroscribe_core::export::{note_to_markdown, suggested_file_name, ExportOptions};
use kiroscribe_core::notes::{make_preview, Note, NotePatch};
use kiroscribe_core::secrets::{
    api_key_status, require_api_key, set_api_key, FileSecretStore, MemorySecretStore, Provider,
};
use kiroscribe_core::settings::Settings;
use kiroscribe_core::transcript::Speaker;
use kiroscribe_core::{CoreError, Database};

fn patch(title: Option<&str>, user_notes: Option<&str>) -> NotePatch {
    NotePatch {
        title: title.map(str::to_string),
        user_notes: user_notes.map(str::to_string),
        ..Default::default()
    }
}

// [NOTE-019][SET-004]
#[test]
fn data_survives_reopening_the_database_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kiroscribe.db");

    let (note_id, saved_settings) = {
        let db = Database::open(&path).unwrap();
        let note = db.create_note().unwrap();
        db.update_note(&note.id, &patch(Some("週次定例"), Some("- リリース日を確認")))
            .unwrap();
        db.add_segment(&note.id, Speaker::Them, 1_000, 2_000, "リリースはいつですか")
            .unwrap();
        db.add_segment(&note.id, Speaker::Me, 3_000, 4_000, "12月5日です").unwrap();
        db.save_enhanced_notes(&note.id, "## 決定事項\n- 12/5 リリース", None).unwrap();
        db.add_chat_message(&note.id, Role::User, "決まったことは？").unwrap();
        db.add_chat_message(&note.id, Role::Assistant, "リリースは 12/5 です").unwrap();
        let settings = db
            .save_settings(Settings {
                language: "en".into(),
                claude_model: "claude-sonnet-5-5".into(),
                my_name: "田花".into(),
                auto_record_meetings: false,
                auto_update: true,
                ..Default::default()
            })
            .unwrap();
        (note.id, settings)
    }; // ここでデータベースを閉じる（アプリの終了）

    let db = Database::open(&path).unwrap();
    let note = db.get_note(&note_id).unwrap();
    assert_eq!(note.title, "週次定例");
    assert_eq!(note.user_notes, "- リリース日を確認");
    assert_eq!(note.enhanced_notes.as_deref(), Some("## 決定事項\n- 12/5 リリース"));
    let list = db.list_notes().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].id, note_id);

    let texts: Vec<String> = db.list_segments(&note_id).unwrap().into_iter().map(|s| s.text).collect();
    assert_eq!(texts, ["リリースはいつですか", "12月5日です"]);
    let chat: Vec<(Role, String)> = db
        .list_chat_messages(&note_id)
        .unwrap()
        .into_iter()
        .map(|m| (m.role, m.content))
        .collect();
    assert_eq!(
        chat,
        [
            (Role::User, "決まったことは？".to_string()),
            (Role::Assistant, "リリースは 12/5 です".to_string())
        ]
    );
    assert_eq!(db.load_settings().unwrap(), saved_settings);
}

// [NOTE-015]
#[test]
fn deleting_a_note_removes_its_transcript_and_chat_only() {
    let db = Database::open_in_memory().unwrap();
    let note = db.create_note().unwrap();
    let other = db.create_note().unwrap();
    for id in [&note.id, &other.id] {
        db.add_segment(id, Speaker::Me, 0, 1, "発話").unwrap();
        db.add_chat_message(id, Role::User, "質問").unwrap();
        db.add_chat_message(id, Role::Assistant, "回答").unwrap();
    }

    db.delete_note(&note.id).unwrap();

    assert!(db.list_segments(&note.id).unwrap().is_empty());
    assert!(db.list_chat_messages(&note.id).unwrap().is_empty());
    // ほかのノートのものは残る
    assert_eq!(db.list_segments(&other.id).unwrap().len(), 1);
    assert_eq!(db.list_chat_messages(&other.id).unwrap().len(), 2);
}

// [NOTE-017]
#[test]
fn missing_note_errors_name_the_id() {
    let db = Database::open_in_memory().unwrap();
    let expected = "ノートが見つかりません: no-such-note";
    assert_eq!(db.get_note("no-such-note").unwrap_err().to_string(), expected);
    assert_eq!(
        db.update_note("no-such-note", &patch(Some("題"), None))
            .unwrap_err()
            .to_string(),
        expected
    );
    assert_eq!(db.delete_note("no-such-note").unwrap_err().to_string(), expected);
    assert!(matches!(
        db.save_enhanced_notes("no-such-note", "本文", None).unwrap_err(),
        CoreError::NoteNotFound(_)
    ));
    // 失敗しても、何も作られない
    assert!(db.list_notes().unwrap().is_empty());
}

#[derive(serde::Deserialize)]
struct PreviewCase {
    text: String,
    preview: String,
}

/// 画面側（`src/lib/notes.test.ts`）と同じ入力例。片方の規則だけを変えると、どちらかのテストが落ちる。
// [NOTE-007]
#[test]
fn preview_matches_the_examples_shared_with_the_frontend() {
    let cases: Vec<PreviewCase> =
        serde_json::from_str(include_str!("fixtures/preview-cases.json")).unwrap();
    assert!(cases.len() >= 10);
    for case in cases {
        assert_eq!(make_preview(&case.text), case.preview, "入力: {:?}", case.text);
    }
}

/// チェックボックスの記号が 2 つ続く行だけ、本体と画面で結果が違う（本体は続く分をすべて除き、画面は 1 つだけ除く）。
/// いまの振る舞いを固定する。そろえるときは、共通の入力例（preview-cases.json）へ移す。
// [NOTE-007]
#[test]
fn preview_strips_repeated_checkbox_markers() {
    assert_eq!(make_preview("- [ ] [ ] 二重"), "二重");
    assert_eq!(make_preview("- [ ] [x] 二重"), "二重");
}

/// 画面側（`src/lib/types.ts` の DEFAULT_SETTINGS）と同じ見本。項目を足すときは、見本と両方を直す。
// [SET-001]
#[test]
fn default_settings_match_the_fixture_shared_with_the_frontend() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/default-settings.json")).unwrap();
    assert_eq!(serde_json::to_value(Settings::default()).unwrap(), fixture);
    // 見本をそのまま読み込める（画面から渡される形）
    let parsed: Settings = serde_json::from_value(fixture).unwrap();
    assert_eq!(parsed, Settings::default());
}

// [KEY-003]
#[test]
fn saving_a_key_replaces_the_previous_one() {
    let store = MemorySecretStore::default();
    set_api_key(&store, Provider::OpenAi, "sk-old").unwrap();
    set_api_key(&store, Provider::OpenAi, " sk-new ").unwrap();
    assert_eq!(require_api_key(&store, Provider::OpenAi).unwrap(), "sk-new");
    // もう一方のキーには影響しない
    assert!(!api_key_status(&store).unwrap().anthropic);
}

// [SEC-001]
#[test]
fn key_status_never_contains_the_key_value() {
    let store = MemorySecretStore::default();
    set_api_key(&store, Provider::OpenAi, "sk-openai-SECRETVALUE").unwrap();
    set_api_key(&store, Provider::Anthropic, "sk-ant-SECRETVALUE").unwrap();

    let status = serde_json::to_value(api_key_status(&store).unwrap()).unwrap();
    // 画面に渡すのは、設定済みかどうかと保存先の説明だけ
    let mut fields: Vec<&str> = status.as_object().unwrap().keys().map(String::as_str).collect();
    fields.sort_unstable();
    assert_eq!(fields, ["anthropic", "openai", "storage"]);
    assert_eq!(status["openai"], true);
    assert_eq!(status["anthropic"], true);
    assert!(!status.to_string().contains("SECRETVALUE"), "{status}");
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

fn files_under(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(files_under(&path));
        } else {
            files.push(path);
        }
    }
    files
}

// [SEC-002]
#[test]
fn api_keys_are_not_written_to_the_database_or_exports() {
    const OPENAI_KEY: &str = "sk-proj-KIROSCRIBETEST-openai-0123456789";
    const ANTHROPIC_KEY: &str = "sk-ant-KIROSCRIBETEST-anthropic-0123456789";

    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let secrets_path = dir.path().join("secrets").join("secrets.json");
    let store = FileSecretStore::new(secrets_path);
    set_api_key(&store, Provider::OpenAi, OPENAI_KEY).unwrap();
    set_api_key(&store, Provider::Anthropic, ANTHROPIC_KEY).unwrap();

    // キーを保存したあとで、ひととおりの保存を行う
    let db = Database::open(&data_dir.join("kiroscribe.db")).unwrap();
    let note = db.create_note().unwrap();
    db.update_note(&note.id, &patch(Some("定例"), Some("- メモ"))).unwrap();
    db.add_segment(&note.id, Speaker::Me, 0, 1_000, "発話").unwrap();
    db.add_chat_message(&note.id, Role::User, "質問").unwrap();
    db.save_enhanced_notes(&note.id, "## 概要\n- A", None).unwrap();
    let settings = db.save_settings(Settings::default()).unwrap();
    let exported = note_to_markdown(
        &db.get_note(&note.id).unwrap(),
        &db.list_segments(&note.id).unwrap(),
        ExportOptions {
            minutes: true,
            notes: true,
            transcript: true,
        },
        &settings.my_name,
    );
    drop(db);

    for key in [OPENAI_KEY, ANTHROPIC_KEY] {
        assert!(!exported.contains(key));
        assert!(!serde_json::to_string(&settings).unwrap().contains(key));
        // データベースのファイル（WAL などを含む）のどこにも書かれていない
        let files = files_under(&data_dir);
        assert!(!files.is_empty());
        for file in files {
            let bytes = std::fs::read(&file).unwrap();
            assert!(!contains_bytes(&bytes, key.as_bytes()), "{} にキーがあります", file.display());
        }
    }
    // 設定には、キーを入れる項目がない
    let fields = serde_json::to_value(&settings).unwrap();
    assert!(fields
        .as_object()
        .unwrap()
        .keys()
        .all(|name| !name.to_lowercase().contains("key")));
}

// [SHR-008]
#[test]
fn suggested_file_name_cuts_the_title_at_80_chars() {
    let created_at = Local.with_ymd_and_hms(2026, 11, 20, 15, 4, 0).unwrap().timestamp_millis();
    let note = |title: &str| Note {
        id: "n".into(),
        title: title.into(),
        user_notes: String::new(),
        enhanced_notes: None,
        enhanced_at: None,
        created_at,
        updated_at: created_at,
    };
    let name = suggested_file_name(&note(&"あ".repeat(100)));
    assert_eq!(name, format!("2026-11-20 {}.md", "あ".repeat(80)));
    // 80 文字ちょうどは切らない
    assert_eq!(suggested_file_name(&note(&"a".repeat(80))), format!("2026-11-20 {}.md", "a".repeat(80)));
    // 制御文字も置き換える
    assert_eq!(suggested_file_name(&note("議事\tメモ\n2")), "2026-11-20 議事_メモ_2.md");
}

// [NOTE-008]
#[test]
fn search_covers_title_notes_minutes_and_transcript() {
    let db = Database::open_in_memory().unwrap();
    let by_title = db.create_note().unwrap();
    let by_notes = db.create_note().unwrap();
    let by_minutes = db.create_note().unwrap();
    let by_transcript = db.create_note().unwrap();
    let unrelated = db.create_note().unwrap();
    db.update_note(&by_title.id, &patch(Some("Offsite の計画"), None)).unwrap();
    // 一覧のプレビュー（1 行目）には出ない行
    db.update_note(&by_notes.id, &patch(Some("定例"), Some("- 日程\n- 来月の offsite の場所")))
        .unwrap();
    db.save_enhanced_notes(&by_minutes.id, "## 決定事項\n- OFFSITE は箱根", None).unwrap();
    db.add_segment(&by_transcript.id, Speaker::Them, 1_000, 2_000, "オフサイト、つまり Offsite ですね")
        .unwrap();
    db.add_segment(&by_transcript.id, Speaker::Me, 3_000, 4_000, "Offsite は来月です").unwrap();
    db.update_note(&unrelated.id, &patch(Some("採用面談"), Some("- 経歴"))).unwrap();

    // 一覧と同じ並び（新しい順）で、当てはまるノートを 1 回ずつ返す。大文字と小文字は区別しない
    let expected = vec![by_transcript.id.clone(), by_minutes.id.clone(), by_notes.id.clone(), by_title.id.clone()];
    assert_eq!(db.search_notes("offsite").unwrap(), expected);
    assert_eq!(db.search_notes("  OFFSITE ").unwrap(), expected);

    assert_eq!(db.search_notes("箱根").unwrap(), vec![by_minutes.id.clone()]);
    assert_eq!(db.search_notes("オフサイト").unwrap(), vec![by_transcript.id.clone()]);
    assert_eq!(db.search_notes("経歴").unwrap(), vec![unrelated.id.clone()]);
    assert!(db.search_notes("どこにもない").unwrap().is_empty());
    // 空の検索では何も返さない（絞り込まないのは、呼ぶ側の役目）
    assert!(db.search_notes("   ").unwrap().is_empty());
    // SQL の特別な文字も、ただの文字として探す
    assert!(db.search_notes("%").unwrap().is_empty());
    assert!(db.search_notes("_").unwrap().is_empty());

    // 消したノートは見つからない
    db.delete_note(&by_transcript.id).unwrap();
    assert!(db.search_notes("オフサイト").unwrap().is_empty());
}

// [MIN-010]
#[test]
fn saving_blank_minutes_returns_the_note_to_no_minutes() {
    let db = Database::open_in_memory().unwrap();
    let note = db.create_note().unwrap();
    db.update_note(&note.id, &patch(Some("定例"), Some("- 自分のメモ"))).unwrap();
    db.save_enhanced_notes(&note.id, "## 決定事項\n- A", None).unwrap();
    assert!(db.list_notes().unwrap()[0].has_enhanced);

    let minutes = |text: &str| NotePatch {
        enhanced_notes: Some(text.to_string()),
        ..Default::default()
    };
    // 手で直した中身があれば、議事録ありのまま
    let edited = db.update_note(&note.id, &minutes("## 決定事項\n- 手で直した")).unwrap();
    assert_eq!(edited.enhanced_notes.as_deref(), Some("## 決定事項\n- 手で直した"));
    assert!(edited.enhanced_at.is_some());

    // 空・空白だけにして保存すると、議事録なしに戻る
    for blank in ["", "  \n\t　"] {
        db.save_enhanced_notes(&note.id, "## 決定事項\n- A", None).unwrap();
        let cleared = db.update_note(&note.id, &minutes(blank)).unwrap();
        assert_eq!(cleared.enhanced_notes, None, "{blank:?}");
        assert_eq!(cleared.enhanced_at, None, "{blank:?}");
        assert_eq!(db.get_note(&note.id).unwrap(), cleared);
        let summary = &db.list_notes().unwrap()[0];
        assert!(!summary.has_enhanced);
        assert_eq!(summary.preview, "自分のメモ", "プレビューは自分のメモに戻る");
    }
    // タイトルとメモは変わらない
    let after = db.get_note(&note.id).unwrap();
    assert_eq!((after.title.as_str(), after.user_notes.as_str()), ("定例", "- 自分のメモ"));

    // ほかの項目だけの更新では、議事録は変わらない
    db.save_enhanced_notes(&note.id, "## 決定事項\n- B", None).unwrap();
    let renamed = db.update_note(&note.id, &patch(Some("週次定例"), None)).unwrap();
    assert_eq!(renamed.enhanced_notes.as_deref(), Some("## 決定事項\n- B"));
    assert!(renamed.enhanced_at.is_some());
}
