//! 外部 API 用の HTTP クライアント。

use std::time::Duration;

/// 接続を打ち切るまでの時間
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// 共通設定の reqwest クライアント。プロキシは環境変数と OS の設定に従う。
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(concat!("Kiroscribe/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("HTTP クライアントの初期化に失敗しました")
}
