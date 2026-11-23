//! E2E テスト用の差し替え口のうち、Tauri に関わる部分。feature `e2e` を付けたときだけコンパイルされ、
//! リリース用のビルドには含まれない。作業フォルダと設定の読み込みは `kiroscribe_core::e2e` にある。
//!
//! ここで差し替えるもの:
//! - データの場所と、アプリの識別子（多重起動の判定に使われる）
//! - アプリ内更新の配布元と公開鍵
//! - OS の画面や機能に出ていくもの（確認・保存のダイアログ、クリップボード、外部ブラウザ）
//! - 会議の検出と、待ち時間

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use kiroscribe_core::e2e as core;
use kiroscribe_core::meetings::{Detection, MeetingTracker};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::utils::config::AppDirectoriesOverride;
use tauri::{Context, Manager, Runtime};

const DIALOG_ANSWERS_FILE: &str = "dialogs.json";
const DIALOG_LOG_FILE: &str = "dialogs.log";
const CLIPBOARD_FILE: &str = "clipboard.txt";
const OPENED_URLS_FILE: &str = "opened-urls.log";
const PID_FILE: &str = "app.pid";
const CLOSE_WINDOW_FILE: &str = "close-window.txt";

/// 作業フォルダごとに変えるアプリの識別子の先頭。製品の識別子（`dev.kiroscribe.desktop`）とは重ならない。
const IDENTIFIER_PREFIX: &str = "dev.kiroscribe.e2e";

/// 起動の前に、アプリの設定（`tauri.conf.json` の内容）を E2E テスト用に書き換える。
///
/// 作業フォルダ（環境変数 `KIROSCRIBE_E2E_DIR`）がなければ起動しない。
/// E2E テスト用のビルドを、利用者の本物のデータや本物の API に向けて動かさないため。
pub fn prepare<R: Runtime>(context: &mut Context<R>) {
    let dir = match core::dir() {
        Ok(Some(dir)) => dir,
        Ok(None) => exit_with(&format!(
            "E2E テスト用のビルドです。作業フォルダを環境変数 {} で指定してください",
            core::DIR_ENV
        )),
        Err(message) => exit_with(message),
    };
    apply(
        context.config_mut(),
        dir,
        core::update_endpoint(),
        core::config().update_pubkey.as_deref(),
    );
    // Windows のドライバー（msedgedriver）が環境変数で渡すウェブビューの引数を、ウィンドウの設定に入れる
    if let Some(args) = browser_args() {
        eprintln!("[e2e] ウェブビューの追加の引数: {args}");
        for window in &mut context.config_mut().app.windows {
            window.additional_browser_args = Some(args.clone());
        }
    }
    if core::config().main_visible_at_start {
        for window in &mut context.config_mut().app.windows {
            window.visible = true;
        }
    }
    log_environment(dir);
}

/// ウェブビュー（WebView2）に追加の引数を渡すための環境変数。Windows のドライバー（msedgedriver）は、
/// これでデバッグ用のポートを開かせて（`--remote-debugging-port=0`）、アプリの画面につなぐ。
const BROWSER_ARGS_ENV: &str = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS";

/// Tauri（wry）が、指定のないときにウェブビューへ渡す引数。
/// 製品のビルドでは wry がこのとおりに渡す（wry の `webview2` の実装と、製品の起動引数で確認した値）。
const DEFAULT_BROWSER_ARGS: &str =
    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --autoplay-policy=no-user-gesture-required";

/// E2E テストのときにウェブビューへ渡す引数。環境変数に引数がなければ `None`（製品と同じく、wry の既定に任せる）。
///
/// wry は、既定の引数をウェブビューの「オプション」として必ず渡す。WebView2 は、オプションの引数があると環境変数の
/// 引数を使わないので、そのままではドライバーの引数（デバッグ用のポートなど）が届かず、WebDriver でつながらない。
/// そこで、既定の引数と環境変数の引数を合わせたものを、こちらから明示して渡す。
/// 同じデータフォルダを使うウェブビューは引数をそろえる必要があるので、ポップアップにも同じものを渡す。
pub fn browser_args() -> Option<String> {
    let from_env = std::env::var(BROWSER_ARGS_ENV).ok()?;
    (!from_env.trim().is_empty()).then(|| merge_browser_args(DEFAULT_BROWSER_ARGS, &from_env))
}

