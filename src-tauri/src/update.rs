//! アプリ内更新。新しい版の確認（起動の少し後と、その後は定期的に）、見つけた更新の保持、
//! ダウンロード → 署名の確認 → インストールを行う。
//!
//! 確認とダウンロードは `tauri-plugin-updater` に任せる（配布元と公開鍵は `tauri.conf.json`）。
//!
//! 設定「新しいバージョンを自動でインストールする」がオンのときは、見つけた更新を先にダウンロードして
//! 署名を確かめておき、邪魔にならないとき（[`InstallConditions`]）にインストールする。
//! オフのときに自動で行うのは確認までで、インストールは利用者の操作（`install_update`）で始める。

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use kiroscribe_core::settings::Settings;
use kiroscribe_core::time::now_ms;
use kiroscribe_core::Database;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_updater::{Update, Updater, UpdaterExt};
use tokio::sync::Notify;

use crate::ai::AiState;
use crate::background::{self, POPUP_LABEL};
use crate::error::{CmdResult, CommandError};
use crate::recording::RecorderState;
use crate::state::AppState;

pub const EVENT_UPDATE_STATUS: &str = "update-status";

/// 起動してから最初に確認するまでの時間（起動直後の処理と重ならないよう少し待つ）
const FIRST_CHECK_DELAY: Duration = Duration::from_secs(20);
/// 確認の間隔
const CHECK_INTERVAL_MS: i64 = 6 * 60 * 60 * 1000;
/// 確認に失敗したとき（起動時にまだネットワークにつながっていない等）に、次に試すまでの間隔
const RETRY_INTERVAL_MS: i64 = 30 * 60 * 1000;
/// 確認する時刻になったか、自動でインストールしてよい状態になったかを調べる間隔
const POLL_INTERVAL: Duration = Duration::from_secs(60);
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// 全体の大きさが分からないダウンロードで、進み具合を知らせる間隔
const PROGRESS_STEP_BYTES: u64 = 512 * 1024;

/// 利用者の操作による更新のために再起動した印（settings テーブルのキー）と、その有効時間
const RESTART_MARKER_KEY: &str = "update.restart";
const RESTART_MARKER_TTL_MS: i64 = 10 * 60 * 1000;
/// 自動の更新のために再起動した印（手動の印とは別のキーにして、どちらか一方だけを残す）
const AUTO_RESTART_MARKER_KEY: &str = "update.autoRestart";
/// 自動でのインストールに失敗したバージョン（同じバージョンを自動で入れ直さない）
const AUTO_SKIP_KEY: &str = "update.autoSkip";
/// 自動で更新したことを、次にウィンドウを開いたときに知らせるためのバージョン
const NOTICE_KEY: &str = "update.notice";

const MSG_RECORDING: &str = "録音を停止してから更新してください";
const MSG_AI_BUSY: &str = "AI が議事録や回答を作成しています。終わってから更新してください";
const MSG_NO_UPDATE: &str = "新しいバージョンは見つかっていません。先に更新を確認してください";
const MSG_IN_PROGRESS: &str = "更新を実行中です";

/// 見つかった新しい版。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableUpdate {
    pub version: String,
    /// リリースノートの本文（Markdown）
    pub notes: Option<String>,
    /// 公開日時（UNIX エポックからのミリ秒）
    pub published_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdatePhase {
    Idle,
    Downloading,
    /// ダウンロードと署名の確認が済み、インストーラを起動している
    Installing,
}

/// 画面に見せる更新の状態。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    /// 見つかった新しい版（なければ `None`）
    pub available: Option<AvailableUpdate>,
    /// 最後に確認できた時刻
    pub checked_at: Option<i64>,
    pub phase: UpdatePhase,
    pub downloaded_bytes: u64,
    /// ダウンロードする大きさ（サーバーが返さなければ `None`）
    pub total_bytes: Option<u64>,
    /// ダウンロードと署名の確認が済んでいて、メインウィンドウを閉じると自動でインストールする
    pub ready: bool,
}

/// ダウンロードして署名を確かめた更新のファイル。
type Package = Arc<Vec<u8>>;

/// 更新の状態と、見つけた更新（`U`。実際は `tauri_plugin_updater::Update`）を持つ。
/// テストでは `U` を別の型にして、状態の移り変わりだけを確かめる。
pub struct Tracker<U> {
    status: UpdateStatus,
    pending: Option<U>,
    /// `pending` をダウンロードしたもの（自動でインストールするために持っておく）
    package: Option<Package>,
    /// 設定「新しいバージョンを自動でインストールする」
    auto_update: bool,
    /// 自動のダウンロードかインストールに失敗した（次に確認できるまで、自動では試さない）
    auto_failed: bool,
    /// 自動では入れないバージョン（前に自動でインストールして、更新できていなかった）
    auto_skip: Option<String>,
    last_attempt_ms: Option<i64>,
    last_attempt_ok: bool,
    /// 最後に画面へ知らせた進み具合（割合、または PROGRESS_STEP_BYTES 単位）
    last_progress_step: u64,
}

impl<U> Default for Tracker<U> {
    fn default() -> Self {
        Self {
            status: UpdateStatus {
                available: None,
                checked_at: None,
                phase: UpdatePhase::Idle,
                downloaded_bytes: 0,
                total_bytes: None,
                ready: false,
            },
            pending: None,
            package: None,
            auto_update: false,
            auto_failed: false,
            auto_skip: None,
            last_attempt_ms: None,
            last_attempt_ok: false,
            last_progress_step: 0,
        }
    }
}

impl<U: Clone> Tracker<U> {
    /// 自動更新の設定と、自動では入れないバージョンを指定して作る。
    pub fn new(auto_update: bool, auto_skip: Option<String>) -> Self {
        Self {
            auto_update,
            auto_skip,
            ..Self::default()
        }
    }

    pub fn status(&self) -> UpdateStatus {
        self.status.clone()
    }

    fn set_package(&mut self, package: Option<Package>) {
        self.status.ready = package.is_some();
        self.package = package;
    }

    fn reset_progress(&mut self) {
        self.status.downloaded_bytes = 0;
        self.status.total_bytes = None;
        self.last_progress_step = 0;
    }

    /// 自動更新の設定を切り替える。変わったときだけ、新しい状態を返す。
    /// オフにしたら、ダウンロードしてあるファイルを捨てる（待っている自動インストールをやめる）。
    pub fn set_auto_update(&mut self, enabled: bool) -> Option<UpdateStatus> {
        if self.auto_update == enabled {
            return None;
        }
        self.auto_update = enabled;
        self.auto_failed = false;
        if !enabled {
            self.set_package(None);
        }
        Some(self.status())
    }

