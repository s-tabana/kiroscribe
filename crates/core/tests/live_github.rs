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

use std::io::Write;
use std::time::Duration;

use kiroscribe_core::help::{allowed_external_url, HelpClient, ReleaseNote, API_BASE_URL, RAW_BASE_URL};

const RELEASE_PAGE: &str = "https://github.com/s-tabana/kiroscribe/releases/tag/";

fn live() -> bool {
    let enabled = std::env::var("KIROSCRIBE_LIVE_TESTS").as_deref() == Ok("1");
    if !enabled {
        eprintln!("KIROSCRIBE_LIVE_TESTS=1 のときだけ、本物の GitHub に問い合わせます");
    }
    enabled
}

/// `/rate_limit` の問い合わせを打ち切るまでの時間
const RATE_LIMIT_TIMEOUT: Duration = Duration::from_secs(10);

/// GitHub Actions の実行記録に、注釈の行（`::warning …`）を出す。
/// libtest は、成功したテストの `println!` の出力を捨てる（`--nocapture` のときだけ見える）ため、
/// 標準出力に直接書き込む。ほかの出力の行の途中に入っても注釈として読まれるよう、改行で始める
fn annotate(line: &str) {
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "\n{line}\n");
    let _ = out.flush();
}

/// 認証なしの GitHub の API の、いまの残りの利用回数（`/rate_limit` への問い合わせは回数に数えられない）
async fn api_rate_limit_remaining(api_base_url: &str, timeout: Duration) -> Result<u64, String> {
    let response = kiroscribe_core::http::client()
        .get(format!("{api_base_url}/rate_limit"))
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .timeout(timeout)
        .send()
        .await
        .map_err(|err| err.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    let text = response.text().await.map_err(|err| err.to_string())?;
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|body| body["resources"]["core"]["remaining"].as_u64())
        .ok_or_else(|| "応答に resources.core.remaining がありません".to_string())
}

/// リリース一覧の取得の結果
#[derive(Debug)]
enum Releases {
    Fetched(Vec<ReleaseNote>),
    /// API の利用回数の上限に達して断られた（取得元が変わったのではない）
    RateLimited(String),
}

/// リリース一覧を取得する。HTTP 403・429 で断られ、利用回数が残っていない（0 回）と確かめられたときだけ
/// `RateLimited` を返す。回数が残っている、確かめられない、ほかの理由で取得できないときは、失敗にする
async fn fetch_releases_unless_rate_limited(
    client: &HelpClient,
    api_base_url: &str,
    timeout: Duration,
) -> Result<Releases, String> {
    let message = match client.fetch_releases().await {
        Ok(releases) => return Ok(Releases::Fetched(releases)),
        Err(err) => err.to_string(),
    };
    if !(message.contains("HTTP 403") || message.contains("HTTP 429")) {
        return Err(format!("リリースノートを GitHub から取得できません: {message}"));
    }
    match api_rate_limit_remaining(api_base_url, timeout).await {
        Ok(0) => Ok(Releases::RateLimited(message)),
        Ok(remaining) => Err(format!(
            "リリースノートを GitHub から取得できません（API の利用回数は残り {remaining} 回）: {message}"
        )),
        Err(reason) => Err(format!(
            "リリースノートを GitHub から取得できず、API の利用回数の上限によるものかも確かめられません（{reason}）: {message}"
        )),
    }
}

const RATE_LIMIT_WARNING: &str =
    "GitHub の API の利用回数（認証なし）が上限に達しているため、リリースノートの取得元は確かめませんでした";

