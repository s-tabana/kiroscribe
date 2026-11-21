mod ai;
mod background;
mod commands;
mod meeting_watch;
mod error;
mod help;
mod recording;
mod state;
mod update;

use kiroscribe_core::legacy;
use kiroscribe_core::secrets::{migrate_legacy_api_keys, platform_store};
use kiroscribe_core::time::now_ms;
use kiroscribe_core::Database;
use tauri::{Manager, WindowEvent};

use crate::ai::AiState;
use crate::background::{BackgroundState, AUTOSTART_ARG, MAIN_LABEL};
use crate::recording::RecorderState;
use crate::state::AppState;
use crate::update::UpdateState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // 2 つ目を起動しようとしたら、常駐中のウィンドウを出すだけにする（会議の検出が二重にならないように）
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            background::show_main(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec![AUTOSTART_ARG]),
        ))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        // 更新の確認とインストールは Rust 側（update.rs）から呼ぶ。画面には権限を渡さない
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let db_path = data_dir.join("kiroscribe.db");
            // 改名前（Kiroku）のデータは、Kiroscribe のデータベースがまだない初回起動のときだけ引き継ぐ
            // （毎回引き継ぐと、Kiroscribe で消した API キーが旧アプリの分から戻ってしまう）
            let first_launch = !db_path.exists();
            if first_launch {
                if let Some(legacy_db) = legacy::legacy_database_path(&data_dir) {
                    // 失敗したら空のデータベースで始めずに起動を止める（次回の起動でやり直せる）
                    legacy::migrate_database(&legacy_db, &db_path)?;
                }
            }
            let db = Database::open(&db_path)?;
            let settings = db.load_settings()?;
            let secrets = platform_store(&data_dir)?;
            if first_launch {
                // API キーは入力し直せるので、引き継げなくても起動は続ける
                if let Err(err) = migrate_legacy_api_keys(&data_dir, secrets.as_ref()) {
                    eprintln!("以前の Kiroku の API キーを引き継げませんでした: {err}");
                }
            }
            // アプリ内更新で再起動した直後か（下で、自動起動の引数が付いていてもウィンドウを出す）
            let restarted_after_update = update::take_restart_marker(&db, now_ms());
            app.manage(AppState::new(db, secrets));
            app.manage(RecorderState::default());
            app.manage(AiState::default());
            app.manage(BackgroundState::default());
            app.manage(UpdateState::default());

            let handle = app.handle().clone();
            // トレイが使えない環境でも起動は続ける
            if let Err(err) = background::setup_tray(&handle, &settings) {
                eprintln!("タスクトレイを作成できませんでした: {err}");
            }
            background::apply_settings(&handle, &settings);
            meeting_watch::spawn(handle.clone());
            update::spawn_auto_check(handle.clone());

            // Windows の起動時に自動で起動したときは、ウィンドウを出さずトレイに常駐する。
            // ただし更新による再起動では、更新前と同じ引数で起動されるので、更新できたことが分かるよう出す
            if restarted_after_update || !std::env::args().any(|a| a == AUTOSTART_ARG) {
                background::show_main(&handle);
            }
            Ok(())
        })
        // メインウィンドウを閉じてもトレイに残す（会議の検出と録音を続けるため）
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let quitting = window.state::<BackgroundState>().is_quitting();
                if window.label() == MAIN_LABEL && !quitting {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_notes,
            commands::create_note,
            commands::get_note,
            commands::update_note,
            commands::delete_note,
            commands::get_settings,
            commands::save_settings,
            commands::get_api_key_status,
            commands::set_api_key,
            commands::export_markdown,
            commands::save_markdown,
            recording::list_audio_devices,
            recording::list_segments,
            recording::get_recording_status,
            recording::start_recording,
            recording::pause_recording,
            recording::resume_recording,
            recording::stop_recording,
            ai::enhance_note,
            ai::cancel_ai,
            ai::list_chat_messages,
            ai::clear_chat_messages,
            ai::send_chat_message,
            background::get_popup_state,
            background::close_popup,
            background::open_note_from_popup,
            background::open_settings_from_popup,
            help::get_guide,
            help::get_release_notes,
            help::open_external,
            update::get_app_version,
            update::get_update_status,
            update::check_for_update,
            update::install_update,
        ])
        .run(tauri::generate_context!())
        .expect("Kiroscribe の起動に失敗しました");
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    fn read_json(path: &Path) -> serde_json::Value {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("{} を読めません: {err}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|err| panic!("{} が JSON ではありません: {err}", path.display()))
    }

    /// バージョンは package.json を正とし、Cargo（ワークスペース）と tauri.conf.json がそれに合っていること。
    /// ずれると、画面に出るバージョン・インストーラ・更新の判定が食い違う。
    #[test]
    fn versions_are_consistent() {
        let tauri_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = tauri_dir.parent().unwrap();

        let package = read_json(&root.join("package.json"));
        let version = package["version"].as_str().expect("package.json の version");
        assert_eq!(env!("CARGO_PKG_VERSION"), version, "Cargo.toml（workspace.package.version）と package.json");

        // tauri.conf.json は package.json を参照する（参照先がこの package.json であること）
        let conf = read_json(&tauri_dir.join("tauri.conf.json"));
        let reference = conf["version"].as_str().expect("tauri.conf.json の version");
        assert!(
            reference.ends_with("package.json"),
            "tauri.conf.json の version は package.json への参照にする（現在: {reference}）"
        );
        assert_eq!(
            tauri_dir.join(reference).canonicalize().unwrap(),
            root.join("package.json").canonicalize().unwrap()
        );

        let lock = read_json(&root.join("package-lock.json"));
        assert_eq!(lock["version"].as_str(), Some(version), "package-lock.json");
        assert_eq!(lock["packages"][""]["version"].as_str(), Some(version), "package-lock.json（packages）");
    }

    /// アプリ内更新の配布元と公開鍵が設定されていること（欠けると更新の確認が常に失敗する）。
    #[test]
    fn updater_is_configured() {
        let conf = read_json(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json"));
        assert_eq!(conf["bundle"]["createUpdaterArtifacts"], true);
        let updater = &conf["plugins"]["updater"];
        assert_eq!(
            updater["endpoints"],
            serde_json::json!(["https://github.com/s-tabana/kiroscribe/releases/latest/download/latest.json"])
        );
        assert_eq!(updater["windows"]["installMode"], "passive");
        let pubkey = updater["pubkey"].as_str().expect("公開鍵");
        assert!(pubkey.len() > 100 && !pubkey.contains(char::is_whitespace), "公開鍵は 1 行の base64 文字列");
    }
}
