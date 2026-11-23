//! E2E テスト用の差し替え口。cargo の feature `e2e` を付けたときだけコンパイルされ、
//! リリース用のビルド（feature なし）には含まれない。
//!
//! 実際のアプリを起動して操作するテストでは、外部の API・音声デバイス・OS の機能を本物のまま使えない。
//! 何をどう差し替えるかは、環境変数 [`DIR_ENV`] が指す「作業フォルダ」で決める。テスト 1 件につき 1 つ用意する。
//! 環境変数がなければ、起動の引数 `--kiroscribe-e2e-dir=<作業フォルダ>`（[`DIR_ARG`]）を見る。
//! アプリ内更新のあと、インストーラはアプリを同じ引数で起動し直すが、環境変数は引き継がれないことがあるため
//! （管理者として動いたインストーラは、デスクトップの利用者の環境でアプリを起動する）。
//!
//! ```text
//! <作業フォルダ>/
//!   config.json     起動時に 1 回読む設定（[`Config`]。接続先・模擬の音声・待ち時間）。なければすべて既定のまま
//!   data/           アプリのデータ（データベース・API キー・ウェブビューのデータ）
//!   meeting.txt     いま会議中として扱う会議アプリ（例: googleMeet）。なし・空なら会議中ではない
//!   mic-error.txt   あるあいだは、マイクを開けなかったことにする（中身が理由の文）
//!   dialogs.json    確認ダイアログと保存ダイアログへの答え
//!   dialogs.log     アプリが出そうとしたダイアログの記録（1 行 1 件の JSON）
//!   clipboard.txt   アプリがクリップボードへ書こうとした文字
//!   opened-urls.log アプリが外部ブラウザで開こうとした URL
//!   app.pid         起動したアプリのプロセス ID
//!   launches.log    起動の記録（1 行 1 件の JSON。バージョン・プロセス ID・引数・ウィンドウを出したか）
//!   close-window.txt ここにウィンドウのラベル（main など）を書くと、利用者が「×」を押したのと同じように閉じる
//!   resize-window.txt ここに「ラベル 幅 高さ」（例: main 400 300）を書くと、そのウィンドウの大きさを変える
//!   window-size.json 大きさを変えたあとの、ウィンドウの大きさと、最小の大きさ（OS に問い合わせた値）
//! ```
//!
//! `meeting.txt` 以下は、アプリの実行中にテストから読み書きする
//! （`meeting.txt` と `mic-error.txt` 以外は Tauri 側の `e2e` モジュールが扱う）。

pub mod audio;
pub mod window;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;

use crate::meetings::MeetingApp;
use crate::retry::RetryPolicy;

/// 作業フォルダを指す環境変数。
pub const DIR_ENV: &str = "KIROSCRIBE_E2E_DIR";

/// 環境変数がないときに作業フォルダを指す、起動の引数（`--kiroscribe-e2e-dir=<作業フォルダ>`）。
pub const DIR_ARG: &str = "--kiroscribe-e2e-dir";

const CONFIG_FILE: &str = "config.json";
const DATA_DIR: &str = "data";
const MEETING_FILE: &str = "meeting.txt";