/// 利用回数の上限で確かめられなかったことを、GitHub Actions の警告と標準エラーで知らせる
fn report_rate_limited(message: &str) {
    if std::env::var_os("GITHUB_ACTIONS").is_some() {
        annotate(&format!("::warning title=GitHub の API の利用回数の上限::{RATE_LIMIT_WARNING}"));
    }
    eprintln!("{RATE_LIMIT_WARNING}: {message}");
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
    let releases = match fetch_releases_unless_rate_limited(&client, API_BASE_URL, RATE_LIMIT_TIMEOUT).await {
        Ok(Releases::Fetched(releases)) => releases,
        Ok(Releases::RateLimited(message)) => {
            report_rate_limited(&message);
            return;
        }
        Err(message) => panic!("{message}"),
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

/// 模擬の GitHub（リリース一覧と `/rate_limit`）
async fn mock_github(releases_status: u16, rate_limit: Option<wiremock::ResponseTemplate>) -> wiremock::MockServer {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/s-tabana/kiroscribe/releases"))
        .respond_with(ResponseTemplate::new(releases_status).set_body_json(serde_json::json!({ "message": "refused" })))
        .mount(&server)
        .await;
    let rate_limit_mock = Mock::given(method("GET")).and(path("/rate_limit"));
    match rate_limit {
        Some(response) => rate_limit_mock.respond_with(response).expect(1).mount(&server).await,
        // 403・429 でなければ、利用回数は確かめない
        None => rate_limit_mock.respond_with(ResponseTemplate::new(500)).expect(0).mount(&server).await,
    }
    server
}

fn remaining(count: u64) -> wiremock::ResponseTemplate {
    wiremock::ResponseTemplate::new(200)
        .set_body_json(serde_json::json!({ "resources": { "core": { "limit": 60, "remaining": count } } }))
}

async fn fetch_from(server: &wiremock::MockServer) -> Result<Releases, String> {
    let client = HelpClient::new().with_base_urls(&server.uri(), &server.uri());
    fetch_releases_unless_rate_limited(&client, &server.uri(), Duration::from_millis(500)).await
}

// [HLP-015]
#[tokio::test]
async fn only_a_refusal_with_no_remaining_api_calls_is_treated_as_the_rate_limit() {
    for status in [403, 429] {
        let server = mock_github(status, Some(remaining(0))).await;
        let result = fetch_from(&server).await;
        assert!(matches!(result, Ok(Releases::RateLimited(_))), "HTTP {status}: {result:?}");
    }
}

// [HLP-015]
#[tokio::test]
async fn other_refusals_and_failures_fail_the_check() {
    // 利用回数が残っているのに断られた
    let server = mock_github(403, Some(remaining(12))).await;
    let err = fetch_from(&server).await.unwrap_err();
    assert!(err.contains("残り 12 回"), "{err}");

    // 利用回数を確かめられない（エラー・形の違う応答・応答の遅れ）
    for (response, reason) in [
        (wiremock::ResponseTemplate::new(500), "HTTP 500"),
        (wiremock::ResponseTemplate::new(200).set_body_string("{}"), "resources.core.remaining"),
        (remaining(0).set_delay(Duration::from_secs(5)), ""),
    ] {
        let server = mock_github(429, Some(response)).await;
        let err = fetch_from(&server).await.unwrap_err();
        assert!(err.contains("確かめられません") && err.contains(reason), "{err}");
    }

    // 403・429 でない（利用回数は確かめずに失敗にする）
    for status in [404, 500] {
        let server = mock_github(status, None).await;
        let err = fetch_from(&server).await.unwrap_err();
        assert!(err.contains(&format!("HTTP {status}")), "{err}");
    }
}

const EMIT_WARNING_ENV: &str = "KIROSCRIBE_TEST_EMIT_RATE_LIMIT_WARNING";

/// 下のテストが、別のプロセスで動かす（ふだんは何もしない）
#[test]
fn emits_the_rate_limit_warning_when_asked() {
    if std::env::var_os(EMIT_WARNING_ENV).is_some() {
        report_rate_limited("HTTP 403（テスト用の値）");
    }
}

/// 警告は、`--nocapture` のない通常の `cargo test` でも、成功したテストから実行記録に出る
// [HLP-015]
#[test]
fn the_rate_limit_warning_reaches_the_actions_log_without_nocapture() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["emits_the_rate_limit_warning_when_asked", "--exact", "--test-threads=1"])
        .env(EMIT_WARNING_ENV, "1")
        .env("GITHUB_ACTIONS", "true")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("1 passed"), "{stdout}");
    let expected = format!("::warning title=GitHub の API の利用回数の上限::{RATE_LIMIT_WARNING}");
    assert!(stdout.lines().any(|line| line == expected), "注釈の行がない:\n{stdout}");
}
