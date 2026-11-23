mod ai;
mod background;
mod commands;
#[cfg(feature = "e2e")]
mod e2e;
mod meeting_watch;
mod error;
mod help;
#[cfg(test)]
mod ipc_tests;
mod recording;
mod runtime;
mod startup;
mod state;
mod update;

use kiroscribe_core::time::now_ms;
use tauri::{Manager, WindowEvent};

use crate::ai::AiState;
use crate::background::{BackgroundState, AUTOSTART_ARG, MAIN_LABEL};
use crate::meeting_watch::MeetingState;
use crate::recording::RecorderState;
use crate::runtime::Runtime;
use crate::state::AppState;
use crate::update::UpdateState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::<Runtime>::new()
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
            // ウィンドウを閉じる操作の扱い（下の on_window_event）が使うので、最初に用意する
            app.manage(BackgroundState::default());
            let data_dir = app.path().app_data_dir()?;
            let startup::Opened { db, settings, secrets } = match startup::open_data(&data_dir) {
                Ok(opened) => opened,
                Err(reason) => {
                    // 空のデータベースでは始めない。理由を知らせて、ダイアログが閉じられたら終了する
                    // （メインウィンドウは出さない。トレイや会議の検出も始めない）
                    startup::report_failure(app.handle(), &reason);
                    return Ok(());
                }
            };
            // アプリ内更新で再起動した直後か（下で、ウィンドウを出すかどうかを決める）
            let version = app.package_info().version.to_string();
            let restart = update::take_restart(&db, &version, now_ms());
            app.manage(UpdateState::new(settings.auto_update, update::auto_skip_version(&db)));
            app.manage(AppState::new(db, secrets));
            app.manage(RecorderState::default());
            app.manage(AiState::default());
            app.manage(MeetingState::default());

            let handle = app.handle().clone();
            // トレイが使えない環境でも起動は続ける
            if let Err(err) = background::setup_tray(&handle, &settings) {
                eprintln!("タスクトレイを作成できませんでした: {err}");
            }
            background::apply_settings(&handle, &settings);
            meeting_watch::spawn(handle.clone());
            update::spawn_auto_check(handle.clone());

            // Windows の起動時に自動で起動したときは、ウィンドウを出さずトレイに常駐する。
            // 更新による再起動では更新前と同じ引数で起動されるので、引数ではなく更新の種類で決める
            let autostart = std::env::args().any(|a| a == AUTOSTART_ARG);
            let show = update::show_window_on_launch(autostart, restart);
            if show {
                background::show_main(&handle);
            }
            #[cfg(feature = "e2e")]
            e2e::log_setup_done(show);
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
        });
    #[allow(unused_mut)]
    let mut context = tauri::generate_context!();
    // E2E テスト用のビルドでだけ、データの場所や OS に出ていくものを差し替える（src/e2e.rs）
    #[cfg(feature = "e2e")]
    let builder = {
        e2e::prepare(&mut context);
        e2e::plugins(builder)
    };
    register_commands(builder)
        .run(context)
        .expect("Kiroscribe の起動に失敗しました");
}

/// 画面から呼べるコマンドを登録する（テストでも同じ一覧を登録して、画面側との食い違いを確かめる）。
fn register_commands(builder: tauri::Builder<Runtime>) -> tauri::Builder<Runtime> {
    builder
        .invoke_handler(tauri::generate_handler![
            commands::list_notes,
            commands::search_notes,
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
            update::take_update_notice,
        ])
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
    // [UPD-017]
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

    /// 待ち時間の本来の値。E2E テスト用のビルド（feature `e2e`）では設定で縮められるので、
    /// 製品の値はここで確かめておく（会議の開始・終了の確定は meetings、API の再試行は retry のテストにある）。
    // [UPD-001][MTG-007][MTG-008][ERR-006]
    #[cfg(not(feature = "e2e"))]
    #[test]
    fn waits_have_their_product_values() {
        use std::time::Duration;
        assert_eq!(crate::update::first_check_delay(), Duration::from_secs(20), "起動から最初の更新の確認まで");
        assert_eq!(crate::meeting_watch::poll_interval(), Duration::from_secs(2), "会議の検出を調べる間隔");
        let tracker = kiroscribe_core::meetings::MeetingTracker::default();
        assert_eq!(tracker.start_confirm_ms, 4_000, "会議の開始を確定するまで");
        assert_eq!(tracker.end_grace_ms, 20_000, "会議が見えなくなってから録音を止めるまで");
        let retry = kiroscribe_core::retry::RetryPolicy::default();
        assert_eq!(retry.max_attempts, 4);
        assert_eq!((retry.base_delay, retry.max_delay), (Duration::from_secs(1), Duration::from_secs(8)));
    }

    /// アプリ内更新の配布元と公開鍵が設定されていること（欠けると更新の確認が常に失敗する）。
    // [UPD-003][REL-007]
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
