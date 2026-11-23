//! 常駐まわり: タスクトレイ、閉じてもトレイに残る動作、Windows 起動時の自動起動、会議検出のポップアップ。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use kiroscribe_core::settings::Settings;
use serde::Serialize;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, State, WebviewUrl, WebviewWindowBuilder};

use crate::error::CmdResult;
use crate::recording::stop_now;
use crate::runtime::{AppHandle, Runtime};
use crate::state::AppState;

pub const MAIN_LABEL: &str = "main";
pub const POPUP_LABEL: &str = "meeting-popup";
pub const EVENT_POPUP: &str = "meeting-popup";
pub const EVENT_OPEN_NOTE: &str = "open-note";
pub const EVENT_OPEN_SETTINGS: &str = "open-settings";
/// メインウィンドウを表示した（閉じてトレイにいた間の知らせを、画面が出せるように）
pub const EVENT_MAIN_SHOWN: &str = "main-shown";
/// Windows 起動時の自動起動で付ける引数。付いていればウィンドウを出さずトレイで起動する
pub const AUTOSTART_ARG: &str = "--autostart";

const POPUP_WIDTH: f64 = 380.0;
const POPUP_HEIGHT: f64 = 128.0;

/// ポップアップに表示する内容。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum PopupPayload {
    /// 自動で録音を始めた
    #[serde(rename_all = "camelCase")]
    Recording { app: String, note_id: String, title: String },
    /// 会議が終わったので録音を止めている（文字起こしの仕上げ中）
    #[serde(rename_all = "camelCase")]
    Finishing { app: String, note_id: String, title: String },
    /// 録音を止めた
    #[serde(rename_all = "camelCase")]
    Stopped { app: String, note_id: String, title: String },
    /// 自動で録音できなかった
    #[serde(rename_all = "camelCase")]
    Error { app: String, message: String },
}

#[derive(Default)]
pub struct BackgroundState {
    popup: Mutex<Option<PopupPayload>>,
    tray_auto_item: Mutex<Option<CheckMenuItem<Runtime>>>,
    quitting: AtomicBool,
}

impl BackgroundState {
    pub fn is_quitting(&self) -> bool {
        self.quitting.load(Ordering::SeqCst)
    }
}

pub fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_LABEL) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        let _ = app.emit_to(MAIN_LABEL, EVENT_MAIN_SHOWN, ());
    }
}

/// メインウィンドウを表示しているか（閉じてトレイにいる間は `false`）。分からなければ、表示しているものとして扱う。
pub fn main_window_visible(app: &AppHandle) -> bool {
    app.get_webview_window(MAIN_LABEL)
        .is_some_and(|window| window.is_visible().unwrap_or(true))
}

/// ポップアップを表示する（既にあれば内容を差し替える）。会議中の画面からフォーカスを奪わないよう、作るときに focus しない。
pub fn show_popup(app: &AppHandle, payload: PopupPayload) {
    let background = app.state::<BackgroundState>();
    *background.popup.lock().unwrap() = Some(payload.clone());

    if app.get_webview_window(POPUP_LABEL).is_some() {
        let _ = app.emit_to(POPUP_LABEL, EVENT_POPUP, payload);
        return;
    }

    // 主モニターの右下（タスクバーの少し上）に出す
    let (x, y) = app
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| {
            let scale = m.scale_factor();
            let size = m.size().to_logical::<f64>(scale);
            let pos = m.position().to_logical::<f64>(scale);
            (
                pos.x + size.width - POPUP_WIDTH - 16.0,
                pos.y + size.height - POPUP_HEIGHT - 64.0,
            )
        })
        .unwrap_or((80.0, 80.0));

    #[allow(unused_mut)]
    let mut builder = WebviewWindowBuilder::new(app, POPUP_LABEL, WebviewUrl::App("index.html#/popup".into()))
        .title("Kiroscribe")
        .inner_size(POPUP_WIDTH, POPUP_HEIGHT)
        .position(x, y)
        .resizable(false)
        .decorations(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false);
    // E2E テスト用のビルドでだけ、メインウィンドウと同じウェブビューの引数を渡す（src/e2e.rs の browser_args）
    #[cfg(feature = "e2e")]
    if let Some(args) = crate::e2e::browser_args() {
        builder = builder.additional_browser_args(&args);
    }
    let built = builder.build();
    if let Err(err) = built {
        eprintln!("ポップアップを表示できませんでした: {err}");
    }
}

/// 自動で始めた録音が、会議の終了を待たずに止まった（利用者がメインウィンドウで止めた）ときに、
/// ポップアップを停止の表示に変える。録音中の表示のまま残さない
/// （残ると、その「停止」が、あとから始めた別の録音を止めてしまう）。
pub fn popup_recording_stopped(app: &AppHandle, note_id: &str) {
    let background = app.state::<BackgroundState>();
    let shown = background.popup.lock().unwrap().clone();
    let Some(PopupPayload::Recording {
        app: label,
        note_id: shown_id,
        title,
    }) = shown
    else {
        return;
    };
    if shown_id != note_id {
        return;
    }
    // 録音のあいだにタイトルを直していたら、いまのタイトルを出す
    let title = app
        .state::<AppState>()
        .db()
        .get_note(note_id)
        .map(|note| note.title)
        .unwrap_or(title);
    let payload = PopupPayload::Stopped {
        app: label,
        note_id: shown_id,
        title,
    };
    if app.get_webview_window(POPUP_LABEL).is_some() {
        show_popup(app, payload);
    } else {
        // 利用者がポップアップを閉じていたら、出し直さない（自分で止めたことは分かっている）
        *background.popup.lock().unwrap() = Some(payload);
    }
}

