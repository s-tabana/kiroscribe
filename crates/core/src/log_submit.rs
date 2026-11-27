//! デバッグ用のログの zip を、ログの回収の入口に送る。
//!
//! 入口は、オーナーのアプリで共通の Cloudflare Worker。`POST <入口>/v1/logs/kiroscribe` に zip をそのまま送ると、
//! 非公開の場所に置いて、提出の番号を返す。入口と提出用の値は、設定（[`Settings::log_intake`]）で決める。
//!
//! [`Settings::log_intake`]: crate::settings::Settings::log_intake

use std::sync::Arc;
use std::time::Duration;

use futures_util::stream;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE};
use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::error::{CoreError, Result};
use crate::settings::Settings;

/// 入口に登録してあるアプリの ID
pub const APP_ID: &str = "kiroscribe";

/// 既定の入口（`https://<ホスト>`）。決まるまでは `None` で、設定（`logIntakeUrl`）もなければ送らない
pub const DEFAULT_LOG_INTAKE_URL: Option<&str> = None;

/// 既定の提出用の値（`X-Log-Token`）。配布物から読めるので秘密ではない。
/// 決まるまでは `None` で、設定（`logIntakeToken`）もなければ送らない
pub const DEFAULT_LOG_INTAKE_TOKEN: Option<&str> = None;

/// 送れる zip の大きさの上限（入口の上限と同じ 20 MB）
pub const MAX_LOG_BYTES: usize = 20 * 1024 * 1024;

/// 送り終えて応答が返るまでを待つ時間
pub const SUBMIT_TIMEOUT: Duration = Duration::from_secs(300);

pub const VERSION_HEADER: &str = "X-Log-App-Version";
pub const CLIENT_ID_HEADER: &str = "X-Log-Client-Id";
pub const TOKEN_HEADER: &str = "X-Log-Token";

/// アプリの導入ごとの乱数の ID を置く、settings テーブルのキー
const CLIENT_ID_KEY: &str = "logSubmit.clientId";

/// 進み具合を知らせる単位
const CHUNK_BYTES: usize = 64 * 1024;

/// 送った大きさ（バイト）と全体の大きさ（バイト）を受け取る
pub type Progress = Arc<dyn Fn(u64, u64) + Send + Sync>;

/// 入口の受け口と、送るときに添える値
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intake {
    /// `POST` する URL（`<入口>/v1/logs/kiroscribe`）
    pub url: String,
    /// `X-Log-Token` の値
    pub token: String,
}

impl Intake {
    /// 入口（`https://<ホスト>`）から受け口を作る。入口の末尾の `/` は除く
    pub fn new(base: &str, token: &str) -> Self {
        Self {
            url: format!("{}/v1/logs/{APP_ID}", base.trim().trim_end_matches('/')),
            token: token.trim().to_string(),
        }
    }
}

/// 入口か提出用の値が決まっていないときの文
pub const NOT_CONFIGURED: &str =
    "ログの送り先がまだ設定されていないため、送れません。「デバッグ用のログを保存…」で保存して、別の方法で渡してください";

/// 設定から送り先を決める。入口か提出用の値が決まっていなければ、送らずに [`NOT_CONFIGURED`] を返す
pub fn intake(settings: &Settings) -> Result<Intake> {
    settings.log_intake().ok_or_else(|| CoreError::Other(NOT_CONFIGURED.into()))
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
        .post(&intake.url)
        .timeout(SUBMIT_TIMEOUT)
        .header(CONTENT_TYPE, "application/zip")
        .header(CONTENT_LENGTH, total)
        .header(VERSION_HEADER, app_version)
        .header(CLIENT_ID_HEADER, client_id)
        .header(TOKEN_HEADER, &intake.token)
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

/// 入口が断ったときの文。入口の説明（`message`）があればそれを使い、なければ状態コードから作る
pub fn rejection_message(status: u16, message: &str) -> String {
    let message: String = message.trim().chars().take(200).collect();
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

    // [SET-019]
    #[test]
    fn intake_url_is_built_from_the_base() {
        assert_eq!(
            Intake::new(" https://logs.example.com/ ", " t "),
            Intake { url: "https://logs.example.com/v1/logs/kiroscribe".into(), token: "t".into() }
        );
        assert_eq!(Intake::new("http://127.0.0.1:4010", "t").url, "http://127.0.0.1:4010/v1/logs/kiroscribe");
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

    // [SET-019]
    #[test]
    fn not_configured_intake_is_not_used() {
        let both = Settings {
            log_intake_url: Some("https://logs.example.com".into()),
            log_intake_token: Some("t".into()),
            ..Default::default()
        };
        assert_eq!(intake(&both).unwrap().url, "https://logs.example.com/v1/logs/kiroscribe");
        if DEFAULT_LOG_INTAKE_URL.is_none() {
            let err = intake(&Settings { log_intake_url: None, ..both.clone() }).unwrap_err();
            assert_eq!(err.to_string(), NOT_CONFIGURED);
        }
        if DEFAULT_LOG_INTAKE_TOKEN.is_none() {
            let err = intake(&Settings { log_intake_token: None, ..both }).unwrap_err();
            assert_eq!(err.to_string(), NOT_CONFIGURED);
        }
    }

    // [SET-018]
    #[tokio::test]
    async fn too_large_zip_is_not_sent() {
        let progress: Progress = Arc::new(|_, _| panic!("送っていないのに進み具合を知らせた"));
        let err = submit_logs(
            &crate::http::client(),
            &Intake::new("http://127.0.0.1:9", "t"),
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
