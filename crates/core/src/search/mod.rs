//! 会議の相棒が探す、PC の中の資料の索引（指定したフォルダ）。
//!
//! - [`extract`]: ファイルから題名と中身を取り出す
//! - [`walk`]: 追加したフォルダの中の、索引に入れるファイルを集める
//! - [`store`]: 索引の保存先（`search-index.db`）と検索
//! - [`indexer`]: 索引を裏で作り、フォルダの変更を見張る

pub mod extract;
pub mod indexer;
pub mod normalize;
pub mod store;
pub mod walk;

pub use indexer::{FolderStatus, IndexState, IndexStatus, Indexer, Intervals, SearchFolder};
pub use store::{Hit, SearchIndex, INDEX_FILE};

use crate::db::Database;
use crate::error::Result;

/// 探すフォルダの一覧を保存する、設定のテーブルのキー（追加と外すは、その場で保存する）。
const FOLDERS_KEY: &str = "companion.folders";

/// フォルダのパスを、比べるための形にする（区切りの違いと末尾の区切りを除く。Windows では大文字と小文字を区別しない）。
fn folder_key(path: &str) -> Vec<String> {
    std::path::Path::new(path)
        .components()
        .filter(|part| !matches!(part, std::path::Component::CurDir))
        .map(|part| {
            let text = part.as_os_str().to_string_lossy();
            if cfg!(windows) {
                text.to_lowercase()
            } else {
                text.into_owned()
            }
        })
        .collect()
}

/// 足そうとしているフォルダと、すでにある一覧のフォルダとの重なり。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlap<'a> {
    /// 同じフォルダ
    Same(&'a SearchFolder),
    /// 一覧のフォルダの中か、一覧のフォルダを含む
    Nested(&'a SearchFolder),
}

/// `path` が、一覧のどれかのフォルダと同じか、中か、それを含むか。
pub fn overlap<'a>(folders: &'a [SearchFolder], path: &str) -> Option<Overlap<'a>> {
    let key = folder_key(path);
    folders.iter().find_map(|folder| {
        let other = folder_key(&folder.path);
        if other == key {
            Some(Overlap::Same(folder))
        } else if other.starts_with(&key) || key.starts_with(&other) {
            Some(Overlap::Nested(folder))
        } else {
            None
        }
    })
}

impl Database {
    /// 探すフォルダの一覧（何も保存していなければ空）。
    pub fn load_search_folders(&self) -> Result<Vec<SearchFolder>> {
        Ok(match self.get_value(FOLDERS_KEY)? {
            Some(json) => serde_json::from_str(&json)?,
            None => Vec::new(),
        })
    }

    pub fn save_search_folders(&self, folders: &[SearchFolder]) -> Result<()> {
        self.set_value(FOLDERS_KEY, &serde_json::to_string(folders)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // [IDX-001][IDX-034]
    #[test]
    fn folder_list_is_empty_by_default_and_persists() {
        let db = Database::open_in_memory().unwrap();
        assert!(db.load_search_folders().unwrap().is_empty(), "初期値は空");
        let folders = vec![
            SearchFolder { path: "C:\\共有資料".into(), no_content: false, place: None },
            SearchFolder { path: "D:\\人事".into(), no_content: true, place: None },
        ];
        db.save_search_folders(&folders).unwrap();
        assert_eq!(db.load_search_folders().unwrap(), folders);
        // 「中身を送らない」がない古い形でも読める（初期値はオフ）
        db.set_value(FOLDERS_KEY, r#"[{"path":"C:\\x"}]"#).unwrap();
        assert!(!db.load_search_folders().unwrap()[0].no_content);
        // 設定（「設定を保存」で保存するもの）とは別に保存する
        assert_eq!(db.load_settings().unwrap(), crate::settings::Settings::default());
    }

    /// 足したときのフォルダの実体を保存し、読み直しても同じ。実体を記録する前の設定や、読めない値は、
    /// 記録していないもの（選び直すまで中身を読まない）として読む。
    // [IDX-027][IDX-001]
    #[test]
    fn the_registered_place_is_saved_with_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("資料");
        std::fs::create_dir_all(&root).unwrap();
        let registered = SearchFolder::register(&root).unwrap();
        let place = registered.place.clone().expect("足したときの実体");
        assert!(place.admits(&crate::search::walk::Root::open(&root).unwrap()));

        let db = Database::open_in_memory().unwrap();
        db.save_search_folders(std::slice::from_ref(&registered)).unwrap();
        assert_eq!(db.load_search_folders().unwrap(), [registered]);

        db.set_value(FOLDERS_KEY, r#"[{"path":"/x","noContent":true}]"#).unwrap();
        assert_eq!(db.load_search_folders().unwrap()[0].place, None, "実体を記録する前の設定");
        db.set_value(FOLDERS_KEY, r#"[{"path":"/x","place":{"real":null,"volume":"x","file":"1"}}]"#).unwrap();
        let loaded = db.load_search_folders().unwrap();
        assert_eq!((loaded[0].place.clone(), loaded[0].path.as_str()), (None, "/x"), "読めない値は、記録していないものとして読む");
        db.set_value(FOLDERS_KEY, r#"[{"path":"/x","place":{"real":null,"volume":"1","file":"1","born":"5"}}]"#).unwrap();
        assert_eq!(db.load_search_folders().unwrap()[0].place, None, "実際の場所を記録していない値は、選び直すまで読まない");
    }

    /// リンクのフォルダ・フォルダでないものは、足せない。
    // [IDX-027]
    #[cfg(unix)]
    #[test]
    fn links_and_files_cannot_be_registered() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("資料");
        std::fs::create_dir_all(&real).unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("リンク")).unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        assert!(SearchFolder::register(&dir.path().join("リンク")).is_err());
        assert!(SearchFolder::register(&dir.path().join("a.txt")).is_err());
        assert!(SearchFolder::register(&dir.path().join("ない")).is_err());
    }

    // [IDX-001]
    #[test]
    fn overlapping_folders_are_detected() {
        let folders = vec![SearchFolder { path: "/home/a/資料".into(), no_content: false, place: None }];
        assert_eq!(overlap(&folders, "/home/a/資料"), Some(Overlap::Same(&folders[0])));
        assert_eq!(overlap(&folders, "/home/a/資料/"), Some(Overlap::Same(&folders[0])), "末尾の区切り");
        assert_eq!(overlap(&folders, "/home/a/資料/人事"), Some(Overlap::Nested(&folders[0])), "中のフォルダ");
        assert_eq!(overlap(&folders, "/home/a"), Some(Overlap::Nested(&folders[0])), "含むフォルダ");
        assert_eq!(overlap(&folders, "/home/a/資料2"), None, "名前の先頭が同じだけのフォルダ");
        assert_eq!(overlap(&folders, "/home/b"), None);
        assert_eq!(overlap(&[], "/home/b"), None);
    }
}