    fn installing(&self) -> bool {
        self.status.phase != UpdatePhase::Idle
    }

    /// 自動確認をする時刻か。
    pub fn check_due(&self, now_ms: i64) -> bool {
        if self.installing() {
            return false;
        }
        let interval = if self.last_attempt_ok {
            CHECK_INTERVAL_MS
        } else {
            RETRY_INTERVAL_MS
        };
        match self.last_attempt_ms {
            None => true,
            // 時計が戻された場合（now < last）も確認する
            Some(last) => !(0..interval).contains(&(now_ms - last)),
        }
    }

    /// 確認を始める。インストール中なら始めない（`false`）。
    pub fn begin_check(&mut self, now_ms: i64) -> bool {
        if self.installing() {
            return false;
        }
        self.last_attempt_ms = Some(now_ms);
        self.last_attempt_ok = false;
        true
    }

    /// 確認の結果を記録する。インストール中に届いた結果は捨てる（ダウンロード中の更新を差し替えない）。
    pub fn record_check(&mut self, found: Option<(AvailableUpdate, U)>, now_ms: i64) -> UpdateStatus {
        if !self.installing() {
            self.last_attempt_ok = true;
            self.auto_failed = false;
            self.status.checked_at = Some(now_ms);
            // 別のバージョンに変わったら（なくなった場合も）、ダウンロードしてあるファイルは使わない
            let same_version = match (&self.status.available, &found) {
                (Some(known), Some((available, _))) => known.version == available.version,
                _ => false,
            };
            if !same_version {
                self.set_package(None);
            }
            match found {
                Some((available, update)) => {
                    self.status.available = Some(available);
                    self.pending = Some(update);
                }
                None => {
                    self.status.available = None;
                    self.pending = None;
                }
            }
        }
        self.status()
    }

    /// 自動のダウンロードを始める。設定がオンで、まだダウンロードしていない更新があるときだけ、その更新を返す。
    pub fn begin_background_download(&mut self) -> Option<U> {
        if !self.auto_update || self.auto_failed || self.installing() || self.package.is_some() {
            return None;
        }
        let version = &self.status.available.as_ref()?.version;
        if self.auto_skip.as_ref() == Some(version) {
            return None;
        }
        let update = self.pending.clone()?;
        self.status.phase = UpdatePhase::Downloading;
        self.reset_progress();
        Some(update)
    }

    /// 自動のダウンロードが終わった（署名の確認も済んでいる）。ファイルを持っておき、インストールできるときを待つ。
    pub fn finish_background_download(&mut self, package: Package) -> UpdateStatus {
        self.status.phase = UpdatePhase::Idle;
        self.reset_progress();
        // ダウンロード中に設定がオフになっていたら、持たない
        if self.auto_update {
            self.set_package(Some(package));
        }
        self.status()
    }

    /// 自動のダウンロードかインストールに失敗した。次に確認できるまで、自動では試さない（利用者の操作では試せる）。
    pub fn fail_auto(&mut self) -> UpdateStatus {
        self.auto_failed = true;
        self.set_package(None);
        self.abort_install()
    }

    /// 設定「新しいバージョンを自動でインストールする」がオンか。
    pub fn auto_update(&self) -> bool {
        self.auto_update
    }

    /// インストールを待っている、ダウンロード済みの更新があるか（ダウンロードやインストールの最中は `false`）。
    pub fn downloaded(&self) -> bool {
        self.package.is_some() && !self.installing()
    }

    /// 自動のインストールを始める。設定がオンで、ダウンロード済みの更新があるときだけ、その更新とファイルを返す。
    pub fn begin_auto_install(&mut self) -> Option<(U, Package)> {
        if !self.auto_update || !self.downloaded() {
            return None;
        }
        let update = self.pending.clone()?;
        let package = self.package.clone()?;
        self.status.phase = UpdatePhase::Installing;
        Some((update, package))
    }

    /// 利用者の操作でインストールを始める。見つけてある更新と、ダウンロード済みならそのファイルを返す
    /// （なければ、ダウンロード中の状態にする）。
    pub fn begin_install(&mut self) -> Result<(U, Option<Package>), String> {
        if self.installing() {
            return Err(MSG_IN_PROGRESS.to_string());
        }
        let update = self.pending.clone().ok_or_else(|| MSG_NO_UPDATE.to_string())?;
        self.status.phase = match self.package {
            Some(_) => UpdatePhase::Installing,
            None => UpdatePhase::Downloading,
        };
        self.reset_progress();
        Ok((update, self.package.clone()))
    }

    /// ダウンロードしてあるファイルを捨てる（インストールできなかったファイルを使い回さない）。
    pub fn discard_package(&mut self) {
        self.set_package(None);
    }

    /// ダウンロードの進み具合を足す。画面へ知らせるほど進んだとき（1% ごと）だけ状態を返す。
    pub fn add_progress(&mut self, chunk_bytes: u64, total_bytes: Option<u64>) -> Option<UpdateStatus> {
        if self.status.phase != UpdatePhase::Downloading {
            return None;
        }
        self.status.downloaded_bytes += chunk_bytes;
        self.status.total_bytes = total_bytes.filter(|total| *total > 0);
        let step = match self.status.total_bytes {
            Some(total) => self.status.downloaded_bytes.min(total) * 100 / total,
            None => self.status.downloaded_bytes / PROGRESS_STEP_BYTES,
        };
        if step == self.last_progress_step {
            return None;
        }
        self.last_progress_step = step;
        Some(self.status())
    }

    pub fn begin_installing(&mut self) -> UpdateStatus {
        self.status.phase = UpdatePhase::Installing;
        self.status()
    }

    /// インストールをやめる（失敗・中止）。見つけた更新は残すので、もう一度試せる。
    pub fn abort_install(&mut self) -> UpdateStatus {
        self.status.phase = UpdatePhase::Idle;
        self.reset_progress();
        self.status()
    }
}

/// 自動でインストールしてよいかを決める材料。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallConditions {
    /// 設定「新しいバージョンを自動でインストールする」がオン
    pub auto_update: bool,
    /// 更新のダウンロードと署名の確認が済んでいる
    pub downloaded: bool,
    /// 録音中（文字起こしの仕上げ中を含む）
    pub recording: bool,
    /// AI が議事録や回答を作成している
    pub ai_busy: bool,
    /// メインウィンドウを表示している（閉じてトレイにいる間は false）
    pub window_visible: bool,
    /// 会議検出のポップアップを表示している
    pub popup_open: bool,
}