/// 2 組の引数を 1 つにまとめる。`--disable-features=` と `--enable-features=` は、複数あると最後の 1 つしか
/// 効かないので、値（機能名の一覧）を 1 つにまとめる。それ以外は、同じものを除いて、出てきた順に並べる。
fn merge_browser_args(defaults: &str, extra: &str) -> String {
    const LISTS: [&str; 2] = ["--disable-features=", "--enable-features="];
    let mut features: [Vec<&str>; 2] = [Vec::new(), Vec::new()];
    let mut list_position: [Option<usize>; 2] = [None, None];
    let mut merged: Vec<String> = Vec::new();
    for arg in defaults.split_whitespace().chain(extra.split_whitespace()) {
        match LISTS.iter().position(|prefix| arg.starts_with(prefix)) {
            Some(list) => {
                for feature in arg[LISTS[list].len()..].split(',').filter(|feature| !feature.is_empty()) {
                    if !features[list].contains(&feature) {
                        features[list].push(feature);
                    }
                }
                if list_position[list].is_none() {
                    // あとで、まとめた値に置き換える
                    list_position[list] = Some(merged.len());
                    merged.push(String::new());
                }
            }
            None => {
                if !merged.iter().any(|known| known == arg) {
                    merged.push(arg.to_string());
                }
            }
        }
    }
    for list in 0..LISTS.len() {
        if let Some(position) = list_position[list] {
            merged[position] = format!("{}{}", LISTS[list], features[list].join(","));
        }
    }
    merged.join(" ")
}

/// WebDriver でつながらないときに原因を追えるよう、起動のしかたをログに残す。
/// Windows のドライバー（msedgedriver）は、環境変数でウェブビューにデバッグ用のポートを開かせる。
fn log_environment(dir: &Path) {
    let args: Vec<String> = std::env::args().skip(1).collect();
    eprintln!("[e2e] 引数: {args:?}");
    for name in [
        BROWSER_ARGS_ENV,
        "WEBVIEW2_USER_DATA_FOLDER",
        "WEBVIEW2_BROWSER_EXECUTABLE_FOLDER",
        "TAURI_WEBVIEW_AUTOMATION",
    ] {
        match std::env::var(name) {
            Ok(value) => eprintln!("[e2e] 環境変数 {name}={value}"),
            Err(_) => eprintln!("[e2e] 環境変数 {name} はありません"),
        }
    }
    eprintln!("[e2e] データフォルダ: {}", core::data_dir(dir).display());
}

fn exit_with(message: &str) -> ! {
    eprintln!("[e2e] {message}");
    std::process::exit(2);
}

fn apply(config: &mut tauri::Config, dir: &Path, update_endpoint: Option<String>, update_pubkey: Option<&str>) {
    // データベース・API キー・ウェブビューのデータを、すべて作業フォルダの中に置く
    config.app.app_directories_override = Some(AppDirectoriesOverride::Root(core::data_dir(dir)));
    // 多重起動の防止は識別子ごとに働く。作業フォルダごとに変えて、同じ作業フォルダを使う 2 つ目の起動だけを止める
    // （製品版や、並行して走るほかのテストとは干渉しない）
    config.identifier = identifier_for(dir);

    // アプリ内更新: 配布元と公開鍵を差し替える。模擬サーバーは http なので、https でない配布元も許す
    let updater = config
        .plugins
        .0
        .entry("updater".to_string())
        .or_insert_with(|| json!({}));
    if let Some(endpoint) = update_endpoint {
        updater["endpoints"] = json!([endpoint]);
        updater["dangerousInsecureTransportProtocol"] = json!(true);
    }
    if let Some(pubkey) = update_pubkey {
        updater["pubkey"] = json!(pubkey);
    }
}

/// 作業フォルダのパスから決まる識別子（英数字とピリオドだけ。各部分は英字で始める）。
fn identifier_for(dir: &Path) -> String {
    // FNV-1a（64bit）
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in dir.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{IDENTIFIER_PREFIX}.d{hash:016x}")
}

fn append_line(file: &str, line: &str) {
    let Some(path) = core::file(file) else { return };
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{line}"));
    if let Err(err) = written {
        eprintln!("[e2e] {} に書けません: {err}", path.display());
    }
}

// ---- ダイアログ・クリップボード ----