/// `config.json` の内容。
///
/// 接続先を書かなかった API は、どこにもつながらないアドレス（[`BLOCKED_URL`]）になる（本物の API を呼ばない）。
/// それ以外の項目は、書かなければ製品と同じ動きになる。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Config {
    /// OpenAI（文字起こし）の接続先。例: `http://127.0.0.1:4010/openai/v1`
    pub openai_base_url: Option<String>,
    /// Anthropic（議事録・チャット）の接続先。例: `http://127.0.0.1:4010/anthropic/v1`
    pub anthropic_base_url: Option<String>,
    /// 使い方（raw.githubusercontent.com）の接続先
    pub github_raw_base_url: Option<String>,
    /// リリース一覧（api.github.com）の接続先
    pub github_api_base_url: Option<String>,
    /// アプリ内更新の `latest.json` の URL
    pub update_endpoint: Option<String>,
    /// アプリ内更新の署名を確かめる公開鍵（`tauri.conf.json` の `plugins.updater.pubkey` と同じ形式）
    pub update_pubkey: Option<String>,
    /// メインウィンドウを、起動時の処理を待たずに最初から表示する（製品では、起動時の処理が済んでから表示する）。
    /// WebDriver でつながらないときの切り分け用
    pub main_visible_at_start: bool,
    /// タスクトレイのアイコンを作れなかったことにする（トレイが使えない環境でも起動することを確かめるため）
    pub tray_unavailable: bool,
    /// 設定の「Windows の起動時に Kiroscribe を起動する」に合わせて、Windows の自動起動に本当に登録する
    /// （インストールしたアプリを使うテスト用）。書かなければ、E2E テスト用のビルドは登録しない
    /// （製品の自動起動の登録を、テストのビルドで書き換えないため）
    pub register_autostart: bool,
    /// 起動できなかったときのダイアログを、記録に置き換えずに本当に出す
    /// （OS のダイアログが出て、閉じると終了することを、UI オートメーションで確かめるテスト用）
    pub native_startup_dialog: bool,
    /// 会議の検出を `meeting.txt` で模擬せず、製品と同じく Windows のマイクの使用状況とウィンドウのタイトルから行う
    /// （マイクを使うプログラムやブラウザを本当に動かして確かめるテスト用）
    pub real_meeting_detection: bool,
    pub audio: AudioConfig,
    pub timing: Timing,
}

/// 模擬の音声。`"speech"`（発話に似た合成音の繰り返し）・`"silence"`（無音）・WAV ファイルのパスのどれか。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct AudioConfig {
    /// マイク（自分）。指定すると実際の音声デバイスを使わなくなる
    pub mic: Option<String>,
    /// システム音声（相手）。`mic` を指定して、こちらを指定しなければ「相手の声は取り込めなかった」扱いになる
    pub system: Option<String>,
}

/// 待ち時間（ミリ秒）。テストを短く終わらせるために縮める。本来の値は、それぞれの単体テストで確かめている。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct Timing {
    /// 会議の検出を調べる間隔（本来は 2 秒）
    pub meeting_poll_ms: Option<u64>,
    /// 会議の開始を確定するまでの時間（本来は 4 秒）
    pub meeting_start_confirm_ms: Option<i64>,
    /// 会議が見えなくなってから終了とみなすまでの猶予（本来は 20 秒）
    pub meeting_end_grace_ms: Option<i64>,
    /// 起動してから最初に更新を確認するまでの時間（本来は 20 秒）
    pub update_first_check_ms: Option<u64>,
    /// API の再試行の最初の待ち時間（本来は 1 秒。以降は倍々で、上限はこの 8 倍）
    pub retry_base_delay_ms: Option<u64>,
}

struct State {
    dir: Option<PathBuf>,
    config: Config,
}

static STATE: OnceLock<Result<State, String>> = OnceLock::new();

/// 作業フォルダの指定を探す。環境変数を先に見て、なければ起動の引数（`--kiroscribe-e2e-dir=<パス>`）を見る。
pub fn find_dir(env: Option<OsString>, args: impl IntoIterator<Item = OsString>) -> Option<PathBuf> {
    if let Some(dir) = env.filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    let prefix = format!("{DIR_ARG}=");
    args.into_iter().find_map(|arg| {
        let arg = arg.into_string().ok()?;
        let dir = arg.strip_prefix(&prefix)?;
        (!dir.is_empty()).then(|| PathBuf::from(dir))
    })
}

