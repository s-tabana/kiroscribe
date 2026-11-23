//! 起動時のデータの用意: 以前のアプリ（Kiroku）からの引き継ぎと、失敗したときの扱い。
//! API キーの保存先がファイルになる E2E テスト用のビルドでだけ動かす（製品のビルドでは OS の資格情報ストアに触れるため）。

use std::path::{Path, PathBuf};

use kiroscribe_core::notes::NotePatch;
use kiroscribe_core::secrets::{set_api_key, Provider};
use kiroscribe_core::transcript::Speaker;
use kiroscribe_core::Database;

use super::super::fixture::fixture;
use crate::startup::{open_data, DB_FILE};

/// このテスト用の、空のフォルダ。中に「いまのアプリ」と「以前のアプリ」のデータフォルダを並べる。
fn root(name: &str) -> PathBuf {
    let dir = fixture().file("startup").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn current(root: &Path) -> PathBuf {
    root.join("dev.kiroscribe.desktop")
}

fn legacy(root: &Path) -> PathBuf {
    root.join("dev.kiroku.desktop")
}

/// 以前のアプリのデータ（ノート 1 件・文字起こし・設定）と API キーを用意する。
fn seed_legacy(root: &Path) -> String {
    let dir = legacy(root);
    std::fs::create_dir_all(&dir).unwrap();
    let db = Database::open(&dir.join("kiroku.db")).unwrap();
    let note = db.create_note().unwrap();
    let patch = NotePatch {
        title: Some("以前のノート".into()),
        user_notes: Some("- 以前のメモ".into()),
        ..Default::default()
    };
    db.update_note(&note.id, &patch).unwrap();
    db.add_segment(&note.id, Speaker::Me, 1_000, 2_000, "以前の発話").unwrap();
    let mut settings = db.load_settings().unwrap();
    settings.my_name = "田中".into();
    db.save_settings(settings).unwrap();
    std::fs::write(dir.join("secrets.json"), r#"{"openai":"sk-legacy-openai","anthropic":"sk-ant-legacy"}"#).unwrap();
    note.id
}

/// 以前のアプリのデータと API キーは、最初の起動のときだけ引き継ぐ。
// [DAT-004][DAT-008][DAT-009]
#[test]
fn legacy_data_and_keys_are_taken_over_on_the_first_launch_only() {
    let root = root("first-launch");
    let note_id = seed_legacy(&root);
    let data_dir = current(&root);

    // 最初の起動: データフォルダを作り、ノート・文字起こし・設定・API キーを引き継ぐ
    {
        let opened = open_data(&data_dir).expect("起動できる");
        assert!(data_dir.join(DB_FILE).is_file());
        let note = opened.db.get_note(&note_id).unwrap();
        assert_eq!((note.title.as_str(), note.user_notes.as_str()), ("以前のノート", "- 以前のメモ"));
        assert_eq!(opened.db.list_segments(&note_id).unwrap()[0].text, "以前の発話");
        assert_eq!(opened.settings.my_name, "田中");
        assert_eq!(opened.secrets.get(Provider::OpenAi).unwrap().as_deref(), Some("sk-legacy-openai"));
        assert_eq!(opened.secrets.get(Provider::Anthropic).unwrap().as_deref(), Some("sk-ant-legacy"));

        // Kiroscribe でキーを消し、ノートを直す
        set_api_key(opened.secrets.as_ref(), Provider::OpenAi, "").unwrap();
        let patch = NotePatch {
            title: Some("Kiroscribe で直したノート".into()),
            ..Default::default()
        };
        opened.db.update_note(&note_id, &patch).unwrap();
    }
    // 以前のアプリのファイルは消さず、変えない
    let old = Database::open(&legacy(&root).join("kiroku.db")).unwrap();
    assert_eq!(old.get_note(&note_id).unwrap().title, "以前のノート");
    assert!(std::fs::read_to_string(legacy(&root).join("secrets.json")).unwrap().contains("sk-legacy-openai"));

    // 2 回目の起動: 引き継ぎはしない（消したキーが戻らず、直したノートが上書きされない）
    let opened = open_data(&data_dir).expect("起動できる");
    assert_eq!(opened.secrets.get(Provider::OpenAi).unwrap(), None, "Kiroscribe で消したキーが戻っている");
    assert_eq!(opened.secrets.get(Provider::Anthropic).unwrap().as_deref(), Some("sk-ant-legacy"));
    assert_eq!(opened.db.get_note(&note_id).unwrap().title, "Kiroscribe で直したノート");
}

/// API キーを引き継げなくても、アプリは起動する。以前のアプリがなければ、空の状態で始まる。
// [DAT-009]
#[test]
fn startup_continues_when_the_legacy_keys_cannot_be_read() {
    let root = root("broken-keys");
    let note_id = seed_legacy(&root);
    std::fs::write(legacy(&root).join("secrets.json"), "{ 壊れた JSON").unwrap();
    let opened = open_data(&current(&root)).expect("キーを引き継げなくても起動する");
    assert_eq!(opened.db.get_note(&note_id).unwrap().title, "以前のノート");
    assert_eq!(opened.secrets.get(Provider::OpenAi).unwrap(), None);

    let root = super::startup::root("fresh");
    let opened = open_data(&current(&root)).expect("以前のアプリがなくても起動する");
    assert!(opened.db.list_notes().unwrap().is_empty());
    assert_eq!(opened.secrets.get(Provider::Anthropic).unwrap(), None);
    assert!(current(&root).join(DB_FILE).is_file());
}

/// 引き継ぎに失敗したら、空のデータベースでは始めずに起動を止める。直れば、次の起動で引き継げる。
// [DAT-007]
#[test]
fn a_failed_takeover_stops_the_startup_and_can_be_retried() {
    let root = root("failed-takeover");
    let note_id = seed_legacy(&root);
    let data_dir = current(&root);
    // データフォルダを作れない（同じ名前のファイルがある）ので、コピーに失敗する
    std::fs::write(&data_dir, "フォルダではない").unwrap();

    let reason = open_data(&data_dir).err().expect("起動を止める");
    assert!(reason.starts_with("以前のアプリ（Kiroku）のデータを引き継げませんでした: "), "{reason}");
    assert!(data_dir.is_file(), "失敗したのに、何かを作り直している");
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2, "中途半端なファイルを残している");

    // 原因がなくなれば、次の起動でやり直せる
    std::fs::remove_file(&data_dir).unwrap();
    let opened = open_data(&data_dir).expect("やり直せる");
    assert_eq!(opened.db.get_note(&note_id).unwrap().title, "以前のノート");
    drop(opened);

    // データベースを開けないときも、理由を付けて止める（空のデータベースに置き換えない）
    let db_path = data_dir.join(DB_FILE);
    for sidecar in ["kiroscribe.db-wal", "kiroscribe.db-shm"] {
        let _ = std::fs::remove_file(data_dir.join(sidecar));
    }
    std::fs::write(&db_path, "データベースではない内容").unwrap();
    let reason = open_data(&data_dir).err().expect("起動を止める");
    assert!(reason.starts_with("データベース（"), "{reason}");
    assert!(reason.contains("を開けませんでした: "), "{reason}");
    assert_eq!(std::fs::read(&db_path).unwrap(), "データベースではない内容".as_bytes());
}

/// 起動時の失敗は、理由と、データを変えていないことを、利用者に知らせる文にする。
// [DAT-007]
#[test]
fn a_startup_failure_is_explained_to_the_user() {
    let root = root("explained");
    let data_dir = current(&root);
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join(DB_FILE), "データベースではない内容").unwrap();
    let reason = open_data(&data_dir).err().expect("起動を止める");

    let message = crate::startup::failure_message(&reason);
    assert!(message.starts_with("Kiroscribe を起動できませんでした。"), "{message}");
    assert!(message.contains(&reason), "理由が入っていない: {message}");
    assert!(message.contains(&data_dir.join(DB_FILE).display().to_string()), "どのファイルかが分からない: {message}");
    assert!(message.contains("データは変更していません"), "{message}");
    assert_eq!(crate::startup::FAILURE_TITLE, "Kiroscribe");
}
