//! デバッグ用のログの zip を、ログの回収の入口に送る。
//!
//! 入口は、オーナーのアプリで共通の Cloudflare Worker。`POST <入口>/v1/logs/kiroscribe` に zip をそのまま送ると、
//! 非公開の場所に置いて、提出の番号を返す。
//!
//! 送り先と提出用の値は本体に埋め込んだ値（[`LOG_INTAKE_URL`]・[`LOG_INTAKE_TOKEN`]）だけを使い、
//! 設定や画面（WebView）からは変えられない。送る前に、許可した入口（[`ALLOWED_INTAKE_HOSTS`]）の
//! `https://` であることを確かめ、リダイレクトには従わない（[`client`]）。
//! E2E テスト用のビルド（feature `e2e`）でだけ、作業フォルダで指定した手元の模擬サーバーに差し替えられる。

use std::sync::Arc;
use std::time::Duration;

use futures_util::stream;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE};
use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::error::{CoreError, Result};

/// 入口に登録してあるアプリの ID
pub const APP_ID: &str = "kiroscribe";

/// 入口（`https://<ホスト>`）
pub const LOG_INTAKE_URL: &str = "https://log-intake.seita-tabana.workers.dev";

/// 提出用の値（`X-Log-Token`）。配布物から読めるので秘密ではない
pub const LOG_INTAKE_TOKEN: &str = "l1vY5LKo4aXh8uXei-_AEHwAonWAKr1G6nGq3rqDzvY";

/// 送ってよい入口のホスト
pub const ALLOWED_INTAKE_HOSTS: &[&str] = &["log-intake.seita-tabana.workers.dev"];

/// 送れる zip の大きさの上限（入口の上限と同じ 20 MB）
pub const MAX_LOG_BYTES: usize = 20 * 1024 * 1024;

/// 送り終えて応答が返るまでを待つ時間
pub const SUBMIT_TIMEOUT: Duration = Duration::from_secs(300);

pub const VERSION_HEADER: &str = "X-Log-App-Version";
pub const CLIENT_ID_HEADER: &str = "X-Log-Client-Id";
pub const TOKEN_HEADER: &str = "X-Log-Token";

/// 提出用の値の長さの上限
const MAX_TOKEN_LEN: usize = 256;

/// アプリの導入ごとの乱数の ID を置く、settings テーブルのキー
const CLIENT_ID_KEY: &str = "logSubmit.clientId";

/// 進み具合を知らせる単位
const CHUNK_BYTES: usize = 64 * 1024;

/// 送った大きさ（バイト）と全体の大きさ（バイト）を受け取る
pub type Progress = Arc<dyn Fn(u64, u64) + Send + Sync>;

/// 入口の受け口と、送るときに添える値。検証を通した値からだけ作る（[`Intake::production`]・[`Intake::local`]）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intake {
    url: String,
    token: String,
}

/// 送り先を使えないときの文
pub const NOT_CONFIGURED: &str =
    "ログの送り先がまだ設定されていないため、送れません。「デバッグ用のログを保存…」で保存して、別の方法で渡してください";

impl Intake {
    /// 本番の入口。`https://`、許可したホスト（[`ALLOWED_INTAKE_HOSTS`]）、既定のポートで、
    /// ユーザー名・パスワード・パス・クエリ・フラグメントを含まないものだけを受け付ける
    pub fn production(base: &str, token: &str) -> Result<Self> {
        let url = parse_base(base)?;
        let host = url.host_str().unwrap_or_default();
        if url.scheme() != "https"
            || url.port().is_some()
            || url.path() != "/"
            || !ALLOWED_INTAKE_HOSTS.contains(&host)
        {
            return Err(invalid(base));
        }
        Self::build(&url, token)
    }

    /// 手元の模擬サーバー（`127.0.0.1`・`localhost`・`[::1]`。パスは付けてよい）。E2E テストと結合テストで使う
    pub fn local(base: &str, token: &str) -> Result<Self> {
        let url = parse_base(base)?;
        let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
        if !matches!(url.scheme(), "http" | "https") || !loopback {
            return Err(invalid(base));
        }
        Self::build(&url, token)
    }

