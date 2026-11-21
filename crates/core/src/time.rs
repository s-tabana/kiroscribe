use std::time::{SystemTime, UNIX_EPOCH};

/// 現在時刻（UNIX エポックからのミリ秒）。
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