impl InstallConditions {
    /// いまインストールしてよいか。インストールはアプリを終了させるので、使っている最中には始めない。
    pub fn allows_install(&self) -> bool {
        self.auto_update
            && self.downloaded
            && !self.recording
            && !self.ai_busy
            && !self.window_visible
            && !self.popup_open
    }
}

pub struct UpdateState {
    tracker: Mutex<Tracker<Update>>,
    /// 次の定期的な判定を待たずに、自動更新の処理を進めたいときに知らせる
    wake: Notify,
}

impl UpdateState {
    pub fn new(auto_update: bool, auto_skip: Option<String>) -> Self {
        Self {
            tracker: Mutex::new(Tracker::new(auto_update, auto_skip)),
            wake: Notify::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Tracker<Update>> {
        self.tracker.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// 設定の変更を反映する。再起動しなくても効くよう、オフにしたら待っている自動インストールをやめ、
/// オンにしたら見つけてある更新のダウンロードをすぐ始める。
pub fn apply_settings(app: &AppHandle, settings: &Settings) {
    let state = app.state::<UpdateState>();
    let changed = state.lock().set_auto_update(settings.auto_update);
    if let Some(status) = changed {
        emit_status(app, &status);
        state.wake.notify_one();
    }
}

fn emit_status(app: &AppHandle, status: &UpdateStatus) {
    let _ = app.emit(EVENT_UPDATE_STATUS, status.clone());
}

fn describe(update: &Update) -> AvailableUpdate {
    AvailableUpdate {
        version: update.version.clone(),
        notes: update
            .body
            .as_ref()
            .map(|body| body.trim().to_string())
            .filter(|body| !body.is_empty()),
        published_at: update.date.map(|date| date.unix_timestamp() * 1000),
    }
}

fn build_updater(app: &AppHandle) -> tauri_plugin_updater::Result<Updater> {
    let builder = app.updater_builder().timeout(CHECK_TIMEOUT);
    #[cfg(debug_assertions)]
    let builder = dev_overrides(builder)?;
    builder.build()
}

/// 開発用: 環境変数で配布元と公開鍵を差し替える（手元のサーバーと試験用の鍵で、確認からダウンロードまでを確かめるため）。
/// `KIROSCRIBE_UPDATE_ENDPOINT` = latest.json の URL、`KIROSCRIBE_UPDATE_PUBKEY` = 公開鍵。リリースビルドでは読まない。
#[cfg(debug_assertions)]
fn dev_overrides(
    mut builder: tauri_plugin_updater::UpdaterBuilder,
) -> tauri_plugin_updater::Result<tauri_plugin_updater::UpdaterBuilder> {
    if let Ok(endpoint) = std::env::var("KIROSCRIBE_UPDATE_ENDPOINT") {
        builder = builder.endpoints(vec![endpoint.parse()?])?;
    }
    if let Ok(pubkey) = std::env::var("KIROSCRIBE_UPDATE_PUBKEY") {
        builder = builder.pubkey(pubkey);
    }
    Ok(builder)
}

/// 新しい版があるか確認して、状態を更新する。
async fn check(app: &AppHandle) -> tauri_plugin_updater::Result<UpdateStatus> {
    let state = app.state::<UpdateState>();
    {
        let mut tracker = state.lock();
        if !tracker.begin_check(now_ms()) {
            return Ok(tracker.status());
        }
    }
    let found = build_updater(app)?.check().await?;
    let status = state
        .lock()
        .record_check(found.map(|update| (describe(&update), update)), now_ms());
    emit_status(app, &status);
    Ok(status)
}

/// 起動の少し後と、その後は 6 時間ごとに新しい版を確認する。設定がオンなら、見つけた更新の
/// ダウンロードと、邪魔にならないときのインストールもここから行う。
/// 失敗（オフライン・この環境向けの配布がない等）は画面に出さず、次の機会に確かめ直す。
pub fn spawn_auto_check(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_CHECK_DELAY).await;
        loop {
            let due = app.state::<UpdateState>().lock().check_due(now_ms());
            if due {
                if let Err(err) = check(&app).await {
                    eprintln!("更新を確認できませんでした: {err}");
                }
            }
            auto_update_step(&app).await;
            let state = app.state::<UpdateState>();
            // PC のスリープをまたいでも遅れすぎないよう、長い sleep 1 回にせず、短い間隔で確かめ直す
            tokio::select! {
                () = tokio::time::sleep(POLL_INTERVAL) => {}
                () = state.wake.notified() => {}
            }
        }
    });
}

/// 自動でインストールしてよいかを決める材料を集める。
async fn install_conditions(app: &AppHandle) -> InstallConditions {
    let recording = app.state::<RecorderState>().busy_note_id().await.is_some();
    let (auto_update, downloaded) = {
        let state = app.state::<UpdateState>();
        let tracker = state.lock();
        (tracker.auto_update(), tracker.downloaded())
    };
    InstallConditions {
        auto_update,
        downloaded,
        recording,
        ai_busy: app.state::<AiState>().is_busy(),
        window_visible: background::main_window_visible(app),
        popup_open: app.get_webview_window(POPUP_LABEL).is_some(),
    }
}

/// 自動更新を 1 歩進める。まだなら更新をダウンロードして署名を確かめ、インストールしてよい状態なら
/// インストールする。よくなければ何もせず、次に呼ばれたときに判定し直す。
/// 失敗は画面に出さない（次に確認できたときに、ダウンロードからやり直す）。
async fn auto_update_step(app: &AppHandle) {
    let state = app.state::<UpdateState>();
    let to_download = state.lock().begin_background_download();
    if let Some(mut update) = to_download {
        update.timeout = Some(DOWNLOAD_TIMEOUT);
        emit_status(app, &state.lock().status());
        let status = match download(app, &update).await {
            Ok(package) => state.lock().finish_background_download(package),
            Err(err) => {
                eprintln!("更新をダウンロードできませんでした: {err}");
                state.lock().fail_auto()
            }
        };
        emit_status(app, &status);
    }

    // インストーラを起動する直前の状態で判定する（ダウンロード中に録音が始まった・ウィンドウを開いた等）
    if !install_conditions(app).await.allows_install() {
        return;
    }
    let Some((update, package)) = state.lock().begin_auto_install() else {
        return;
    };
    emit_status(app, &state.lock().status());
    let current_version = app.package_info().version.to_string();
    mark_auto_restart(&app.state::<AppState>().db(), &current_version, &update.version, now_ms());
    // `install` は、渡したファイルの署名をもう一度確かめてからインストーラを起動する
    if let Err(err) = update.install(package.as_slice()) {
        clear_restart_markers(&app.state::<AppState>().db());
        eprintln!("更新を自動でインストールできませんでした: {err}");
        let status = state.lock().fail_auto();
        emit_status(app, &status);
        return;
    }
    // Windows ではここに来ない（インストーラの起動後にプロセスが終了する）
    app.restart()
}

/// 手動の確認で見せるメッセージ。
fn check_error_message(err: &tauri_plugin_updater::Error) -> String {
    use tauri_plugin_updater::Error;
    match err {
        Error::Reqwest(_) | Error::Network(_) => {
            "更新を確認できませんでした。インターネット接続を確認して、もう一度お試しください".into()
        }
        Error::ReleaseNotFound => {
            "更新の情報を取得できませんでした。まだリリースが公開されていないか、一時的に取得できない状態です".into()
        }
        Error::TargetNotFound(_)
        | Error::TargetsNotFound(_)
        | Error::UnsupportedOs
        | Error::UnsupportedArch => "このパソコンの環境向けの更新は配布されていません".into(),
        other => format!("更新を確認できませんでした: {other}"),
    }
}

fn install_error_message(err: &tauri_plugin_updater::Error) -> String {
    use tauri_plugin_updater::Error;
    match err {
        Error::Reqwest(_) | Error::Network(_) => {
            "更新をダウンロードできませんでした。インターネット接続を確認して、もう一度お試しください".into()
        }
        Error::Minisign(_)
        | Error::Base64(_)
        | Error::SignatureUtf8(_)
        | Error::SignedVersionMismatch { .. }
        | Error::MissingSignedVersion => {
            "ダウンロードしたファイルの署名を確認できなかったため、更新を中止しました".into()
        }
        other => format!("更新をインストールできませんでした: {other}"),
    }
}

/// 更新による再起動の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restart {
    /// 利用者の操作（［更新して再起動］など）による更新
    Manual,
    /// 自動の更新
    Auto,
}