    fn build(base: &Url, token: &str) -> Result<Self> {
        let token_ok = (1..=MAX_TOKEN_LEN).contains(&token.len()) && token.chars().all(|c| c.is_ascii_graphic());
        if !token_ok {
            return Err(CoreError::Other(NOT_CONFIGURED.into()));
        }
        let base = base.as_str().trim_end_matches('/');
        Ok(Self { url: format!("{base}/v1/logs/{APP_ID}"), token: token.to_string() })
    }

    /// `POST` する URL（`<入口>/v1/logs/kiroscribe`）
    pub fn url(&self) -> &str {
        &self.url
    }

    /// `X-Log-Token` の値
    pub fn token(&self) -> &str {
        &self.token
    }
}

fn invalid(base: &str) -> CoreError {
    CoreError::Other(format!("{NOT_CONFIGURED}（使えない送り先: {}）", base.chars().take(100).collect::<String>()))
}

/// 入口の URL を読む。空白・制御文字、ユーザー名・パスワード、クエリ、フラグメントを含むものは断る
fn parse_base(base: &str) -> Result<Url> {
    if base.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid(base));
    }
    let url = Url::parse(base).map_err(|_| invalid(base))?;
    let plain = url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some()
        && url.query().is_none()
        && url.fragment().is_none()
        && !base.contains(['?', '#', '@']);
    if !plain {
        return Err(invalid(base));
    }
    Ok(url)
}

/// 送り先。本番は本体に埋め込んだ入口だけを使う。
/// E2E テスト用のビルドでは、指定した手元の模擬サーバーだけに送る。
pub fn intake() -> Result<Intake> {
    #[cfg(feature = "e2e")]
    {
        let (base, token) = crate::e2e::log_intake().ok_or_else(|| CoreError::Other(NOT_CONFIGURED.into()))?;
        Intake::local(&base, &token)
    }
    #[cfg(not(feature = "e2e"))]
    Intake::production(LOG_INTAKE_URL, LOG_INTAKE_TOKEN)
}