fn load_from_env() -> Result<State, String> {
    let Some(dir) = find_dir(std::env::var_os(DIR_ENV), std::env::args_os().skip(1)) else {
        return Ok(State {
            dir: None,
            config: Config::default(),
        });
    };
    if !dir.is_absolute() {
        return Err(format!("作業フォルダ（{DIR_ENV}）は絶対パスで指定してください（現在: {}）", dir.display()));
    }
    std::fs::create_dir_all(&dir).map_err(|err| format!("{} を作成できません: {err}", dir.display()))?;
    let path = dir.join(CONFIG_FILE);
    let config = match std::fs::read_to_string(&path) {
        Ok(text) => parse_config(&text).map_err(|err| format!("{} を読めません: {err}", path.display()))?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(err) => return Err(format!("{} を読めません: {err}", path.display())),
    };
    Ok(State {
        dir: Some(dir),
        config,
    })
}

/// `config.json` を読む。知らない項目があればエラーにする（書き間違いで差し替えが効かないまま進むのを防ぐ）。
pub fn parse_config(text: &str) -> Result<Config, String> {
    serde_json::from_str(text).map_err(|err| err.to_string())
}

fn state() -> Result<&'static State, &'static str> {
    STATE.get_or_init(load_from_env).as_ref().map_err(String::as_str)
}

/// 環境変数の代わりに、作業フォルダと設定を直接与える（同じプロセスの中で動かす結合テスト用）。
/// 最初の 1 回だけ有効で、既に読み込み済みなら `false`。
pub fn install(dir: PathBuf, config: Config) -> bool {
    STATE
        .set(Ok(State {
            dir: Some(dir),
            config,
        }))
        .is_ok()
}

/// 作業フォルダ。環境変数がなければ `None`、設定を読めなければエラー。
pub fn dir() -> Result<Option<&'static Path>, &'static str> {
    Ok(state()?.dir.as_deref())
}

/// 設定。作業フォルダがない・設定を読めないときは、何も書かれていないものとして扱う。
pub fn config() -> &'static Config {
    static DEFAULT: OnceLock<Config> = OnceLock::new();
    match state() {
        Ok(state) => &state.config,
        Err(_) => DEFAULT.get_or_init(Config::default),
    }
}

/// アプリのデータ（データベース・API キー・ウェブビューのデータ）を置くフォルダ。
pub fn data_dir(dir: &Path) -> PathBuf {
    dir.join(DATA_DIR)
}

/// 作業フォルダの中のファイル。作業フォルダがなければ `None`。
pub fn file(name: &str) -> Option<PathBuf> {
    dir().ok().flatten().map(|dir| dir.join(name))
}

/// 接続先を指定しなかったときに使う、どこにもつながらないアドレス。
/// 作業フォルダを使っているあいだは、設定の書き忘れがあっても本物の API を呼ばない。
pub const BLOCKED_URL: &str = "http://127.0.0.1:9";

fn trimmed(url: &Option<String>) -> Option<String> {
    url.as_deref()
        .map(|url| url.trim().trim_end_matches('/').to_string())
        .filter(|url| !url.is_empty())
}

/// 差し替え後の接続先。作業フォルダを使っていなければ `None`（製品の接続先を使う）。
fn resolve_endpoint(in_work_dir: bool, configured: &Option<String>) -> Option<String> {
    in_work_dir.then(|| trimmed(configured).unwrap_or_else(|| BLOCKED_URL.to_string()))
}

fn endpoint(configured: &Option<String>) -> Option<String> {
    resolve_endpoint(dir().ok().flatten().is_some(), configured)
}

pub fn openai_base_url() -> Option<String> {
    endpoint(&config().openai_base_url)
}

pub fn anthropic_base_url() -> Option<String> {
    endpoint(&config().anthropic_base_url)
}

/// 使い方（raw）とリリース一覧（API）の接続先。
pub fn github_base_urls() -> (Option<String>, Option<String>) {
    let config = config();
    (endpoint(&config.github_raw_base_url), endpoint(&config.github_api_base_url))
}

