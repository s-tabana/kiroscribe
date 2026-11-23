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

/// API キーの断片の代わりに表示する文。
pub const REDACTED_API_KEY: &str = "（キーは表示しません）";

fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '*' | '.' | '…' | '•')
}

/// 表示用の文から、API キーらしき文字列（`sk-` で始まるもの）を取り除く。
///
/// OpenAI はキーが誤っているとき、`sk-...abcd` のようにキーの先頭と末尾を含む文を返すことがある。
/// 伏せ字（`*`・`.`・`…`）を挟んだものも、ひと続きのキーとして扱う。
pub fn redact_api_keys(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let rest = &text[index..];
        // 単語の途中の「sk-」（task-1 など）はキーとみなさない
        let at_word_start = !text[..index]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric());
        if at_word_start && rest.starts_with("sk-") {
            let end = rest
                .char_indices()
                .find(|(_, c)| !is_key_char(*c))
                .map_or(rest.len(), |(at, _)| at);
            // 文末のピリオドは文の一部として残す
            let key = rest[..end].trim_end_matches('.');
            if key.len() > "sk-".len() {
                out.push_str(REDACTED_API_KEY);
                index += key.len();
                continue;
            }
        }
        let c = rest.chars().next().expect("index は文字の境界");
        out.push(c);
        index += c.len_utf8();
    }
    out
}

fn api_display(service: &str, status: u16, message: &str) -> String {
    // メッセージは API が返した文のことがある。どの経路で作られたエラーでも、キーの断片は表示しない
    let message = &redact_api_keys(message);
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
/// 本文に API キーの断片が含まれていれば取り除く（[`redact_api_keys`]）。
pub fn api_error_message(body: &str) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    if let Some(message) = parsed
        .as_ref()
        .and_then(|v| v.pointer("/error/message"))
        .and_then(|m| m.as_str())
    {
        return redact_api_keys(message);
    }
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return "（本文なし）".to_string();
    }
    // 先に取り除いてから切り詰める（切り詰めた位置でキーが途切れて、見分けられなくなるのを防ぐ）
    redact_api_keys(trimmed).chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // [ERR-004]
    #[test]
    fn extracts_message_from_json_error() {
        let body = r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#;
        assert_eq!(api_error_message(body), "invalid x-api-key");
        assert_eq!(api_error_message("  "), "（本文なし）");
        assert_eq!(api_error_message("Bad Gateway"), "Bad Gateway");
    }

    // [SEC-014]
    #[test]
    fn api_key_fragments_are_removed_from_messages() {
        let cases = [
            // OpenAI がキーの誤りで返す文（先頭と末尾を残した伏せ字）
            (
                "Incorrect API key provided: sk-proj-********************************abcd. You can find your API key at https://platform.openai.com/account/api-keys.",
                "Incorrect API key provided: （キーは表示しません）. You can find your API key at https://platform.openai.com/account/api-keys.",
            ),
            ("Incorrect API key provided: sk-...abcd", "Incorrect API key provided: （キーは表示しません）"),
            ("Incorrect API key provided: sk-abc…wxyz.", "Incorrect API key provided: （キーは表示しません）."),
            // キーそのもの
            ("key=sk-ant-api03-AbC_dEf-123 is invalid", "key=（キーは表示しません） is invalid"),
            ("「sk-test1234」は使えません", "「（キーは表示しません）」は使えません"),
            ("sk-one と sk-two", "（キーは表示しません） と （キーは表示しません）"),
            // キーでないもの
            ("task-1 failed, ask-me later", "task-1 failed, ask-me later"),
            ("prefix sk- only", "prefix sk- only"),
            ("日本語の文はそのまま", "日本語の文はそのまま"),
            ("", ""),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_api_keys(input), expected, "入力: {input}");
        }
    }

    // [SEC-014]
    #[test]
    fn api_errors_never_show_key_fragments() {
        // 応答の JSON から取り出すとき
        let body = r#"{"error":{"message":"Incorrect API key provided: sk-proj-****abcd.","type":"invalid_request_error"}}"#;
        let message = api_error_message(body);
        assert_eq!(message, "Incorrect API key provided: （キーは表示しません）.");
        // JSON でない本文をそのまま使うとき（300 文字で切る前に取り除く）
        let raw = format!("{} sk-live-{}", "x".repeat(290), "A".repeat(40));
        let message = api_error_message(&raw);
        assert!(!message.contains("sk-"), "{message}");
        assert!(message.chars().count() <= 300);

        // どの経路で作られたエラーでも、表示する文には出さない
        for status in [401, 403, 429, 500] {
            let shown = CoreError::Api {
                service: "OpenAI",
                status,
                message: "bad key sk-secret1234".into(),
            }
            .to_string();
            assert!(!shown.contains("sk-secret1234"), "HTTP {status}: {shown}");
            assert!(shown.contains(REDACTED_API_KEY), "HTTP {status}: {shown}");
        }
    }

    // [ERR-004]
    #[test]
    fn error_detail_is_cut_at_300_chars() {
        let long = "あ".repeat(400);
        assert_eq!(api_error_message(&long).chars().count(), 300);
        // JSON でも、error.message がなければ本文の先頭を使う
        assert_eq!(api_error_message(r#"{"detail":"not found"}"#), r#"{"detail":"not found"}"#);
        // error.message は切らずに使う
        let body = serde_json::json!({ "error": { "message": "い".repeat(400) } }).to_string();
        assert_eq!(api_error_message(&body).chars().count(), 400);
    }

    // [REC-002][MIN-003][KEY-007]
    #[test]
    fn key_errors_have_japanese_messages() {
        assert_eq!(
            CoreError::MissingApiKey("OpenAI").to_string(),
            "API キーが設定されていません（OpenAI）。設定画面から入力してください"
        );
        assert_eq!(
            CoreError::MissingApiKey("Anthropic").to_string(),
            "API キーが設定されていません（Anthropic）。設定画面から入力してください"
        );
        assert_eq!(
            CoreError::Secret("アクセスが拒否されました".into()).to_string(),
            "資格情報ストアのエラー: アクセスが拒否されました"
        );
    }

    // [ERR-003]
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