/// 自動の更新の前に残す印。
#[derive(Debug, Serialize, Deserialize)]
struct AutoRestartMarker {
    /// 更新する前のバージョン
    from: String,
    /// インストールしようとしたバージョン
    to: String,
    at: i64,
}

fn is_fresh(marked_at: i64, now_ms: i64) -> bool {
    (0..=RESTART_MARKER_TTL_MS).contains(&(now_ms - marked_at))
}

/// 利用者の操作による更新のために再起動することを記録する。インストーラは更新前と同じ引数で起動し直すので、
/// Windows の起動時に自動起動していた場合（`--autostart`）は、そのままだとウィンドウが出ず、更新できたか分からない。
pub fn mark_restart(db: &Database, now_ms: i64) {
    clear_restart_markers(db);
    let _ = db.set_value(RESTART_MARKER_KEY, &now_ms.to_string());
}

/// 自動の更新のために再起動することを記録する。インストーラは更新前と同じ引数で起動し直すので、
/// ウィンドウを閉じてトレイにいた場合（`--autostart` なしで起動していた）は、そのままだとウィンドウが出てしまう。
pub fn mark_auto_restart(db: &Database, from: &str, to: &str, now_ms: i64) {
    clear_restart_markers(db);
    let marker = AutoRestartMarker {
        from: from.to_string(),
        to: to.to_string(),
        at: now_ms,
    };
    if let Ok(json) = serde_json::to_string(&marker) {
        let _ = db.set_value(AUTO_RESTART_MARKER_KEY, &json);
    }
}

fn clear_restart_markers(db: &Database) {
    let _ = db.delete_value(RESTART_MARKER_KEY);
    let _ = db.delete_value(AUTO_RESTART_MARKER_KEY);
}

/// 更新の直後の起動かどうかを調べる（印は 1 回で消す。古い印では、更新の直後として扱わない）。
///
/// 自動の更新の印があれば、いまのバージョンと比べて結果を残す。
/// - 更新できていた: 次にウィンドウを開いたときに知らせるよう、バージョンを記録する
/// - 更新できていなかった（インストーラが失敗した等）: そのバージョンを自動で入れるのを繰り返さないよう記録する
pub fn take_restart(db: &Database, current_version: &str, now_ms: i64) -> Option<Restart> {
    let manual = db.get_value(RESTART_MARKER_KEY).ok().flatten();
    let auto = db.get_value(AUTO_RESTART_MARKER_KEY).ok().flatten();
    clear_restart_markers(db);

    if let Some(marker) = auto.and_then(|raw| serde_json::from_str::<AutoRestartMarker>(&raw).ok()) {
        if marker.from == current_version {
            let _ = db.set_value(AUTO_SKIP_KEY, &marker.to);
        } else {
            let _ = db.set_value(NOTICE_KEY, current_version);
            if is_fresh(marker.at, now_ms) {
                return Some(Restart::Auto);
            }
        }
    }
    manual
        .and_then(|raw| raw.parse::<i64>().ok())
        .is_some_and(|marked_at| is_fresh(marked_at, now_ms))
        .then_some(Restart::Manual)
}

/// 起動したときにウィンドウを出すか。
/// - 利用者の操作による更新のあと: 自動起動の引数が付いていても出す（更新できたことが分かるように）
/// - 自動の更新のあと: 引数が付いていなくても出さない（トレイに戻るだけにする）
/// - それ以外: Windows の起動時の自動起動（`--autostart`）なら出さず、トレイに常駐する
pub fn show_window_on_launch(autostart: bool, restart: Option<Restart>) -> bool {
    match restart {
        Some(Restart::Manual) => true,
        Some(Restart::Auto) => false,
        None => !autostart,
    }
}

/// 自動では入れないバージョン（前に自動でインストールして、更新できていなかった）。
pub fn auto_skip_version(db: &Database) -> Option<String> {
    db.get_value(AUTO_SKIP_KEY).ok().flatten()
}

/// 自動で更新したことの知らせ（更新後のバージョン）を取り出す。1 回で消す。
/// その後さらに別のバージョンになっていたら、知らせない。
fn take_notice(db: &Database, current_version: &str) -> Option<String> {
    let version = db.get_value(NOTICE_KEY).ok().flatten()?;
    let _ = db.delete_value(NOTICE_KEY);
    (version == current_version).then_some(version)
}

