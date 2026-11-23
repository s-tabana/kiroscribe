//! 本物の GitHub に問い合わせて、ヘルプの取得元（使い方・リリースノート）が、いまも製品のアプリから読めることを確かめる。
//!
//! 取得元の URL・リポジトリの公開設定・ファイルの置き場所・API の応答の形は、アプリの外で変わりうる。
//! 模擬サーバーのテスト（network.rs ほか）では気づけないので、インターネットにつながる CI で、製品と同じ
//! クライアントのまま問い合わせる。環境変数 `KIROSCRIBE_LIVE_TESTS=1` のときだけ問い合わせ、それ以外では何もせずに通る
//! （オフラインの開発環境で `cargo test` が失敗しないように）。

use kiroscribe_core::help::{allowed_external_url, HelpClient, API_BASE_URL, RAW_BASE_URL};

const RELEASE_PAGE: &str = "https://github.com/s-tabana/kiroscribe/releases/tag/";

fn live() -> bool {
    let enabled = std::env::var("KIROSCRIBE_LIVE_TESTS").as_deref() == Ok("1");
    if !enabled {
        eprintln!("KIROSCRIBE_LIVE_TESTS=1 のときだけ、本物の GitHub に問い合わせます");
    }
    enabled
}

// [HLP-015][HLP-003]
#[tokio::test]
async fn the_guide_on_github_is_the_published_guide() {
    if !live() {
        return;
    }
    let client = HelpClient::new().with_base_urls(RAW_BASE_URL, API_BASE_URL);
    let guide = client.fetch_guide().await.expect("使い方を GitHub から取得できません");
    // アプリに同梱した使い方と同じ文書（題名の行が同じ）
    let bundled = include_str!("../../../docs/guide.md");
    assert_eq!(guide.lines().next(), bundled.lines().next(), "取得した使い方の題名が、同梱のものと違う");
    assert!(guide.lines().count() > 10, "使い方が短すぎる:\n{guide}");
}

// [HLP-015][HLP-004][HLP-008]
#[tokio::test]
async fn the_release_notes_on_github_are_readable() {
    if !live() {
        return;
    }
    let client = HelpClient::new().with_base_urls(RAW_BASE_URL, API_BASE_URL);
    let releases = client
        .fetch_releases()
        .await
        .expect("リリースノートを GitHub から取得できません");
    assert!(!releases.is_empty());
    // 公開済みのバージョン（0.3.0）が読める
    assert!(
        releases.iter().any(|r| r.version == "0.3.0"),
        "0.3.0 のリリースがない: {:?}",
        releases.iter().map(|r| &r.version).collect::<Vec<_>>()
    );
    for release in &releases {
        assert!(!release.version.is_empty() && !release.name.is_empty(), "{release:?}");
        assert!(release.published_at.is_some(), "公開日がない: {release:?}");
        // 「GitHub」のリンクは、このリポジトリのリリースのページで、アプリが外部ブラウザで開いてよい URL
        let url = release.html_url.as_deref().expect("リリースのページの URL がない");
        assert!(url.starts_with(RELEASE_PAGE), "{url}");
        assert_eq!(allowed_external_url(url).as_deref(), Some(url));
    }
    // 新しい順
    let dates: Vec<&str> = releases.iter().filter_map(|r| r.published_at.as_deref()).collect();
    let mut sorted = dates.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(dates, sorted, "新しい順に並んでいない");
}