/// ログの提出に使う HTTP クライアント。リダイレクトには従わない（許可した入口の外に送らない）
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(crate::http::CONNECT_TIMEOUT)
        .user_agent(concat!("Kiroscribe/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("HTTP クライアントの初期化に失敗しました")
}

/// 入口が受け付けたときの応答
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Submission {
    /// 提出の番号（例: `KS-20261127-7F3A`）
    pub id: String,
    /// 入口が受け取った時刻（ISO 8601）
    pub received_at: String,
}

#[derive(Deserialize)]
struct Rejected {
    #[serde(default)]
    message: String,
}

/// アプリの導入ごとの乱数の ID。最初に使うときに作って保存し、以後は同じ値を返す（利用者名や端末名は入れない）
pub fn client_id(db: &Database) -> Result<String> {
    if let Some(id) = db.get_value(CLIENT_ID_KEY)?.filter(|id| !id.trim().is_empty()) {
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().to_string();
    db.set_value(CLIENT_ID_KEY, &id)?;
    Ok(id)
}

/// zip を送り、入口が返した提出の番号を返す。`progress` には、送った分を本文の区切りごとに知らせる。
pub async fn submit_logs(
    client: &reqwest::Client,
    intake: &Intake,
    app_version: &str,
    client_id: &str,
    zip: Vec<u8>,
    progress: Progress,
) -> Result<Submission> {
    if zip.len() > MAX_LOG_BYTES {
        return Err(CoreError::Other(format!(
            "ログが大きすぎるため送れません（{} MB。上限は 20 MB）。「デバッグ用のログを保存…」で保存して、別の方法で渡してください",
            zip.len().div_ceil(1024 * 1024)
        )));
    }
    let total = zip.len() as u64;
    let zip = Arc::new(zip);
    let chunks = stream::iter((0..zip.len()).step_by(CHUNK_BYTES).map(move |start| {
        let end = (start + CHUNK_BYTES).min(zip.len());
        progress(end as u64, total);
        Ok::<_, std::io::Error>(zip[start..end].to_vec())
    }));

    let response = client
        .post(intake.url())
        .timeout(SUBMIT_TIMEOUT)
        .header(CONTENT_TYPE, "application/zip")
        .header(CONTENT_LENGTH, total)
        .header(VERSION_HEADER, app_version)
        .header(CLIENT_ID_HEADER, client_id)
        .header(TOKEN_HEADER, intake.token())
        .body(reqwest::Body::wrap_stream(chunks))
        .send()
        .await
        .map_err(send_error)?;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if status.is_success() {
        return serde_json::from_str::<Submission>(&body)
            .ok()
            .filter(|submission| is_submission_id(&submission.id))
            .ok_or_else(|| CoreError::Other("ログは送りましたが、送り先の応答を読めませんでした".into()));
    }
    let message = serde_json::from_str::<Rejected>(&body).map(|r| r.message).unwrap_or_default();
    Err(CoreError::Other(rejection_message(status.as_u16(), &message)))
}

/// 提出の番号の形（入口が返すのは `KS-20261127-7F3A` のような英数字とハイフン）
fn is_submission_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn send_error(err: reqwest::Error) -> CoreError {
    let detail = std::error::Error::source(&err)
        .map(|cause| cause.to_string())
        .unwrap_or_else(|| err.to_string());
    let summary = if err.is_timeout() {
        "応答がなく時間切れになりました"
    } else if err.is_connect() {
        "送り先につながりませんでした。インターネットに接続しているか、プロキシの設定を確かめてください"
    } else {
        "通信に失敗しました"
    };
    CoreError::Http(format!("ログを送れませんでした。{summary}（{detail}）"))
}

/// 入口が断ったときの文。状態コードに応じた案内に、入口の説明があれば添える。
pub fn rejection_message(status: u16, message: &str) -> String {
    let message: String = message.trim().chars().take(200).collect();
    if matches!(status, 300..=499 | 501 | 505) && !matches!(status, 408 | 413 | 425 | 429) {
        let reason = if message.is_empty() {
            format!("ログの送り先がエラーを返しました（HTTP {status}）")
        } else {
            format!("ログを送れませんでした: {message}（HTTP {status}）")
        };
        return format!(
            "{reason}。アプリを最新版に更新してください。提出できない場合は「デバッグ用のログを保存…」で保存して、別の方法で渡してください"
        );
    }
    if !message.is_empty() {
        return format!("ログを送れませんでした: {message}（HTTP {status}）");
    }
    match status {
        413 => "ログが大きすぎるため、送り先が受け付けませんでした（上限は 20 MB）。「デバッグ用のログを保存…」で保存して、別の方法で渡してください".into(),
        429 => "ログの提出が続いたため、送り先が受け付けませんでした。しばらく待ってから、もう一度お試しください（HTTP 429）".into(),
        _ => format!("ログの送り先がエラーを返しました（HTTP {status}）。しばらく待ってから、もう一度お試しください"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // [SET-018]
    #[test]
    fn rejection_messages_prefer_the_intake_explanation() {
        assert_eq!(
            rejection_message(429, "提出が多すぎます。1 時間ほど待ってください"),
            "ログを送れませんでした: 提出が多すぎます。1 時間ほど待ってください（HTTP 429）"
        );
        assert!(rejection_message(413, "").contains("上限は 20 MB"));
        assert!(rejection_message(429, " ").contains("しばらく待ってから"));
        assert_eq!(
            rejection_message(503, ""),
            "ログの送り先がエラーを返しました（HTTP 503）。しばらく待ってから、もう一度お試しください"
        );
        for status in [307, 400, 401, 403, 404, 410, 422] {
            let error = rejection_message(status, "受付を終了しました");
            assert!(error.contains("受付を終了しました"), "{error}");
            assert!(error.contains("最新版に更新"), "{error}");
            assert!(error.contains("デバッグ用のログを保存"), "{error}");
        }
        for status in [408, 425, 429, 503] {
            assert!(rejection_message(status, "").contains("しばらく待ってから"));
        }
        // 長い説明は切り詰める
        assert_eq!(rejection_message(400, &"あ".repeat(500)).chars().filter(|&c| c == 'あ').count(), 200);
    }

    // [SET-017]
    #[test]
    fn submission_ids_are_short_and_plain() {
        assert!(is_submission_id("KS-20261127-7F3A"));
        assert!(!is_submission_id(""));
        assert!(!is_submission_id("<script>"));
        assert!(!is_submission_id(&"A".repeat(65)));
    }

    // [SET-019][SEC-015]
    #[test]
    fn production_intake_is_the_embedded_https_host() {
        #[cfg(feature = "e2e")]
        let intake = Intake::production(LOG_INTAKE_URL, LOG_INTAKE_TOKEN).unwrap();
        #[cfg(not(feature = "e2e"))]
        let intake = intake().unwrap();
        assert_eq!(intake.url(), "https://log-intake.seita-tabana.workers.dev/v1/logs/kiroscribe");
        assert_eq!(intake.token(), LOG_INTAKE_TOKEN);
        assert_eq!(
            Intake::production("https://log-intake.seita-tabana.workers.dev/", "t").unwrap().url(),
            "https://log-intake.seita-tabana.workers.dev/v1/logs/kiroscribe"
        );
    }

    // [SEC-015]
    #[test]
    fn production_intake_refuses_other_destinations() {
        let host = "log-intake.seita-tabana.workers.dev";
        for base in [
            format!("http://{host}"),
            format!("https://user:pass@{host}"),
            format!("https://user@{host}"),
            format!("https://{host}/?to=evil"),
            format!("https://{host}/?"),
            format!("https://{host}/#x"),
            format!("https://{host}/other"),
            format!("https://{host}:8443"),
            format!("https://{host}.evil.example"),
            format!("https://evil.example/{host}"),
            format!(" https://{host}"),
            format!("https://{host}\n"),
            "https://127.0.0.1".into(),
            "file:///tmp".into(),
            "".into(),
        ] {
            let err = Intake::production(&base, "t").unwrap_err();
            assert!(err.to_string().starts_with(NOT_CONFIGURED), "{base:?}: {err}");
        }
        for token in ["", " ", "a b", "改行\n", &"t".repeat(MAX_TOKEN_LEN + 1)] {
            assert!(Intake::production(&format!("https://{host}"), token).is_err(), "{token:?}");
        }
    }

    // [SEC-015]
    #[test]
    fn local_intake_is_loopback_only() {
        assert_eq!(
            Intake::local("http://127.0.0.1:4010/", "t").unwrap().url(),
            "http://127.0.0.1:4010/v1/logs/kiroscribe"
        );
        assert!(Intake::local("http://localhost:4010", "t").is_ok());
        assert!(Intake::local("http://[::1]:4010", "t").is_ok());
        assert_eq!(
            Intake::local("http://127.0.0.1:4010/logs/", "t").unwrap().url(),
            "http://127.0.0.1:4010/logs/v1/logs/kiroscribe"
        );
        for base in [
            "https://log-intake.seita-tabana.workers.dev",
            "http://192.168.0.10:4010",
            "http://127.0.0.1.evil.example",
            "http://u:p@127.0.0.1:4010",
            "http://127.0.0.1:4010/logs?x=1",
            "file:///tmp",
        ] {
            assert!(Intake::local(base, "t").is_err(), "{base}");
        }
    }

    // [SET-017]
    #[test]
    fn client_id_is_random_and_kept() {
        let db = Database::open_in_memory().unwrap();
        let first = client_id(&db).unwrap();
        assert!(uuid::Uuid::parse_str(&first).is_ok(), "{first}");
        assert_eq!(client_id(&db).unwrap(), first, "2 回目も同じ値");
        let other = Database::open_in_memory().unwrap();
        assert_ne!(client_id(&other).unwrap(), first, "導入ごとに違う値");
    }

    // [SET-018]
    #[tokio::test]
    async fn too_large_zip_is_not_sent() {
        let progress: Progress = Arc::new(|_, _| panic!("送っていないのに進み具合を知らせた"));
        let err = submit_logs(
            &client(),
            &Intake::local("http://127.0.0.1:9", "t").unwrap(),
            "0.0.0",
            "client",
            vec![0; MAX_LOG_BYTES + 1],
            progress,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("ログが大きすぎるため送れません（21 MB。上限は 20 MB）"), "{err}");
    }
}