/// 更新してよい状態か。更新はアプリを終了させるので、録音中と AI の生成中は始めない。
async fn ensure_can_update(app: &AppHandle) -> CmdResult<()> {
    if app.state::<RecorderState>().busy_note_id().await.is_some() {
        return Err(CommandError(MSG_RECORDING.into()));
    }
    if app.state::<AiState>().is_busy() {
        return Err(CommandError(MSG_AI_BUSY.into()));
    }
    Ok(())
}

/// 更新をダウンロードする。進み具合は画面へ知らせる。
/// ダウンロードが終わると、プラグインが署名を確認する（合わなければエラー）。
async fn download(app: &AppHandle, update: &Update) -> tauri_plugin_updater::Result<Package> {
    let state = app.state::<UpdateState>();
    let bytes = update
        .download(
            |chunk_bytes, total_bytes| {
                let progressed = state.lock().add_progress(chunk_bytes as u64, total_bytes);
                if let Some(status) = progressed {
                    emit_status(app, &status);
                }
            },
            || {},
        )
        .await?;
    Ok(Arc::new(bytes))
}

async fn download_and_install(app: &AppHandle, update: &Update, package: Option<Package>) -> CmdResult<()> {
    let state = app.state::<UpdateState>();
    let package = match package {
        Some(package) => package,
        None => download(app, update)
            .await
            .map_err(|err| CommandError(install_error_message(&err)))?,
    };

    // ダウンロード中に会議を検出して録音が始まっていたら、その録音を止めない
    ensure_can_update(app).await?;

    let status = state.lock().begin_installing();
    emit_status(app, &status);
    mark_restart(&app.state::<AppState>().db(), now_ms());
    // `install` は、渡したファイルの署名をもう一度確かめてからインストーラを起動する
    update.install(package.as_slice()).map_err(|err| {
        clear_restart_markers(&app.state::<AppState>().db());
        state.lock().discard_package();
        CommandError(install_error_message(&err))
    })
}

#[tauri::command]
pub fn get_app_version(app: AppHandle) -> String {
    app.package_info().version.to_string()
}

#[tauri::command]
pub fn get_update_status(updates: State<'_, UpdateState>) -> UpdateStatus {
    updates.lock().status()
}

/// 自動で更新したあと、メインウィンドウを表示しているときに 1 回だけ、更新後のバージョンを返す
/// （ウィンドウを出さずに起動し直すので、次にウィンドウを開いたときに知らせる）。
#[tauri::command]
pub fn take_update_notice(app: AppHandle, state: State<'_, AppState>) -> Option<String> {
    if !background::main_window_visible(&app) {
        return None;
    }
    take_notice(&state.db(), &app.package_info().version.to_string())
}

/// 手動の確認。失敗したら理由を返す（自動確認と違い、黙って無視しない）。
#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> CmdResult<UpdateStatus> {
    let status = check(&app)
        .await
        .map_err(|err| CommandError(check_error_message(&err)))?;
    // 設定がオンなら、見つけた更新のダウンロードをすぐ始める
    app.state::<UpdateState>().wake.notify_one();
    Ok(status)
}