/// `dialogs.json` の内容。テストが、利用者の代わりにダイアログへ答える。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
struct DialogAnswers {
    /// 確認ダイアログに「はい」と答えるか（書かなければ「いいえ」）
    confirm: bool,
    /// 保存ダイアログで選ぶパス（書かなければ取り消し）
    save_path: Option<String>,
}

fn dialog_answers() -> DialogAnswers {
    let Some(path) = core::file(DIALOG_ANSWERS_FILE) else {
        return DialogAnswers::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
            eprintln!("[e2e] {} を読めません: {err}", path.display());
            DialogAnswers::default()
        }),
        Err(_) => DialogAnswers::default(),
    }
}

/// メッセージダイアログのボタン（`@tauri-apps/plugin-dialog` が送る形）から、
/// 「はい」にあたるボタンと「いいえ」にあたるボタンの名前を決める。押されたボタンの名前が、画面への戻り値になる。
fn button_labels(buttons: Option<&Value>) -> (String, String) {
    let pair = |yes: &str, no: &str| (yes.to_string(), no.to_string());
    match buttons {
        None | Some(Value::Null) => pair("Ok", "Ok"),
        Some(Value::String(preset)) => match preset.as_str() {
            "OkCancel" => pair("Ok", "Cancel"),
            "YesNo" | "YesNoCancel" => pair("Yes", "No"),
            _ => pair("Ok", "Ok"),
        },
        Some(custom) => {
            let label = |value: Option<&Value>| value.and_then(Value::as_str).unwrap_or("Ok").to_string();
            if let Some(ok) = custom.get("OkCustom") {
                let ok = label(Some(ok));
                (ok.clone(), ok)
            } else if let Some(labels) = custom.get("OkCancelCustom").or_else(|| custom.get("YesNoCancelCustom")) {
                (label(labels.get(0)), label(labels.get(1)))
            } else {
                pair("Ok", "Ok")
            }
        }
    }
}

/// `plugin:dialog|message`（確認ダイアログ）の代わり。OS のダイアログは出さず、`dialogs.json` の答えを返す。
#[tauri::command]
fn message(message: String, title: Option<String>, kind: Option<String>, buttons: Option<Value>) -> String {
    let (yes, no) = button_labels(buttons.as_ref());
    let answer = if dialog_answers().confirm { yes } else { no };
    append_line(
        DIALOG_LOG_FILE,
        &json!({ "type": "message", "message": message, "title": title, "kind": kind, "answer": answer }).to_string(),
    );
    answer
}

/// `plugin:dialog|save`（保存ダイアログ）の代わり。`dialogs.json` のパスを返す。
#[tauri::command]
fn save(options: Value) -> Option<String> {
    let answer = dialog_answers().save_path;
    append_line(
        DIALOG_LOG_FILE,
        &json!({ "type": "save", "options": options, "answer": answer }).to_string(),
    );
    answer
}

/// `plugin:clipboard-manager|write_text` の代わり。OS のクリップボードには触れず、`clipboard.txt` に書く。
#[tauri::command]
fn write_text(text: String) -> Result<(), String> {
    let path = core::file(CLIPBOARD_FILE).ok_or("作業フォルダがありません")?;
    std::fs::write(&path, text).map_err(|err| format!("{} に書けません: {err}", path.display()))
}

fn dialog_plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("dialog")
        .invoke_handler(tauri::generate_handler![message, save])
        .build()
}

fn clipboard_plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("clipboard-manager")
        .invoke_handler(tauri::generate_handler![write_text])
        .build()
}

