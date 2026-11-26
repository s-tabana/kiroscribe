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

pub use indexer::{FolderStatus, IndexState, IndexStatus, Indexer, SearchFolder};
pub use store::{Hit, SearchIndex, INDEX_FILE};

use crate::db::Database;
use crate::error::Result;

/// 探すフォルダの一覧を保存する、設定のテーブルのキー（追加と外すは、その場で保存する）。
const FOLDERS_KEY: &str = "companion.folders";

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
            SearchFolder { path: "C:\\共有資料".into(), no_content: false },
            SearchFolder { path: "D:\\人事".into(), no_content: true },
        ];
        db.save_search_folders(&folders).unwrap();
        assert_eq!(db.load_search_folders().unwrap(), folders);
        // 「中身を送らない」がない古い形でも読める（初期値はオフ）
        db.set_value(FOLDERS_KEY, r#"[{"path":"C:\\x"}]"#).unwrap();
        assert!(!db.load_search_folders().unwrap()[0].no_content);
        // 設定（「設定を保存」で保存するもの）とは別に保存する
        assert_eq!(db.load_settings().unwrap(), crate::settings::Settings::default());
    }
}
