//! 改名前（Kiroku）のデータの引き継ぎ。
//!
//! アプリ名を Kiroku から Kiroscribe に変えたとき、識別子（`dev.kiroku.desktop` → `dev.kiroscribe.desktop`）と
//! データベースのファイル名（`kiroku.db` → `kiroscribe.db`）も変わった。旧アプリのデータは別のフォルダに残るので、
//! 新しい場所にまだデータベースがなければ、旧の場所からコピーして引き継ぐ。旧のファイルは消さない。
//!
//! API キーの引き継ぎは [`crate::secrets`] にある。

use std::fs::File;
use std::path::{Path, PathBuf};

use crate::error::Result;

/// 改名前のアプリの識別子（アプリデータフォルダの名前）。
pub const LEGACY_IDENTIFIER: &str = "dev.kiroku.desktop";
/// 改名前のデータベースのファイル名。
pub const LEGACY_DB_FILE: &str = "kiroku.db";

/// コピーの途中のファイルに付ける接尾辞。そろってから本来の名前に変える。
const STAGING_SUFFIX: &str = ".migrating";

/// 改名前のアプリのデータフォルダ。いまのデータフォルダと同じ親フォルダにある。
pub fn legacy_data_dir(data_dir: &Path) -> Option<PathBuf> {
    data_dir.parent().map(|parent| parent.join(LEGACY_IDENTIFIER))
}

/// 改名前のアプリのデータベースの場所。
pub fn legacy_database_path(data_dir: &Path) -> Option<PathBuf> {
    legacy_data_dir(data_dir).map(|dir| dir.join(LEGACY_DB_FILE))
}

/// 旧データベースを新しい場所へコピーする。コピーしたら `true` を返す。
///
/// 新しい場所に既にデータベースがあるとき、旧データベースがないときは何もしない。
/// SQLite の WAL（`-wal`）と共有メモリ（`-shm`）のファイルがあれば一緒にコピーする
/// （WAL には、本体にまだ書き戻されていない最近の変更が入っている）。
pub fn migrate_database(legacy_db: &Path, new_db: &Path) -> Result<bool> {
    if new_db.exists() || !legacy_db.is_file() {
        return Ok(false);
    }
    if let Some(parent) = new_db.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut staged = Vec::new();
    let result = stage_copies(legacy_db, new_db, &mut staged).and_then(|()| commit(&staged));
    if result.is_err() {
        // 中途半端なファイルを残さない（残すと、次に作る空のデータベースと食い違う WAL が読まれてしまう）。
        // 本体は最後に置くので「新しいデータベースがある」状態にはならず、次回の起動でやり直せる
        for (staging, target) in &staged {
            let _ = std::fs::remove_file(staging);
            if target != new_db {
                let _ = std::fs::remove_file(target);
            }
        }
    }
    result.map(|()| true)
}

