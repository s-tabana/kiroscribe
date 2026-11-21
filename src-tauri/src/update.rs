//! アプリ内更新。新しい版の確認（起動の少し後と、その後は定期的に）、見つけた更新の保持、
//! ダウンロード → 署名の確認 → インストールを行う。
//!
//! 確認とダウンロードは `tauri-plugin-updater` に任せる（配布元と公開鍵は `tauri.conf.json`）。
//! 自動で行うのは確認までで、インストールは必ず利用者の操作（`install_update`）で始める。

use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use kiroscribe_core::time::now_ms;
use kiroscribe_core::Database;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_updater::{Update, Updater, UpdaterExt};

use crate::ai::AiState;
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
/// 確認する時刻になったかを調べる間隔。PC のスリープをまたいでも遅れすぎないよう、長い sleep 1 回にしない
const POLL_INTERVAL: Duration = Duration::from_secs(10 * 60);
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// 全体の大きさが分からないダウンロードで、進み具合を知らせる間隔
const PROGRESS_STEP_BYTES: u64 = 512 * 1024;

/// 更新のために再起動した印（settings テーブルのキー）と、その有効時間
const RESTART_MARKER_KEY: &str = "update.restart";
const RESTART_MARKER_TTL_MS: i64 = 10 * 60 * 1000;

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
}

/// 更新の状態と、見つけた更新（`U`。実際は `tauri_plugin_updater::Update`）を持つ。
/// テストでは `U` を別の型にして、状態の移り変わりだけを確かめる。
pub struct Tracker<U> {
    status: UpdateStatus,
    pending: Option<U>,
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
            },
            pending: None,
            last_attempt_ms: None,
            last_attempt_ok: false,
            last_progress_step: 0,
        }
    }
}

impl<U: Clone> Tracker<U> {
    pub fn status(&self) -> UpdateStatus {
        self.status.clone()
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
            self.status.checked_at = Some(now_ms);
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

    /// インストールを始める（ダウンロード中の状態にして、見つけてある更新を返す）。
    pub fn begin_install(&mut self) -> Result<U, String> {
        if self.installing() {
            return Err(MSG_IN_PROGRESS.to_string());
        }
        let update = self.pending.clone().ok_or_else(|| MSG_NO_UPDATE.to_string())?;
        self.status.phase = UpdatePhase::Downloading;
        self.status.downloaded_bytes = 0;
        self.status.total_bytes = None;
        self.last_progress_step = 0;
        Ok(update)
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
        self.status.downloaded_bytes = 0;
        self.status.total_bytes = None;
        self.status()
    }
}

#[derive(Default)]
pub struct UpdateState {
    tracker: Mutex<Tracker<Update>>,
}

impl UpdateState {
    fn lock(&self) -> MutexGuard<'_, Tracker<Update>> {
        self.tracker.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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

/// 起動の少し後と、その後は 6 時間ごとに新しい版を確認する。
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
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    });
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

/// 更新のために再起動することを記録する。Windows の起動時に自動起動していた場合、インストーラは同じ引数
/// （`--autostart`）で起動し直すため、そのままだとウィンドウが出ず、更新できたか分からない。
pub fn mark_restart(db: &Database, now_ms: i64) {
    let _ = db.set_value(RESTART_MARKER_KEY, &now_ms.to_string());
}

fn clear_restart_marker(db: &Database) {
    let _ = db.delete_value(RESTART_MARKER_KEY);
}

/// 更新の直後の起動かどうか（印は 1 回で消す。古い印は無視する）。
pub fn take_restart_marker(db: &Database, now_ms: i64) -> bool {
    let Ok(Some(raw)) = db.get_value(RESTART_MARKER_KEY) else {
        return false;
    };
    clear_restart_marker(db);
    raw.parse::<i64>()
        .is_ok_and(|marked_at| (0..=RESTART_MARKER_TTL_MS).contains(&(now_ms - marked_at)))
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

async fn download_and_install(app: &AppHandle, update: &Update) -> CmdResult<()> {
    let state = app.state::<UpdateState>();
    // ダウンロードが終わると、プラグインが署名を確認する（合わなければエラー）
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
        .await
        .map_err(|err| CommandError(install_error_message(&err)))?;

    // ダウンロード中に会議を検出して録音が始まっていたら、その録音を止めない
    ensure_can_update(app).await?;

    let status = state.lock().begin_installing();
    emit_status(app, &status);
    mark_restart(&app.state::<AppState>().db(), now_ms());
    update.install(bytes).map_err(|err| {
        clear_restart_marker(&app.state::<AppState>().db());
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

/// 手動の確認。失敗したら理由を返す（自動確認と違い、黙って無視しない）。
#[tauri::command]
pub async fn check_for_update(app: AppHandle) -> CmdResult<UpdateStatus> {
    check(&app)
        .await
        .map_err(|err| CommandError(check_error_message(&err)))
}

/// 見つけてある更新をダウンロードしてインストールする。進み具合は `update-status` イベントで知らせる。
/// Windows ではインストーラを起動した時点でこのアプリが終了し、インストール後に起動し直される。
#[tauri::command]
pub async fn install_update(app: AppHandle) -> CmdResult<()> {
    ensure_can_update(&app).await?;
    let updates = app.state::<UpdateState>();
    let mut update = updates.lock().begin_install().map_err(CommandError)?;
    update.timeout = Some(DOWNLOAD_TIMEOUT);
    emit_status(&app, &updates.lock().status());

    if let Err(err) = download_and_install(&app, &update).await {
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
        assert_eq!(tracker.begin_install(), Ok("更新の本体"));
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
        assert_eq!(tracker.begin_install(), Ok("更新の本体"));
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
            })
        );
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
    fn restart_marker_is_used_once_and_expires() {
        let db = Database::open_in_memory().unwrap();
        assert!(!take_restart_marker(&db, 1_000), "印がなければ通常の起動");

        mark_restart(&db, 1_000);
        assert!(take_restart_marker(&db, 31_000), "更新の直後の起動");
        assert!(!take_restart_marker(&db, 32_000), "2 回目の起動では使わない");

        // インストールが途中で止まった等で残った古い印は無視して消す
        mark_restart(&db, 1_000);
        assert!(!take_restart_marker(&db, 1_000 + RESTART_MARKER_TTL_MS + 1));
        assert_eq!(db.get_value(RESTART_MARKER_KEY).unwrap(), None);

        mark_restart(&db, 5_000);
        clear_restart_marker(&db);
        assert!(!take_restart_marker(&db, 6_000));
    }
}