/// アプリ内更新の `latest.json` の URL。
pub fn update_endpoint() -> Option<String> {
    let configured = config().update_endpoint.as_deref().map(str::trim).filter(|url| !url.is_empty());
    dir().ok().flatten()?;
    Some(configured.map_or_else(|| format!("{BLOCKED_URL}/latest.json"), str::to_string))
}

fn shortened_retry(timing: &Timing, default: &RetryPolicy) -> Option<RetryPolicy> {
    let base = Duration::from_millis(timing.retry_base_delay_ms?);
    Some(RetryPolicy {
        max_attempts: default.max_attempts,
        base_delay: base,
        max_delay: base.saturating_mul(8),
    })
}

/// 再試行の待ち時間を縮めた設定。指定がなければ `None`（本来の値を使う）。回数は変えない。
pub fn retry_policy(default: &RetryPolicy) -> Option<RetryPolicy> {
    shortened_retry(&config().timing, default)
}

/// 会議の検出の差し替え。作業フォルダがなければ `None`（実際の検出を使う）。
/// あれば実際の検出は行わず、`meeting.txt` に書かれた会議アプリの会議中として扱う（なし・空なら会議中ではない）。
/// `config.json` の `realMeetingDetection` が true なら `None`（実際の検出を使う）。
pub fn simulated_meeting() -> Option<Option<MeetingApp>> {
    if config().real_meeting_detection {
        return None;
    }
    let path = file(MEETING_FILE)?;
    Some(std::fs::read_to_string(path).ok().and_then(|text| parse_meeting(&text)))
}

