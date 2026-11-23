//! 常駐まわり: タスクトレイ、閉じてもトレイに残る動作、Windows 起動時の自動起動、会議検出のポップアップ。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use kiroscribe_core::settings::Settings;
use serde::Serialize;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, PhysicalPosition, PhysicalSize, State, WebviewUrl, WebviewWindowBuilder};

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
/// 設定が、設定画面の外（タスクトレイのメニュー）で変わった（開いている設定画面が追従できるように）
pub const EVENT_SETTINGS_CHANGED: &str = "settings-changed";
/// Windows 起動時の自動起動で付ける引数。付いていればウィンドウを出さずトレイで起動する
pub const AUTOSTART_ARG: &str = "--autostart";

const POPUP_WIDTH: f64 = 380.0;
const POPUP_HEIGHT: f64 = 128.0;
/// ポップアップと、モニターの右端・下端とのあいだ（下は、タスクバーにかからないように広くとる）
const POPUP_MARGIN_RIGHT: f64 = 16.0;
const POPUP_MARGIN_BOTTOM: f64 = 64.0;

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
    // 起動に失敗して、理由を知らせるダイアログを出しているあいだは、使えない画面を出さない
    if app.try_state::<AppState>().is_none() {
        return;
    }
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

    // 主モニターの右下（タスクバーの少し上）に出す。位置は、見えている部分の左上で決める
    let monitor = app.primary_monitor().ok().flatten();
    let target = monitor.as_ref().map(|m| popup_position(m.position(), m.size(), m.scale_factor()));
    let (x, y) = match (&monitor, target) {
        (Some(m), Some(target)) => {
            let logical = target.to_logical::<f64>(m.scale_factor());
            (logical.x, logical.y)
        }
        _ => (80.0, 80.0),
    };

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
    match builder.build() {
        Ok(window) => {
            // Windows では、ウィンドウの位置は、見えない枠（左右と下に数 px）を含めた外側で決まる。
            // そのままだと、見えている部分が意図より右下に寄るので、枠のぶんだけ戻す
            if let (Some(target), Ok(outer), Ok(inner)) = (target, window.outer_position(), window.inner_position()) {
                let frame = (inner.x - outer.x, inner.y - outer.y);
                if frame != (0, 0) {
                    let _ = window.set_position(PhysicalPosition::new(target.x - frame.0, target.y - frame.1));
                }
            }
        }
        Err(err) => eprintln!("ポップアップを表示できませんでした: {err}"),
    }
}

/// ポップアップの見えている部分の左上の位置（物理ピクセル）: モニターの右端から 16px、下端から 64px 内側に、
/// 幅 380px・高さ 128px のポップアップが収まる位置。px は、表示の拡大率を掛ける前の値（拡大率 150% なら 1.5 倍になる）。
fn popup_position(monitor_position: &PhysicalPosition<i32>, monitor_size: &PhysicalSize<u32>, scale: f64) -> PhysicalPosition<i32> {
    let from_right = ((POPUP_WIDTH + POPUP_MARGIN_RIGHT) * scale).round() as i32;
    let from_bottom = ((POPUP_HEIGHT + POPUP_MARGIN_BOTTOM) * scale).round() as i32;
    PhysicalPosition::new(
        monitor_position.x + monitor_size.width as i32 - from_right,
        monitor_position.y + monitor_size.height as i32 - from_bottom,
    )
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
    // 開いている設定画面の表示を合わせる（古い表示のまま保存されて、切り替えが元に戻らないように）
    let _ = app.emit(EVENT_SETTINGS_CHANGED, &settings);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// ポップアップの位置は、表示の拡大率が変わっても、見た目で右端から 16px・下端から 64px 内側になる。
    // [POP-001]
    #[test]
    fn the_popup_sits_at_the_bottom_right_of_the_primary_monitor_at_any_scale() {
        let origin = PhysicalPosition::new(0, 0);
        // 100%: 1920x1080
        let at = popup_position(&origin, &PhysicalSize::new(1920, 1080), 1.0);
        assert_eq!((at.x, at.y), (1920 - 380 - 16, 1080 - 128 - 64));
        // 150%: 物理ピクセルでは、ポップアップも余白も 1.5 倍
        let at = popup_position(&origin, &PhysicalSize::new(2880, 1620), 1.5);
        assert_eq!((at.x, at.y), (2880 - 570 - 24, 1620 - 192 - 96));
        let logical = at.to_logical::<f64>(1.5);
        assert_eq!((logical.x, logical.y), (1920.0 - 380.0 - 16.0, 1080.0 - 128.0 - 64.0), "見た目の位置は 100% のときと同じ");
        // 125%: 端数は丸める（1px 以内）
        let at = popup_position(&origin, &PhysicalSize::new(1920, 1080), 1.25);
        assert_eq!((at.x, at.y), (1920 - 495, 1080 - 240));
        // 主モニターが原点にないとき（左に別のモニターがある、など）
        let at = popup_position(&PhysicalPosition::new(-1280, 200), &PhysicalSize::new(1280, 720), 1.0);
        assert_eq!((at.x, at.y), (-1280 + 1280 - 396, 200 + 720 - 192));
    }
}
