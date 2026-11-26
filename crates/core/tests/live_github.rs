//! 本物の GitHub に問い合わせて、ヘルプの取得元（使い方・リリースノート）が、いまも製品のアプリから読めることを確かめる。
//!
//! 取得元の URL・リポジトリの公開設定・ファイルの置き場所・API の応答の形は、アプリの外で変わりうる。
//! 模擬サーバーのテスト（network.rs ほか）では気づけないので、インターネットにつながる CI で、製品と同じ
//! クライアントのまま問い合わせる。環境変数 `KIROSCRIBE_LIVE_TESTS=1` のときだけ問い合わせ、それ以外では何もせずに通る
//! （オフラインの開発環境で `cargo test` が失敗しないように）。
//!
//! リリース一覧は、製品と同じく認証なしで GitHub の API に問い合わせる。認証なしの問い合わせの回数には、
//! IP アドレスごとの上限（1 時間に 60 回）があり、CI の共有のランナーではほかの利用者と分け合う。
//! 上限に達して断られたときは、確かめずに警告を出して終える（取得元が変わったこととは区別する）。

use kiroscribe_core::help::{allowed_external_url, HelpClient, ReleaseNote, API_BASE_URL, RAW_BASE_URL};

const RELEASE_PAGE: &str = "https://github.com/s-tabana/kiroscribe/releases/tag/";

fn live() -> bool {
    let enabled = std::env::var("KIROSCRIBE_LIVE_TESTS").as_deref() == Ok("1");
    if !enabled {
        eprintln!("KIROSCRIBE_LIVE_TESTS=1 のときだけ、本物の GitHub に問い合わせます");
    }
    enabled
}

/// 認証なしの GitHub の API の利用回数が、いま上限に達しているか（`/rate_limit` への問い合わせは回数に数えられない）。
/// 確かめられなければ、達していないとみなす（断られた理由を取り違えて、本当の失敗を見逃さないように）
async fn api_rate_limited() -> bool {
    let response = kiroscribe_core::http::client()
        .get(format!("{API_BASE_URL}/rate_limit"))
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await;
    let Ok(response) = response else {
        return false;
    };
    let Ok(text) = response.text().await else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|body| body["resources"]["core"]["remaining"].as_u64())
        == Some(0)
}

/// リリース一覧を取得する。API の利用回数の上限で断られたときは、警告を出して `None` を返す
async fn fetch_releases_unless_rate_limited(client: &HelpClient) -> Option<Vec<ReleaseNote>> {
    match client.fetch_releases().await {
        Ok(releases) => Some(releases),
        Err(err) => {
            let message = err.to_string();
            let refused = message.contains("HTTP 403") || message.contains("HTTP 429");
            if refused && api_rate_limited().await {
                let warning = "GitHub の API の利用回数（認証なし）が上限に達しているため、リリースノートの取得元は確かめませんでした";
                if std::env::var_os("GITHUB_ACTIONS").is_some() {
                    println!("::warning title=GitHub の API の利用回数の上限::{warning}");
                }
                eprintln!("{warning}: {message}");
                return None;
            }
            panic!("リリースノートを GitHub から取得できません: {message}");
        }
    }
}

/// 「1.2.3」の形のバージョンを、大きさを比べられる数の組にする
fn version_numbers(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.split('.').map(|part| {
        (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse::<u64>().ok())
            .flatten()
    });
    let numbers = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(numbers)
}

// [HLP-015]
#[test]
fn version_numbers_reads_only_published_version_forms() {
    assert_eq!(version_numbers("0.3.0"), Some((0, 3, 0)));
    assert_eq!(version_numbers("1.10.2"), Some((1, 10, 2)));
    for wrong in ["0.3", "0.3.0.1", "0.3.0-beta.1", "v0.3.0", "0..1", ""] {
        assert_eq!(version_numbers(wrong), None, "{wrong}");
    }
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
    let Some(releases) = fetch_releases_unless_rate_limited(&client).await else {
        return;
    };
    assert!(!releases.is_empty());
    for release in &releases {
        assert!(!release.version.is_empty() && !release.name.is_empty(), "{release:?}");
        // 公開済みのリリースは「v1.2.3」のタグで、プレリリース（ハイフンの付くもの）は入らない
        assert!(version_numbers(&release.version).is_some(), "バージョンの形が違う: {release:?}");
        assert_eq!(release.tag_name, format!("v{}", release.version), "{release:?}");
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
    // 最新のリリースが先頭で、バージョンも大きい順
    let versions: Vec<(u64, u64, u64)> = releases.iter().filter_map(|r| version_numbers(&r.version)).collect();
    let mut descending = versions.clone();
    descending.sort_by(|a, b| b.cmp(a));
    assert_eq!(versions, descending, "バージョンが大きい順に並んでいない");
}