/// テストからアプリを操作するための窓口。
///
/// - 起動したら、プロセス ID を `app.pid` に書く。多重起動の判定（先に登録されているプラグイン）を通ったあとに
///   動くので、書くのは 1 つ目の起動だけ
/// - `close-window.txt` にウィンドウのラベル（`main` など）が書かれたら、そのウィンドウを閉じる。
///   利用者がウィンドウの「×」を押したのと同じ扱いになる（WebDriver からは「×」を押せないため）
fn control_plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("e2e-control")
        // 起動に失敗したときに、どこまで進んだかをログから追えるようにする
        .on_page_load(|webview, payload| {
            eprintln!("[e2e] ページの読み込み（{}）: {:?} {}", webview.label(), payload.event(), payload.url());
        })
        .setup(|app, _api| {
            eprintln!("[e2e] 起動しました（pid {}）", std::process::id());
            if let Some(path) = core::file(PID_FILE) {
                std::fs::write(path, std::process::id().to_string())?;
            }
            let app = app.clone();
            std::thread::Builder::new()
                .name("kiroscribe-e2e-control".into())
                .spawn(move || loop {
                    std::thread::sleep(Duration::from_millis(100));
                    let Some(path) = core::file(CLOSE_WINDOW_FILE) else { return };
                    let Ok(label) = std::fs::read_to_string(&path) else { continue };
                    let _ = std::fs::remove_file(&path);
                    match app.get_webview_window(label.trim()) {
                        Some(window) => {
                            if let Err(err) = window.close() {
                                eprintln!("[e2e] ウィンドウ {} を閉じられません: {err}", label.trim());
                            }
                        }
                        None => eprintln!("[e2e] ウィンドウ {} がありません", label.trim()),
                    }
                })?;
            Ok(())
        })
        .build()
}

/// ダイアログとクリップボードのプラグインを、E2E テスト用のものに置き換える
/// （同じ名前のプラグインは後から登録したほうが使われる。画面側のコードと権限の設定は製品のまま）。
/// あわせて、テストからアプリを操作するための窓口を足す。
pub fn plugins<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        .plugin(dialog_plugin())
        .plugin(clipboard_plugin())
        .plugin(control_plugin())
}

/// 起動時の処理（`lib.rs` の setup）が終わったことを、ログに残す。
pub fn log_setup_done(main_shown: bool) {
    eprintln!("[e2e] 起動時の処理が終わりました（メインウィンドウ: {}）", if main_shown { "表示" } else { "非表示" });
}

/// 外部ブラウザは開かず、開こうとした URL を `opened-urls.log` に記録する。
pub fn record_opened_url(url: &str) {
    append_line(OPENED_URLS_FILE, url);
}

// ---- 会議の検出・待ち時間 ----

/// 会議の検出を調べる間隔。指定がなければ `None`（本来の値を使う）。
pub fn meeting_poll_interval() -> Option<Duration> {
    core::config().timing.meeting_poll_ms.map(Duration::from_millis)
}

/// 会議の開始・終了を確定するまでの時間を、指定があれば縮める。
pub fn tune_tracker(tracker: &mut MeetingTracker) {
    let timing = &core::config().timing;
    if let Some(ms) = timing.meeting_start_confirm_ms {
        tracker.start_confirm_ms = ms;
    }
    if let Some(ms) = timing.meeting_end_grace_ms {
        tracker.end_grace_ms = ms;
    }
}

/// 会議の検出の差し替え。実際の検出（マイクの使用状況）の代わりに、`meeting.txt` の内容を「いまの状態」とする。
/// 戻り値は `MeetingTracker::tick` に渡す（始まりそうな会議, 進行中の会議が続いているか）。
pub fn simulated_detection() -> Option<(Option<Detection>, bool)> {
    let meeting = core::simulated_meeting()?;
    Some((meeting.map(|app| Detection { app, via_browser: false }), meeting.is_some()))
}

/// タスクトレイのアイコンを作れなかったことにするか。
pub fn tray_unavailable() -> bool {
    core::config().tray_unavailable
}

