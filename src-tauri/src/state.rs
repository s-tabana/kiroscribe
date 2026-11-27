use std::sync::{Mutex, MutexGuard};

use kiroscribe_core::secrets::SecretStore;
use kiroscribe_core::Database;

/// アプリ全体で共有する状態。
pub struct AppState {
    db: Mutex<Database>,
    pub secrets: Box<dyn SecretStore>,
    pub log_submission: tokio::sync::Mutex<()>,
}

impl AppState {
    pub fn new(db: Database, secrets: Box<dyn SecretStore>) -> Self {
        Self {
            db: Mutex::new(db),
            secrets,
            log_submission: tokio::sync::Mutex::new(()),
        }
    }

    /// DB をロックして返す。別スレッドがパニックしてもデータ自体は使えるため、毒化は無視する。
    pub fn db(&self) -> MutexGuard<'_, Database> {
        self.db.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