/// `meeting.txt` の内容（`googleMeet`・`slackHuddle`・`zoom`・`teams`・`discord`）を読む。
pub fn parse_meeting(text: &str) -> Option<MeetingApp> {
    let name = text.trim();
    if name.is_empty() {
        return None;
    }
    match serde_json::from_value(serde_json::Value::String(name.to_string())) {
        Ok(app) => Some(app),
        Err(_) => {
            eprintln!("[e2e] {MEETING_FILE} の会議アプリ名が正しくありません: {name}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    /// 環境変数を先に使い、なければ引数を使う（アプリ内更新のあと、インストーラが引数だけを引き継いで起動し直すため）。
    /// [UPD-018]
    #[test]
    fn the_work_directory_comes_from_the_environment_or_the_arguments() {
        let work = if cfg!(windows) { r"C:\work\e2e" } else { "/work/e2e" };
        let other = if cfg!(windows) { r"C:\other" } else { "/other" };
        assert_eq!(find_dir(None, args(&[])), None);
        assert_eq!(find_dir(None, args(&["--autostart"])), None);
        assert_eq!(find_dir(Some(work.into()), args(&[])), Some(PathBuf::from(work)));
        let from_arg = format!("--kiroscribe-e2e-dir={work}");
        assert_eq!(find_dir(None, args(&["--autostart", &from_arg])), Some(PathBuf::from(work)));
        assert_eq!(
            find_dir(Some(other.into()), args(&[&from_arg])),
            Some(PathBuf::from(other)),
            "環境変数があれば、そちらを使う"
        );
        assert_eq!(find_dir(Some("".into()), args(&[&from_arg])), Some(PathBuf::from(work)), "空の環境変数は無いのと同じ");
        assert_eq!(find_dir(None, args(&["--kiroscribe-e2e-dir="])), None);
        assert_eq!(find_dir(None, args(&["--kiroscribe-e2e-dir", work])), None, "= でつないだ形だけを受け付ける");
    }

    #[test]
    fn config_reads_only_what_is_written() {
        assert_eq!(parse_config("{}").unwrap(), Config::default());

        let config = parse_config(
            r#"{
                "openaiBaseUrl": "http://127.0.0.1:4010/openai/v1/",
                "updatePubkey": "KEY",
                "audio": { "mic": "speech" },
                "timing": { "meetingEndGraceMs": 3000, "retryBaseDelayMs": 20 }
            }"#,
        )
        .unwrap();
        assert_eq!(config.openai_base_url.as_deref(), Some("http://127.0.0.1:4010/openai/v1/"));
        assert_eq!(config.anthropic_base_url, None);
        assert_eq!(config.update_pubkey.as_deref(), Some("KEY"));
        assert_eq!(config.audio.mic.as_deref(), Some("speech"));
        assert_eq!(config.audio.system, None);
        assert_eq!(config.timing.meeting_end_grace_ms, Some(3000));
        assert_eq!(config.timing.meeting_poll_ms, None);
        // 末尾の / は取り除いて使う
        assert_eq!(trimmed(&config.openai_base_url).as_deref(), Some("http://127.0.0.1:4010/openai/v1"));
        assert_eq!(trimmed(&Some("  ".into())), None);
    }

    /// 書き間違えた項目を黙って無視しない（差し替えたつもりで本物の API を呼ぶ事故を防ぐ）。
    #[test]
    fn unknown_keys_are_rejected() {
        assert!(parse_config(r#"{"openAiBaseUrl": "http://127.0.0.1:1"}"#).is_err());
        assert!(parse_config(r#"{"timing": {"meetingGraceMs": 1}}"#).is_err());
        assert!(parse_config(r#"{"audio": {"speaker": "speech"}}"#).is_err());
        assert!(parse_config("not json").is_err());
    }

    /// 作業フォルダを使っているあいだは、接続先を書き忘れても本物の API には向かない。
    #[test]
    fn unconfigured_endpoints_are_blocked_inside_a_work_directory() {
        let configured = Some("http://127.0.0.1:4010/openai/v1/".to_string());
        assert_eq!(
            resolve_endpoint(true, &configured).as_deref(),
            Some("http://127.0.0.1:4010/openai/v1")
        );
        assert_eq!(resolve_endpoint(true, &None).as_deref(), Some(BLOCKED_URL));
        assert_eq!(resolve_endpoint(true, &Some(" ".into())).as_deref(), Some(BLOCKED_URL));
        // 作業フォルダがなければ差し替えない（feature を付けた単体テストなどは、製品と同じ接続先のまま）
        assert_eq!(resolve_endpoint(false, &configured), None);
        assert_eq!(resolve_endpoint(false, &None), None);
    }

    #[test]
    fn retry_delays_shrink_but_attempts_stay() {
        let default = RetryPolicy {
            max_attempts: 4,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(8),
        };
        assert!(shortened_retry(&Timing::default(), &default).is_none(), "指定がなければ本来の値");

        let timing = Timing {
            retry_base_delay_ms: Some(20),
            ..Timing::default()
        };
        let shortened = shortened_retry(&timing, &default).unwrap();
        assert_eq!(shortened.max_attempts, 4, "回数は変えない（何回目であきらめるかも確かめたいので）");
        let delays: Vec<u128> = (0..5).map(|i| shortened.delay_after(i).as_millis()).collect();
        assert_eq!(delays, [20, 40, 80, 160, 160]);
    }

    #[test]
    fn meeting_file_names_a_meeting_app() {
        assert_eq!(parse_meeting("googleMeet\n"), Some(MeetingApp::GoogleMeet));
        assert_eq!(parse_meeting("  slackHuddle "), Some(MeetingApp::SlackHuddle));
        assert_eq!(parse_meeting("zoom"), Some(MeetingApp::Zoom));
        assert_eq!(parse_meeting("teams"), Some(MeetingApp::Teams));
        assert_eq!(parse_meeting("discord"), Some(MeetingApp::Discord));
        assert_eq!(parse_meeting(""), None);
        assert_eq!(parse_meeting("  \n"), None);
        assert_eq!(parse_meeting("Google Meet"), None, "表示名ではなく識別子で書く");
    }
}