/// 見つけてある更新をインストールする（まだならダウンロードしてから）。進み具合は `update-status` イベントで知らせる。
/// Windows ではインストーラを起動した時点でこのアプリが終了し、インストール後に起動し直される。
#[tauri::command]
pub async fn install_update(app: AppHandle) -> CmdResult<()> {
    ensure_can_update(&app).await?;
    let updates = app.state::<UpdateState>();
    let (mut update, package) = updates.lock().begin_install().map_err(CommandError)?;
    update.timeout = Some(DOWNLOAD_TIMEOUT);
    emit_status(&app, &updates.lock().status());

    if let Err(err) = download_and_install(&app, &update, package).await {
        let status = updates.lock().abort_install();
        emit_status(&app, &status);
        return Err(err);
    }
    // Windows ではここに来ない（インストーラの起動後にプロセスが終了する）。
    // それ以外の環境では、入れ替えたあと自分で起動し直す
    app.restart()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn available(version: &str) -> AvailableUpdate {
        AvailableUpdate {
            version: version.into(),
            notes: Some("- 変更点".into()),
            published_at: Some(1_700_000_000_000),
        }
    }

    fn found(version: &str) -> Option<(AvailableUpdate, &'static str)> {
        Some((available(version), "更新の本体"))
    }

    fn package() -> Package {
        Arc::new(vec![1, 2, 3])
    }

    /// 自動更新がオンで、0.3.0 を見つけてダウンロードまで済ませた状態
    fn downloaded() -> Tracker<&'static str> {
        let mut tracker = Tracker::new(true, None);
        tracker.record_check(found("0.3.0"), 1_000);
        tracker.begin_background_download().expect("ダウンロードを始める");
        tracker.finish_background_download(package());
        tracker
    }

    const HOUR: i64 = 60 * 60 * 1000;

    #[test]
    fn check_result_becomes_the_status() {
        let mut tracker = Tracker::<&str>::default();
        assert_eq!(tracker.status().available, None);
        assert_eq!(tracker.status().checked_at, None);

        let status = tracker.record_check(found("0.3.0"), 1_000);
        assert_eq!(status.available, Some(available("0.3.0")));
        assert_eq!(status.checked_at, Some(1_000));
        assert_eq!(status.phase, UpdatePhase::Idle);

        // その後の確認で見つからなければ（リリースが取り下げられた等）消す
        let status = tracker.record_check(None, 2_000);
        assert_eq!(status.available, None);
        assert_eq!(status.checked_at, Some(2_000));
        assert_eq!(tracker.begin_install(), Err(MSG_NO_UPDATE.to_string()));
    }

    #[test]
    fn with_auto_update_off_nothing_is_downloaded_or_installed_automatically() {
        let mut tracker = Tracker::<&str>::default();
        let status = tracker.record_check(found("0.3.0"), 1_000);
        assert!(!status.ready);
        assert_eq!(tracker.begin_background_download(), None);
        assert!(!tracker.auto_update());
        assert!(!tracker.downloaded());
        assert_eq!(tracker.begin_auto_install(), None);
        assert_eq!(tracker.status().phase, UpdatePhase::Idle);
        // 利用者の操作では、これまでどおりダウンロードから始める
        assert_eq!(tracker.begin_install(), Ok(("更新の本体", None)));
        assert_eq!(tracker.status().phase, UpdatePhase::Downloading);
    }

    #[test]
    fn with_auto_update_on_a_found_update_is_downloaded_and_kept_until_it_can_be_installed() {
        let mut tracker = Tracker::<&str>::new(true, None);
        assert_eq!(tracker.begin_background_download(), None, "更新が見つかっていなければ何もしない");

        tracker.record_check(found("0.3.0"), 1_000);
        assert!(tracker.auto_update());
        assert!(!tracker.downloaded(), "ダウンロードが済むまではインストールしない");
        assert_eq!(tracker.begin_auto_install(), None);

        assert_eq!(tracker.begin_background_download(), Some("更新の本体"));
        assert_eq!(tracker.status().phase, UpdatePhase::Downloading);
        assert_eq!(tracker.begin_background_download(), None, "同時に 2 つはダウンロードしない");
        assert_eq!(tracker.begin_install(), Err(MSG_IN_PROGRESS.to_string()));
        assert!(tracker.add_progress(500, Some(1_000)).is_some(), "進み具合は手動のときと同じに知らせる");

        let status = tracker.finish_background_download(package());
        assert_eq!(status.phase, UpdatePhase::Idle);
        assert!(status.ready);
        assert_eq!((status.downloaded_bytes, status.total_bytes), (0, None));
        assert!(tracker.downloaded());
        assert_eq!(tracker.begin_background_download(), None, "ダウンロード済みなら繰り返さない");

        assert_eq!(tracker.begin_auto_install(), Some(("更新の本体", package())));
        assert_eq!(tracker.status().phase, UpdatePhase::Installing);
        assert_eq!(tracker.begin_auto_install(), None);
    }

    #[test]
    fn manual_install_uses_the_downloaded_package() {
        let mut tracker = downloaded();
        assert_eq!(tracker.begin_install(), Ok(("更新の本体", Some(package()))));
        assert_eq!(tracker.status().phase, UpdatePhase::Installing, "ダウンロードは飛ばす");

        // 録音が始まっていた等で中止しても、ファイルは持ったまま次の機会を待つ
        let status = tracker.abort_install();
        assert!(status.ready);
        assert!(tracker.downloaded());

        // インストールできなかったファイルは捨てて、次はダウンロードからやり直す
        tracker.begin_install().unwrap();
        tracker.discard_package();
        assert!(!tracker.abort_install().ready);
        assert_eq!(tracker.begin_install(), Ok(("更新の本体", None)));
    }

    #[test]
    fn turning_auto_update_off_stops_the_pending_install_without_a_restart() {
        let mut tracker = downloaded();
        assert_eq!(tracker.set_auto_update(true), None, "変わらなければ何もしない");

        let status = tracker.set_auto_update(false).expect("オフにした");
        assert!(!status.ready);
        assert_eq!(status.available, Some(available("0.3.0")), "更新があることは知らせ続ける");
        assert!(!tracker.auto_update());
        assert!(!tracker.downloaded());
        assert_eq!(tracker.begin_auto_install(), None);
        assert_eq!(tracker.begin_background_download(), None);

        // オンに戻すと、ダウンロードからやり直す
        assert!(tracker.set_auto_update(true).is_some());
        assert_eq!(tracker.begin_background_download(), Some("更新の本体"));

        // ダウンロード中にオフにしたら、終わってもファイルを持たない
        tracker.set_auto_update(false);
        let status = tracker.finish_background_download(package());
        assert_eq!(status.phase, UpdatePhase::Idle);
        assert!(!status.ready);
        assert_eq!(tracker.begin_auto_install(), None);
    }

    #[test]
    fn a_failed_automatic_attempt_waits_for_the_next_check() {
        let mut tracker = Tracker::<&str>::new(true, None);
        tracker.record_check(found("0.3.0"), 1_000);
        tracker.begin_background_download().unwrap();
        let status = tracker.fail_auto();
        assert_eq!(status.phase, UpdatePhase::Idle);
        assert!(!status.ready);
        assert_eq!(status.available, Some(available("0.3.0")));
        assert_eq!(tracker.begin_background_download(), None, "すぐには繰り返さない");
        // 利用者の操作では試せる
        assert_eq!(tracker.begin_install(), Ok(("更新の本体", None)));
        tracker.abort_install();

        // 次に確認できたら、もう一度ダウンロードする
        tracker.record_check(found("0.3.0"), 2_000);
        assert_eq!(tracker.begin_background_download(), Some("更新の本体"));
        tracker.finish_background_download(package());

        // インストールに失敗したときも同じ（ファイルを捨てて、次の確認を待つ）
        tracker.begin_auto_install().unwrap();
        assert!(!tracker.fail_auto().ready);
        assert_eq!(tracker.begin_background_download(), None);
        assert_eq!(tracker.begin_auto_install(), None);
    }

    #[test]
    fn downloaded_package_is_kept_only_for_the_same_version() {
        let mut tracker = downloaded();
        assert!(tracker.record_check(found("0.3.0"), 2_000).ready, "同じバージョンなら持ったまま");

        let status = tracker.record_check(found("0.3.1"), 3_000);
        assert!(!status.ready, "別のバージョンが出たら、前のファイルは使わない");
        assert_eq!(tracker.begin_auto_install(), None);
        assert_eq!(tracker.begin_background_download(), Some("更新の本体"));
        tracker.finish_background_download(package());

        // リリースが取り下げられたら捨てる
        assert!(!tracker.record_check(None, 4_000).ready);
        assert_eq!(tracker.begin_auto_install(), None);
    }

    #[test]
    fn a_version_that_failed_to_install_automatically_is_left_to_the_user() {
        let mut tracker = Tracker::<&str>::new(true, Some("0.3.0".into()));
        tracker.record_check(found("0.3.0"), 1_000);
        assert_eq!(tracker.begin_background_download(), None);
        assert_eq!(tracker.begin_install(), Ok(("更新の本体", None)), "知らせと手動の更新はそのまま");
        tracker.abort_install();

        // その次のバージョンは、また自動で入れる
        tracker.record_check(found("0.3.1"), 2_000);
        assert_eq!(tracker.begin_background_download(), Some("更新の本体"));
    }

    #[test]
    fn installs_automatically_only_when_nothing_is_in_use() {
        let quiet = InstallConditions {
            auto_update: true,
            downloaded: true,
            recording: false,
            ai_busy: false,
            window_visible: false,
            popup_open: false,
        };
        assert!(quiet.allows_install(), "設定がオンで、ダウンロード済みで、トレイに隠れていて、何もしていない");

        assert!(!InstallConditions { auto_update: false, ..quiet }.allows_install(), "設定がオフ");
        assert!(!InstallConditions { downloaded: false, ..quiet }.allows_install(), "ダウンロードがまだ");
        assert!(!InstallConditions { recording: true, ..quiet }.allows_install(), "録音中");
        assert!(!InstallConditions { ai_busy: true, ..quiet }.allows_install(), "AI の生成中");
        assert!(!InstallConditions { window_visible: true, ..quiet }.allows_install(), "ウィンドウを表示中");
        assert!(!InstallConditions { popup_open: true, ..quiet }.allows_install(), "ポップアップを表示中");

        // 組み合わせをすべて確かめる: 1 つでも当てはまれば待つ
        for bits in 0..64_u8 {
            let flag = |bit: u8| bits & (1 << bit) != 0;
            let conditions = InstallConditions {
                auto_update: flag(0),
                downloaded: flag(1),
                recording: flag(2),
                ai_busy: flag(3),
                window_visible: flag(4),
                popup_open: flag(5),
            };
            assert_eq!(conditions.allows_install(), conditions == quiet, "{conditions:?}");
        }
    }

    #[test]
    fn a_download_or_install_in_progress_is_not_ready_to_install() {
        let mut tracker = Tracker::<&str>::new(true, None);
        tracker.record_check(found("0.3.0"), 1_000);
        tracker.begin_background_download().unwrap();
        assert!(!tracker.downloaded(), "ダウンロード中");
        tracker.finish_background_download(package());
        assert!(tracker.downloaded());

        // 手動のインストールと重ねない
        tracker.begin_install().unwrap();
        assert!(!tracker.downloaded());
        assert_eq!(tracker.begin_auto_install(), None);
    }

    #[test]
    fn checks_every_six_hours_and_retries_sooner_after_a_failure() {
        let mut tracker = Tracker::<&str>::default();
        assert!(tracker.check_due(0), "起動後の最初の確認");

        // 失敗した確認（begin_check だけで結果が記録されない）のあとは 30 分後に試す
        assert!(tracker.begin_check(0));
        assert!(!tracker.check_due(29 * 60 * 1000));
        assert!(tracker.check_due(30 * 60 * 1000));

        // 成功したら 6 時間後
        assert!(tracker.begin_check(HOUR));
        tracker.record_check(None, HOUR);
        assert!(!tracker.check_due(HOUR + 6 * HOUR - 1));
        assert!(tracker.check_due(HOUR + 6 * HOUR));
        assert!(tracker.check_due(0), "時計が戻されても確認が止まらない");
    }

    #[test]
    fn install_needs_a_found_update_and_runs_one_at_a_time() {
        let mut tracker = Tracker::<&str>::default();
        assert_eq!(tracker.begin_install(), Err(MSG_NO_UPDATE.to_string()));

        tracker.record_check(found("0.3.0"), 1_000);
        assert_eq!(tracker.begin_install(), Ok(("更新の本体", None)));
        assert_eq!(tracker.status().phase, UpdatePhase::Downloading);
        assert_eq!(tracker.begin_install(), Err(MSG_IN_PROGRESS.to_string()));

        // インストール中は確認しない。途中で届いた確認の結果で、ダウンロード中の更新を差し替えない
        assert!(!tracker.check_due(100 * HOUR));
        assert!(!tracker.begin_check(100 * HOUR));
        let status = tracker.record_check(None, 2_000);
        assert_eq!(status.available, Some(available("0.3.0")));
        assert_eq!(status.checked_at, Some(1_000));

        // 失敗して中止したら、同じ更新をもう一度試せる
        let status = tracker.abort_install();
        assert_eq!(status.phase, UpdatePhase::Idle);
        assert_eq!(status.available, Some(available("0.3.0")));
        assert_eq!(tracker.begin_install(), Ok(("更新の本体", None)));
    }

    #[test]
    fn download_progress_is_reported_once_per_percent() {
        let mut tracker = Tracker::<&str>::default();
        tracker.record_check(found("0.3.0"), 1_000);
        assert_eq!(tracker.add_progress(10, Some(1_000)), None, "ダウンロード中でなければ数えない");
        tracker.begin_install().unwrap();

        let total = Some(10_000);
        assert_eq!(tracker.add_progress(50, total), None, "1% に満たない");
        let status = tracker.add_progress(50, total).expect("1% に達した");
        assert_eq!((status.downloaded_bytes, status.total_bytes), (100, total));
        assert_eq!(tracker.add_progress(99, total), None);
        assert!(tracker.add_progress(1, total).is_some());

        let mut reports = 0;
        for _ in 0..98 {
            if tracker.add_progress(100, total).is_some() {
                reports += 1;
            }
        }
        assert_eq!(reports, 98);
        assert_eq!(tracker.status().downloaded_bytes, 10_000);

        let status = tracker.begin_installing();
        assert_eq!(status.phase, UpdatePhase::Installing);
        assert_eq!(tracker.add_progress(100, total), None);

        // 中止すると進み具合は 0 に戻る
        let status = tracker.abort_install();
        assert_eq!((status.downloaded_bytes, status.total_bytes), (0, None));
    }

    #[test]
    fn progress_without_total_size_is_reported_by_bytes() {
        let mut tracker = Tracker::<&str>::default();
        tracker.record_check(found("0.3.0"), 1_000);
        tracker.begin_install().unwrap();
        assert_eq!(tracker.add_progress(PROGRESS_STEP_BYTES - 1, None), None);
        let status = tracker.add_progress(1, Some(0)).expect("512KB ごとに知らせる");
        assert_eq!(status.total_bytes, None, "大きさ 0 は不明として扱う");
    }

    #[test]
    fn status_is_serialized_for_the_frontend() {
        let mut tracker = Tracker::<&str>::default();
        tracker.record_check(found("0.3.0"), 1_000);
        tracker.begin_install().unwrap();
        tracker.add_progress(500, Some(1_000));
        assert_eq!(
            serde_json::to_value(tracker.status()).unwrap(),
            serde_json::json!({
                "available": {"version": "0.3.0", "notes": "- 変更点", "publishedAt": 1_700_000_000_000_i64},
                "checkedAt": 1_000,
                "phase": "downloading",
                "downloadedBytes": 500,
                "totalBytes": 1_000,
                "ready": false,
            })
        );
        assert_eq!(serde_json::to_value(downloaded().status()).unwrap()["ready"], true);
    }

    #[test]
    fn check_errors_are_explained_in_japanese() {
        use tauri_plugin_updater::Error;
        assert!(check_error_message(&Error::ReleaseNotFound).contains("まだリリースが公開されていない"));
        for err in [
            Error::TargetNotFound("linux-x86_64".into()),
            Error::TargetsNotFound(vec!["windows-x86_64-nsis".into(), "windows-x86_64".into()]),
            Error::UnsupportedOs,
        ] {
            assert_eq!(check_error_message(&err), "このパソコンの環境向けの更新は配布されていません");
        }
        assert!(check_error_message(&Error::Network("timeout".into())).contains("インターネット接続"));
        assert!(check_error_message(&Error::EmptyEndpoints).starts_with("更新を確認できませんでした: "));

        assert!(install_error_message(&Error::MissingSignedVersion).contains("署名を確認できなかった"));
        assert!(install_error_message(&Error::Network("503".into())).contains("ダウンロードできませんでした"));
        assert!(install_error_message(&Error::InvalidUpdaterFormat).starts_with("更新をインストールできませんでした: "));
    }

    #[test]
    fn manual_restart_marker_is_used_once_and_expires() {
        let db = Database::open_in_memory().unwrap();
        assert_eq!(take_restart(&db, "0.3.0", 1_000), None, "印がなければ通常の起動");

        mark_restart(&db, 1_000);
        assert_eq!(take_restart(&db, "0.3.0", 31_000), Some(Restart::Manual), "更新の直後の起動");
        assert_eq!(take_restart(&db, "0.3.0", 32_000), None, "2 回目の起動では使わない");

        // インストールが途中で止まった等で残った古い印は無視して消す
        mark_restart(&db, 1_000);
        assert_eq!(take_restart(&db, "0.3.0", 1_000 + RESTART_MARKER_TTL_MS + 1), None);
        assert_eq!(db.get_value(RESTART_MARKER_KEY).unwrap(), None);

        mark_restart(&db, 5_000);
        clear_restart_markers(&db);
        assert_eq!(take_restart(&db, "0.3.0", 6_000), None);

        // 手動の更新では、知らせも「自動では入れない」記録も残さない
        assert_eq!(take_notice(&db, "0.3.0"), None);
        assert_eq!(auto_skip_version(&db), None);
    }

    #[test]
    fn automatic_restart_is_told_apart_from_a_manual_one() {
        let db = Database::open_in_memory().unwrap();
        mark_auto_restart(&db, "0.3.0", "0.4.0", 1_000);
        assert_eq!(take_restart(&db, "0.4.0", 31_000), Some(Restart::Auto));
        assert_eq!(take_restart(&db, "0.4.0", 32_000), None, "2 回目の起動では使わない");
        assert_eq!(auto_skip_version(&db), None);

        // 印は、あとから付けた方だけが残る
        mark_restart(&db, 1_000);
        mark_auto_restart(&db, "0.3.0", "0.4.0", 2_000);
        assert_eq!(db.get_value(RESTART_MARKER_KEY).unwrap(), None);
        assert_eq!(take_restart(&db, "0.4.0", 3_000), Some(Restart::Auto));

        mark_auto_restart(&db, "0.3.0", "0.4.0", 1_000);
        mark_restart(&db, 2_000);
        assert_eq!(db.get_value(AUTO_RESTART_MARKER_KEY).unwrap(), None);
        assert_eq!(take_restart(&db, "0.4.0", 3_000), Some(Restart::Manual));
    }

    #[test]
    fn window_is_shown_on_launch_unless_started_in_the_background() {
        // 通常の起動: 自動起動（--autostart）のときだけ出さない
        assert!(show_window_on_launch(false, None));
        assert!(!show_window_on_launch(true, None));
        // 手動の更新のあと: 自動起動の引数が付いていても出す
        assert!(show_window_on_launch(true, Some(Restart::Manual)));
        assert!(show_window_on_launch(false, Some(Restart::Manual)));
        // 自動の更新のあと: ウィンドウを閉じてトレイにいた場合（--autostart なし）も出さない
        assert!(!show_window_on_launch(false, Some(Restart::Auto)));
        assert!(!show_window_on_launch(true, Some(Restart::Auto)));
    }

    #[test]
    fn automatic_update_leaves_a_notice_that_is_shown_once() {
        let db = Database::open_in_memory().unwrap();
        mark_auto_restart(&db, "0.3.0", "0.4.0", 1_000);
        assert_eq!(take_notice(&db, "0.3.0"), None, "起動し直すまでは知らせない");

        assert_eq!(take_restart(&db, "0.4.0", 31_000), Some(Restart::Auto));
        // ウィンドウを開かないまま起動し直しても残る
        assert_eq!(take_restart(&db, "0.4.0", 90_000_000), None);
        assert_eq!(take_notice(&db, "0.4.0").as_deref(), Some("0.4.0"));
        assert_eq!(take_notice(&db, "0.4.0"), None, "1 回だけ");

        // 起動し直されるのが遅れても（印が古くても）、更新したことは知らせる。ウィンドウは通常どおりに出す
        mark_auto_restart(&db, "0.4.0", "0.5.0", 1_000);
        assert_eq!(take_restart(&db, "0.5.0", 1_000 + RESTART_MARKER_TTL_MS + 1), None);
        // その後さらに別のバージョンになっていたら、古い知らせは出さない
        assert_eq!(take_notice(&db, "0.6.0"), None);
        assert_eq!(take_notice(&db, "0.5.0"), None);
    }

    #[test]
    fn automatic_install_that_did_not_update_is_not_repeated() {
        let db = Database::open_in_memory().unwrap();
        mark_auto_restart(&db, "0.3.0", "0.4.0", 1_000);
        // 起動してみたら、バージョンが変わっていない（インストーラが失敗した等）
        assert_eq!(take_restart(&db, "0.3.0", 31_000), None, "通常の起動として扱う");
        assert_eq!(auto_skip_version(&db).as_deref(), Some("0.4.0"));
        assert_eq!(take_notice(&db, "0.3.0"), None, "更新していないので知らせない");

        // 壊れた印は無視する
        db.set_value(AUTO_RESTART_MARKER_KEY, "壊れた値").unwrap();
        assert_eq!(take_restart(&db, "0.3.0", 1_000), None);
        assert_eq!(db.get_value(AUTO_RESTART_MARKER_KEY).unwrap(), None);
    }
}
