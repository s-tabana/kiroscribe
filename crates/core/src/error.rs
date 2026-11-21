use thiserror::Error;

/// コア層で発生するエラー。UI へはメッセージ文字列として渡す。
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("データベースエラー: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("データの変換に失敗しました: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("ノートが見つかりません: {0}")]
    NoteNotFound(String),

    #[error("API キーが設定されていません（{0}）。設定画面から入力してください")]
    MissingApiKey(&'static str),

    #[error("資格情報ストアのエラー: {0}")]
    Secret(String),

    #[error("{0}")]
    Http(String),

    #[error("{}", api_display(service, *status, message))]
    Api {
        service: &'static str,
        status: u16,
        message: String,
    },

    #[error("音声デバイスのエラー: {0}")]
    Audio(String),

    #[error("入出力エラー: {0}")]
    Io(#[from] std::io::Error),

    #[error("中止しました")]
    Cancelled,

    #[error("Claude がこの内容への回答を控えました。メモや文字起こしの内容を確認して、もう一度お試しください")]
    Refused,

    #[error("{0}")]
    Other(String),
}

impl From<reqwest::Error> for CoreError {
    fn from(err: reqwest::Error) -> Self {
        let detail = std::error::Error::source(&err)
            .map(|cause| cause.to_string())
            .unwrap_or_else(|| err.to_string());
        let summary = if err.is_timeout() {
            "応答がなくタイムアウトしました"
        } else if err.is_connect() {
            "接続できませんでした。インターネット接続やプロキシの設定を確認してください"
        } else {
            "通信に失敗しました"
        };
        CoreError::Http(format!("{summary}（{detail}）"))
    }
}

pub type Result<T> = std::result::Result<T, CoreError>;

fn api_display(service: &str, status: u16, message: &str) -> String {
    match status {
        401 | 403 => format!(
            "{service} の API キーが正しくないか、権限がありません（HTTP {status}: {message}）。設定画面でキーを確認してください"
        ),
        429 => format!("{service} の利用上限に達しました。しばらく待ってから試してください（HTTP 429: {message}）"),
        _ => format!("{service} がエラーを返しました（HTTP {status}）: {message}"),
    }
}

/// API のエラーレスポンス本文から、利用者に見せるメッセージを取り出す。
/// OpenAI・Anthropic とも `{"error": {"message": "..."}}` の形で返す。
pub fn api_error_message(body: &str) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    if let Some(message) = parsed
        .as_ref()
        .and_then(|v| v.pointer("/error/message"))
        .and_then(|m| m.as_str())
    {
        return message.to_string();
    }
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "（本文なし）".to_string();
    }
    trimmed.chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_message_from_json_error() {
        let body = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
        assert_eq!(api_error_message(body), "invalid x-api-key");
        assert_eq!(api_error_message("  "), "（本文なし）");
        assert_eq!(api_error_message("Bad Gateway"), "Bad Gateway");
    }

    #[test]
    fn api_error_display_is_readable() {
        let err = |status| CoreError::Api {
            service: "OpenAI",
            status,
            message: "msg".into(),
        };
        assert_eq!(
            err(401).to_string(),
            "OpenAI の API キーが正しくないか、権限がありません（HTTP 401: msg）。設定画面でキーを確認してください"
        );
        assert!(err(429).to_string().starts_with("OpenAI の利用上限に達しました"));
        assert_eq!(err(500).to_string(), "OpenAI がエラーを返しました（HTTP 500）: msg");
    }
}