/// 旧のファイルを、新しい場所の一時的な名前へコピーする。`staged` は（一時的な名前, 本来の名前）。
fn stage_copies(
    legacy_db: &Path,
    new_db: &Path,
    staged: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<()> {
    // 旧アプリが動いたままでも食い違いにくいよう、WAL を先に、本体を後にコピーする
    let legacy_wal = with_suffix(legacy_db, "-wal");
    if legacy_wal.is_file() {
        let target = with_suffix(new_db, "-wal");
        let staging = with_suffix(&target, STAGING_SUFFIX);
        staged.push((staging.clone(), target));
        copy_file(&legacy_wal, &staging)?;
    }

    // 共有メモリのファイルは SQLite が開くときに作り直すので、コピーできなくても続ける
    // （Windows では、旧アプリが書き込み中だとロックされていて読めないことがある）
    let legacy_shm = with_suffix(legacy_db, "-shm");
    if legacy_shm.is_file() {
        let target = with_suffix(new_db, "-shm");
        let staging = with_suffix(&target, STAGING_SUFFIX);
        if copy_file(&legacy_shm, &staging).is_ok() {
            staged.push((staging, target));
        } else {
            let _ = std::fs::remove_file(&staging);
        }
    }

    let staging = with_suffix(new_db, STAGING_SUFFIX);
    staged.push((staging.clone(), new_db.to_path_buf()));
    copy_file(legacy_db, &staging)?;
    Ok(())
}

/// 一時的な名前を本来の名前に変える。本体（`staged` の末尾）が最後になる。
fn commit(staged: &[(PathBuf, PathBuf)]) -> Result<()> {
    for (staging, target) in staged {
        std::fs::rename(staging, target)?;
    }
    Ok(())
}

/// ファイルを読んで、新しいファイルに書き出す。
///
/// `std::fs::copy`（Windows では OS のコピー機能）に任せず `File::open` で読むのは、旧アプリがデータベースを
/// 開いたままでも読めるようにするため（`File::open` は、ほかの読み書きを妨げない共有モードで開く）。
fn copy_file(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut source = File::open(from)?;
    let mut target = File::create(to)?;
    std::io::copy(&mut source, &mut target)?;
    target.sync_all()
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::notes::NotePatch;

    /// 旧アプリと同じ配置（`<親>/dev.kiroku.desktop/kiroku.db`）と、新しいデータベースの場所を返す。
    fn layout(root: &Path) -> (PathBuf, PathBuf) {
        let data_dir = root.join("dev.kiroscribe.desktop");
        let legacy_db = legacy_database_path(&data_dir).unwrap();
        (legacy_db, data_dir.join("kiroscribe.db"))
    }

    fn file_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn legacy_paths_are_siblings_of_the_data_dir() {
        let data_dir = Path::new("/home/me/.local/share/dev.kiroscribe.desktop");
        assert_eq!(
            legacy_data_dir(data_dir).unwrap(),
            Path::new("/home/me/.local/share/dev.kiroku.desktop")
        );
        assert_eq!(
            legacy_database_path(data_dir).unwrap(),
            Path::new("/home/me/.local/share/dev.kiroku.desktop/kiroku.db")
        );
        assert_eq!(legacy_data_dir(Path::new("/")), None);
    }

    #[test]
    fn copies_database_with_wal_and_shm_and_keeps_the_originals() {
        let root = tempfile::tempdir().unwrap();
        let (legacy_db, new_db) = layout(root.path());
        std::fs::create_dir_all(legacy_db.parent().unwrap()).unwrap();
        std::fs::write(&legacy_db, b"db").unwrap();
        std::fs::write(with_suffix(&legacy_db, "-wal"), b"wal").unwrap();
        std::fs::write(with_suffix(&legacy_db, "-shm"), b"shm").unwrap();

        assert!(migrate_database(&legacy_db, &new_db).unwrap());

        assert_eq!(std::fs::read(&new_db).unwrap(), b"db");
        assert_eq!(std::fs::read(with_suffix(&new_db, "-wal")).unwrap(), b"wal");
        assert_eq!(std::fs::read(with_suffix(&new_db, "-shm")).unwrap(), b"shm");
        // 一時的な名前のファイルは残らない
        assert_eq!(
            file_names(new_db.parent().unwrap()),
            ["kiroscribe.db", "kiroscribe.db-shm", "kiroscribe.db-wal"]
        );
        // 旧のファイルは消さない
        assert_eq!(
            file_names(legacy_db.parent().unwrap()),
            ["kiroku.db", "kiroku.db-shm", "kiroku.db-wal"]
        );
        assert_eq!(std::fs::read(&legacy_db).unwrap(), b"db");

        // 2 回目は、新しいデータベースがあるので何もしない
        assert!(!migrate_database(&legacy_db, &new_db).unwrap());
    }

    #[test]
    fn copies_only_the_database_when_there_is_no_wal() {
        let root = tempfile::tempdir().unwrap();
        let (legacy_db, new_db) = layout(root.path());
        std::fs::create_dir_all(legacy_db.parent().unwrap()).unwrap();
        std::fs::write(&legacy_db, b"db").unwrap();

        assert!(migrate_database(&legacy_db, &new_db).unwrap());
        assert_eq!(file_names(new_db.parent().unwrap()), ["kiroscribe.db"]);
    }

    #[test]
    fn does_not_overwrite_an_existing_database() {
        let root = tempfile::tempdir().unwrap();
        let (legacy_db, new_db) = layout(root.path());
        std::fs::create_dir_all(legacy_db.parent().unwrap()).unwrap();
        std::fs::create_dir_all(new_db.parent().unwrap()).unwrap();
        std::fs::write(&legacy_db, b"old").unwrap();
        std::fs::write(with_suffix(&legacy_db, "-wal"), b"old-wal").unwrap();
        std::fs::write(&new_db, b"new").unwrap();

        assert!(!migrate_database(&legacy_db, &new_db).unwrap());
        assert_eq!(std::fs::read(&new_db).unwrap(), b"new");
        // 旧の WAL だけを持ち込むこともしない
        assert_eq!(file_names(new_db.parent().unwrap()), ["kiroscribe.db"]);
    }

    #[test]
    fn does_nothing_without_a_legacy_database() {
        let root = tempfile::tempdir().unwrap();
        let (legacy_db, new_db) = layout(root.path());

        assert!(!migrate_database(&legacy_db, &new_db).unwrap());
        assert!(!new_db.exists());
        assert!(!new_db.parent().unwrap().exists(), "フォルダも作らない");

        // 旧のフォルダだけあってデータベースがない場合も同じ
        std::fs::create_dir_all(legacy_db.parent().unwrap()).unwrap();
        assert!(!migrate_database(&legacy_db, &new_db).unwrap());
        assert!(!new_db.exists());
    }

    #[test]
    fn failed_copy_leaves_nothing_behind_and_can_be_retried() {
        let root = tempfile::tempdir().unwrap();
        let (legacy_db, new_db) = layout(root.path());
        std::fs::create_dir_all(legacy_db.parent().unwrap()).unwrap();
        std::fs::write(&legacy_db, b"db").unwrap();
        std::fs::write(with_suffix(&legacy_db, "-wal"), b"wal").unwrap();
        // 本体の一時的な名前と同じ名前のフォルダを置いて、本体のコピーを失敗させる
        let blocker = with_suffix(&new_db, STAGING_SUFFIX);
        std::fs::create_dir_all(&blocker).unwrap();

        assert!(migrate_database(&legacy_db, &new_db).is_err());
        assert!(!new_db.exists());
        assert!(
            !with_suffix(&new_db, "-wal").exists(),
            "WAL だけが残ってはいけない"
        );
        assert_eq!(
            file_names(new_db.parent().unwrap()),
            ["kiroscribe.db.migrating"],
            "残るのは原因のフォルダだけ"
        );

        // 原因がなくなれば、次回はコピーできる
        std::fs::remove_dir(&blocker).unwrap();
        assert!(migrate_database(&legacy_db, &new_db).unwrap());
        assert_eq!(std::fs::read(&new_db).unwrap(), b"db");
        assert_eq!(std::fs::read(with_suffix(&new_db, "-wal")).unwrap(), b"wal");
    }

    /// 本物の SQLite で確かめる: 旧アプリが開いたまま（変更が WAL にだけある状態）でも、ノートが引き継がれる。
    #[test]
    fn notes_survive_the_move_even_while_the_legacy_database_is_open() {
        let root = tempfile::tempdir().unwrap();
        let (legacy_db, new_db) = layout(root.path());
        let legacy = Database::open(&legacy_db).unwrap();
        let note = legacy.create_note().unwrap();
        legacy
            .update_note(
                &note.id,
                &NotePatch {
                    title: Some("定例会議".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(
            with_suffix(&legacy_db, "-wal").is_file(),
            "前提: 旧データベースは WAL モードで開いている"
        );

        assert!(migrate_database(&legacy_db, &new_db).unwrap());

        let migrated = Database::open(&new_db).unwrap();
        assert_eq!(migrated.get_note(&note.id).unwrap().title, "定例会議");
        // 引き継いだあとの変更は、旧データベースには影響しない
        migrated.delete_note(&note.id).unwrap();
        assert_eq!(legacy.get_note(&note.id).unwrap().title, "定例会議");
    }
}