/// 起動してから最初に更新を確認するまでの時間。指定がなければ `None`（本来の値を使う）。
pub fn update_first_check_delay() -> Option<Duration> {
    core::config().timing.update_first_check_ms.map(Duration::from_millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ドライバーが環境変数で渡す引数は、Tauri の既定の引数と合わせて 1 つにする。
    #[test]
    fn driver_arguments_are_merged_with_the_defaults() {
        // msedgedriver が実際に渡す引数（抜粋）
        let from_driver = "--allow-pre-commit-input --disable-features=IgnoreDuplicateNavs,Prewarm --enable-automation \
                           --remote-debugging-port=0 --test-type=webdriver";
        let merged = merge_browser_args(DEFAULT_BROWSER_ARGS, from_driver);
        assert_eq!(
            merged,
            "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,IgnoreDuplicateNavs,Prewarm \
             --autoplay-policy=no-user-gesture-required --allow-pre-commit-input --enable-automation \
             --remote-debugging-port=0 --test-type=webdriver"
        );
        // --disable-features は 1 つだけ（複数あると、最後の 1 つしか効かない）
        assert_eq!(merged.matches("--disable-features=").count(), 1);
        for kept in ["msSmartScreenProtection", "Prewarm", "--remote-debugging-port=0", "--autoplay-policy=no-user-gesture-required"] {
            assert!(merged.contains(kept), "{kept} がなくなっている: {merged}");
        }

        // 同じ引数・同じ機能名は 1 回だけ。--enable-features も同じようにまとめる
        assert_eq!(
            merge_browser_args(
                "--disable-features=A,B --enable-features=X --flag",
                "--flag --enable-features=Y,X --disable-features=B,C --other=1"
            ),
            "--disable-features=A,B,C --enable-features=X,Y --flag --other=1"
        );
        // 片方にしかないときは、そのまま
        assert_eq!(merge_browser_args(DEFAULT_BROWSER_ARGS, ""), DEFAULT_BROWSER_ARGS);
        assert_eq!(merge_browser_args("", "  --remote-debugging-port=0  "), "--remote-debugging-port=0");
        assert_eq!(merge_browser_args("--a", "--enable-features=X"), "--a --enable-features=X");
    }

    #[test]
    fn identifier_depends_on_the_directory() {
        let a = identifier_for(Path::new("/tmp/e2e/a"));
        let b = identifier_for(Path::new("/tmp/e2e/b"));
        assert_ne!(a, b);
        assert_eq!(a, identifier_for(Path::new("/tmp/e2e/a")), "同じ作業フォルダなら同じ（2 つ目の起動を止めるため）");
        // D-Bus の名前としても使われる: 各部分は英字で始まり、英数字だけ
        assert!(a.starts_with("dev.kiroscribe.e2e.d"));
        assert!(a.split('.').all(|part| {
            part.starts_with(|c: char| c.is_ascii_alphabetic()) && part.chars().all(|c| c.is_ascii_alphanumeric())
        }));
    }

    // [UPD-018]
    #[test]
    fn config_is_redirected_into_the_work_directory() {
        let mut config: tauri::Config = serde_json::from_value(json!({
            "identifier": "dev.kiroscribe.desktop",
            "plugins": { "updater": { "pubkey": "REAL", "endpoints": ["https://example.com/latest.json"] } }
        }))
        .unwrap();
        let dir = Path::new("/tmp/e2e/work");
        apply(&mut config, dir, Some("http://127.0.0.1:4010/update/latest.json".into()), Some("TEST"));

        assert_eq!(
            config.app.app_directories_override,
            Some(AppDirectoriesOverride::Root(dir.join("data")))
        );
        assert_eq!(config.identifier, identifier_for(dir));
        let updater = &config.plugins.0["updater"];
        assert_eq!(updater["endpoints"], json!(["http://127.0.0.1:4010/update/latest.json"]));
        assert_eq!(updater["pubkey"], "TEST");
        assert_eq!(updater["dangerousInsecureTransportProtocol"], true);

        // 公開鍵を指定しなければ、製品の公開鍵のまま（模擬の配布元の更新は、署名が合わず拒否される）
        let mut config: tauri::Config = serde_json::from_value(json!({
            "identifier": "dev.kiroscribe.desktop",
            "plugins": { "updater": { "pubkey": "REAL" } }
        }))
        .unwrap();
        apply(&mut config, dir, Some("http://127.0.0.1:9/latest.json".into()), None);
        assert_eq!(config.plugins.0["updater"]["pubkey"], "REAL");
    }

    #[test]
    fn dialog_buttons_map_to_the_label_the_frontend_compares() {
        let labels = |buttons: Value| button_labels(Some(&buttons));
        // ask() は YesNo を送り、戻り値が "Yes" かどうかで判定する
        assert_eq!(labels(json!("YesNo")), ("Yes".into(), "No".into()));
        assert_eq!(labels(json!("OkCancel")), ("Ok".into(), "Cancel".into()));
        assert_eq!(labels(json!({ "OkCancelCustom": ["更新する", "あとで"] })), ("更新する".into(), "あとで".into()));
        assert_eq!(labels(json!({ "OkCustom": "閉じる" })), ("閉じる".into(), "閉じる".into()));
        assert_eq!(button_labels(None), ("Ok".into(), "Ok".into()));
    }
}