pub fn close_popup_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(POPUP_LABEL) {
        let _ = window.close();
    }
}

#[tauri::command]
pub fn get_popup_state(background: State<'_, BackgroundState>) -> Option<PopupPayload> {
    background.popup.lock().unwrap().clone()
}

#[tauri::command]
pub fn close_popup(app: AppHandle) {
    close_popup_window(&app);
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenNoteEvent {
    note_id: String,
    enhance: bool,
}

/// ポップアップからノートを開く（`enhance` なら議事録の作成も始める）。
#[tauri::command]
pub fn open_note_from_popup(app: AppHandle, note_id: String, enhance: bool) {
    show_main(&app);
    let _ = app.emit_to(MAIN_LABEL, EVENT_OPEN_NOTE, OpenNoteEvent { note_id, enhance });
    close_popup_window(&app);
}

#[tauri::command]
pub fn open_settings_from_popup(app: AppHandle) {
    show_main(&app);
    let _ = app.emit_to(MAIN_LABEL, EVENT_OPEN_SETTINGS, ());
    close_popup_window(&app);
}

/// 設定の変更を、トレイのチェックと自動起動の登録に反映する。
pub fn apply_settings(app: &AppHandle, settings: &Settings) {
    if let Some(item) = app.state::<BackgroundState>().tray_auto_item.lock().unwrap().as_ref() {
        let _ = item.set_checked(settings.auto_record_meetings);
    }
    // 開発中の実行ファイルを自動起動に登録しないよう、リリースビルドだけで行う（E2E テスト用のビルドでも行わない）
    #[cfg(all(not(debug_assertions), not(feature = "e2e")))]
    {
        use tauri_plugin_autostart::ManagerExt;
        let autolaunch = app.autolaunch();
        let result = if settings.launch_at_login {
            autolaunch.enable()
        } else {
            autolaunch.disable()
        };
        if let Err(err) = result {
            eprintln!("自動起動の設定を変更できませんでした: {err}");
        }
    }
}

/// トレイのメニューの「会議を検出したら自動で録音」: 設定のオン・オフを入れ替えて保存する。
pub(crate) fn toggle_auto_record(app: &AppHandle) -> CmdResult<()> {
    let state = app.state::<AppState>();
    let settings = {
        let db = state.db();
        let mut settings = db.load_settings()?;
        settings.auto_record_meetings = !settings.auto_record_meetings;
        db.save_settings(settings)?
    };
    apply_settings(app, &settings);
    crate::meeting_watch::settings_changed(app, !settings.auto_record_meetings, &settings);
    Ok(())
}

/// 終了の前の片づけ: 録音中なら止めて、最後の発話の文字起こしが終わるまで待つ。
/// ここから先は、メインウィンドウを閉じる操作を「隠す」に読み替えない（本当に終了する）。
pub(crate) async fn prepare_quit(app: &AppHandle) {
    app.state::<BackgroundState>().quitting.store(true, Ordering::SeqCst);
    stop_now(app).await;
}

/// 終了する。録音中なら止めて、最後の発話の文字起こしを待ってから終わる。
pub fn quit(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        prepare_quit(&app).await;
        app.exit(0);
    });
}

pub fn setup_tray(app: &AppHandle, settings: &Settings) -> tauri::Result<()> {
    // E2E テスト用のビルドでだけ、トレイを作れない環境のふりができる（src/e2e.rs）
    #[cfg(feature = "e2e")]
    if crate::e2e::tray_unavailable() {
        return Err(tauri::Error::Io(std::io::Error::other("E2E: タスクトレイは使えないことにしています")));
    }
    let open = MenuItem::with_id(app, "open", "Kiroscribe を開く", true, None::<&str>)?;
    let auto = CheckMenuItem::with_id(
        app,
        "auto-record",
        "会議を検出したら自動で録音",
        true,
        settings.auto_record_meetings,
        None::<&str>,
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let quit_item = MenuItem::with_id(app, "quit", "Kiroscribe を終了", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &auto, &separator, &quit_item])?;
    *app.state::<BackgroundState>().tray_auto_item.lock().unwrap() = Some(auto);

    let mut builder = TrayIconBuilder::with_id("kiroscribe")
        .tooltip("Kiroscribe（会議の自動検出）")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "auto-record" => {
                if let Err(err) = toggle_auto_record(app) {
                    eprintln!("設定を保存できませんでした: {}", err.0);
                }
            }
            "quit" => quit(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}
