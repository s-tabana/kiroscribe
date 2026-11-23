//! 起動時のデータの用意: 以前のアプリ（Kiroku）からの引き継ぎ、データベース、API キーの保存先。

use std::path::Path;

use kiroscribe_core::legacy;
use kiroscribe_core::secrets::{migrate_legacy_api_keys, platform_store, SecretStore};
use kiroscribe_core::settings::Settings;
use kiroscribe_core::Database;

/// データベースのファイル名（アプリのデータフォルダの中）。
pub const DB_FILE: &str = "kiroscribe.db";

/// 起動時に開いたデータ。
pub struct Opened {
    pub db: Database,
    pub settings: Settings,
    pub secrets: Box<dyn SecretStore>,
}

/// アプリのデータフォルダを開く。
///
/// 改名前（Kiroku）のデータは、Kiroscribe のデータベースがまだない初回起動のときだけ引き継ぐ
/// （毎回引き継ぐと、Kiroscribe で消した API キーが旧アプリの分から戻ってしまう）。
/// 失敗したら、空のデータベースでは始めずに理由を返す（次回の起動でやり直せる）。
pub fn open_data(data_dir: &Path) -> Result<Opened, String> {
    let db_path = data_dir.join(DB_FILE);
    let first_launch = !db_path.exists();
    if first_launch {
        if let Some(legacy_db) = legacy::legacy_database_path(data_dir) {
            legacy::migrate_database(&legacy_db, &db_path)
                .map_err(|err| format!("以前のアプリ（Kiroku）のデータを引き継げませんでした: {err}"))?;
        }
    }
    let db = Database::open(&db_path)
        .map_err(|err| format!("データベース（{}）を開けませんでした: {err}", db_path.display()))?;
    let settings = db
        .load_settings()
        .map_err(|err| format!("設定を読み込めませんでした: {err}"))?;
    let secrets =
        platform_store(data_dir).map_err(|err| format!("API キーの保存先を開けませんでした: {err}"))?;
    if first_launch {
        // API キーは入力し直せるので、引き継げなくても起動は続ける
        if let Err(err) = migrate_legacy_api_keys(data_dir, secrets.as_ref()) {
            eprintln!("以前の Kiroku の API キーを引き継げませんでした: {err}");
        }
    }
    Ok(Opened { db, settings, secrets })
}
