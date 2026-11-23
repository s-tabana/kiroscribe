//! 外部 API 呼び出しの再試行（指数バックオフ）。

use std::future::Future;
use std::time::Duration;

use crate::error::{CoreError, Result};

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// 最初の試行を含む最大回数
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        // E2E テスト用のビルド（feature `e2e`）でだけ、待ち時間を縮められる
        #[cfg(feature = "e2e")]
        if let Some(shortened) = crate::e2e::retry_policy(&Self::STANDARD) {
            return shortened;
        }
        Self::STANDARD
    }
}

impl RetryPolicy {
    const STANDARD: Self = Self {
        max_attempts: 4,
        base_delay: Duration::from_secs(1),
        max_delay: Duration::from_secs(8),
    };

    /// n 回目（0 始まり）の失敗後に待つ時間: 1s, 2s, 4s, 8s, 8s …
    pub fn delay_after(&self, failure_index: u32) -> Duration {
        let factor = 2u32.saturating_pow(failure_index);
        self.base_delay
            .saturating_mul(factor)
            .min(self.max_delay)
    }
}

/// 再試行すれば直る見込みのあるエラーか。通信エラー・レート制限・サーバーエラーが対象。
/// キーの誤り（401）やリクエスト不正（400）は何度送っても同じなので再試行しない。
pub fn is_retryable(err: &CoreError) -> bool {
    match err {
        CoreError::Http(_) => true,
        CoreError::Api { status, .. } => *status == 408 || *status == 429 || *status >= 500,
        _ => false,
    }
}

/// `op` を最大 `policy.max_attempts` 回まで試す。再試行の前に `on_retry(試行回数, エラー)` を呼ぶ。
pub async fn retry_async<T, F, Fut>(
    policy: &RetryPolicy,
    mut op: F,
    mut on_retry: impl FnMut(u32, &CoreError),
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut attempt = 0;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(err) if attempt + 1 < policy.max_attempts && is_retryable(&err) => {
                on_retry(attempt + 1, &err);
                tokio::time::sleep(policy.delay_after(attempt)).await;
                attempt += 1;
            }
            Err(err) => return Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    fn fast() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(2),
        }
    }

    fn api(status: u16) -> CoreError {
        CoreError::Api {
            service: "test",
            status,
            message: String::new(),
        }
    }

    // [ERR-006]
    #[test]
    fn delays_grow_exponentially_up_to_max() {
        let p = RetryPolicy::default();
        let secs: Vec<u64> = (0..5).map(|i| p.delay_after(i).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 8]);
    }

    // [ERR-006][MIN-016]
    #[test]
    fn retryable_classification() {
        assert!(is_retryable(&CoreError::Http("timeout".into())));
        assert!(is_retryable(&api(429)));
        assert!(is_retryable(&api(503)));
        assert!(!is_retryable(&api(401)));
        assert!(!is_retryable(&api(400)));
        assert!(!is_retryable(&CoreError::MissingApiKey("OpenAI")));
    }

    // [ERR-006]
    #[tokio::test]
    async fn succeeds_after_transient_failures() {
        let calls = AtomicU32::new(0);
        let mut retries = Vec::new();
        let result = retry_async(
            &fast(),
            || async {
                let n = calls.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    Err(api(500))
                } else {
                    Ok("ok")
                }
            },
            |attempt, _| retries.push(attempt),
        )
        .await;
        assert_eq!(result.unwrap(), "ok");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(retries, [1, 2]);
    }

    // [ERR-006]
    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        let calls = AtomicU32::new(0);
        let result: Result<()> = retry_async(
            &fast(),
            || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(api(503))
            },
            |_, _| {},
        )
        .await;
        assert!(matches!(result, Err(CoreError::Api { status: 503, .. })));
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    // [ERR-006]
    #[tokio::test]
    async fn does_not_retry_permanent_errors() {
        let calls = AtomicU32::new(0);
        let result: Result<()> = retry_async(
            &fast(),
            || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(api(401))
            },
            |_, _| {},
        )
        .await;
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
