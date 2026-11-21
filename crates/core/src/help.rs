//! アプリ内ヘルプ（使い方・リリースノート）。
//!
//! 内容は GitHub の公開リポジトリから取得する。取得できたら settings テーブルに保存しておき、
//! 取得できないとき（オフラインなど）は 保存済みの内容 → アプリ同梱の内容 の順に使う。

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::error::{CoreError, Result};

/// 使い方（Markdown のファイル）の取得元
pub const RAW_BASE_URL: &str = "https://raw.githubusercontent.com";
/// リリース一覧（GitHub REST API）の取得元
pub const API_BASE_URL: &str = "https://api.github.com";

const REPOSITORY: &str = "s-tabana/kiroscribe";
const BRANCH: &str = "main";
const GUIDE_PATH: &str = "docs/guide.md";
const RELEASES_PER_PAGE: u32 = 20;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// アプリに同梱する版（ビルドした時点の内容）
const BUNDLED_GUIDE: &str = include_str!("../../../docs/guide.md");
const BUNDLED_CHANGELOG: &str = include_str!("../../../CHANGELOG.md");

const GUIDE_CACHE_KEY: &str = "help.guide";
const RELEASES_CACHE_KEY: &str = "help.releases";

/// 外部ブラウザで開く URL の長さの上限
const MAX_URL_LEN: usize = 2048;

/// 表示している内容の出どころ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentSource {
    /// いま GitHub から取得した
    Network,
    /// 取得できなかったので、前回取得して保存した内容
    Cache,
    /// 取得できず保存もないので、アプリに同梱の内容
    Bundled,
}

/// 使い方。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Guide {
    pub markdown: String,
    pub source: ContentSource,
    /// GitHub から取得した時刻（同梱版なら `None`）
    pub fetched_at: Option<i64>,
}

/// 1 つのリリースの説明。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseNote {
    /// タグ名（例: `v0.2.0`）
    pub tag_name: String,
    /// タグ名から先頭の `v` を除いたもの。アプリのバージョンと比べるために使う
    pub version: String,
    /// リリースの名前（なければタグ名）
    pub name: String,
    /// 公開日時。GitHub からは ISO 8601、同梱版では CHANGELOG の日付（`2026-11-21`）
    pub published_at: Option<String>,
    /// 本文（Markdown）
    pub body: String,
    /// GitHub のリリースページ
    pub html_url: Option<String>,
}

/// リリースノート（新しい順）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseNotes {
    pub releases: Vec<ReleaseNote>,
    pub source: ContentSource,
    pub fetched_at: Option<i64>,
}

/// GitHub REST API のリリース（使う項目だけ）。
#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    name: Option<String>,
    published_at: Option<String>,
    body: Option<String>,
    html_url: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

fn version_of(tag_name: &str) -> String {
    let tag = tag_name.trim();
    tag.strip_prefix(['v', 'V']).unwrap_or(tag).to_string()
}

impl From<GithubRelease> for ReleaseNote {
    fn from(release: GithubRelease) -> Self {
        let name = release
            .name
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| release.tag_name.clone());
        Self {
            version: version_of(&release.tag_name),
            tag_name: release.tag_name,
            name,
            published_at: release.published_at,
            body: release.body.unwrap_or_default().trim().to_string(),
            html_url: release.html_url,
        }
    }
}

/// GitHub からヘルプの内容を取得するクライアント。
pub struct HelpClient {
    client: reqwest::Client,
    raw_base_url: String,
    api_base_url: String,
}

impl Default for HelpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HelpClient {
    pub fn new() -> Self {
        Self {
            client: crate::http::client(),
            raw_base_url: RAW_BASE_URL.to_string(),
            api_base_url: API_BASE_URL.to_string(),
        }
    }

    /// 取得元を差し替える（テスト用）。
    pub fn with_base_urls(mut self, raw_base_url: &str, api_base_url: &str) -> Self {
        self.raw_base_url = raw_base_url.trim_end_matches('/').to_string();
        self.api_base_url = api_base_url.trim_end_matches('/').to_string();
        self
    }

    async fn get_text(&self, url: String, accept: &str) -> Result<String> {
        let response = self
            .client
            .get(url)
            .header(reqwest::header::ACCEPT, accept)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(CoreError::Http(format!(
                "GitHub から取得できませんでした（HTTP {}）",
                status.as_u16()
            )));
        }
        Ok(response.text().await?)
    }

    /// 使い方（`docs/guide.md`）を取得する。
    pub async fn fetch_guide(&self) -> Result<String> {
        let url = format!("{}/{REPOSITORY}/{BRANCH}/{GUIDE_PATH}", self.raw_base_url);
        let markdown = self.get_text(url, "text/plain").await?;
        if markdown.trim().is_empty() {
            return Err(CoreError::Other("使い方の内容が空でした".into()));
        }
        Ok(markdown)
    }

    /// 公開済みのリリースを新しい順に取得する（下書きとプレリリースは除く）。
    pub async fn fetch_releases(&self) -> Result<Vec<ReleaseNote>> {
        let url = format!(
            "{}/repos/{REPOSITORY}/releases?per_page={RELEASES_PER_PAGE}",
            self.api_base_url
        );
        let body = self.get_text(url, "application/vnd.github+json").await?;
        let releases: Vec<GithubRelease> = serde_json::from_str(&body)?;
        let notes: Vec<ReleaseNote> = releases
            .into_iter()
            .filter(|r| !r.draft && !r.prerelease)
            .map(ReleaseNote::from)
            .collect();
        if notes.is_empty() {
            return Err(CoreError::Other("公開されているリリースがありません".into()));
        }
        Ok(notes)
    }
}

/// 保存しておく取得結果。アプリを更新した直後は同梱版のほうが新しいことがあるため、
/// 取得したときのアプリのバージョンも持ち、バージョンが変わったら使わない。
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Cached<T> {
    app_version: String,
    fetched_at: i64,
    content: T,
}

fn load_cache<T: serde::de::DeserializeOwned>(
    db: &Database,
    key: &str,
    app_version: &str,
) -> Option<Cached<T>> {
    let raw = db.get_value(key).ok().flatten()?;
    let cached: Cached<T> = serde_json::from_str(&raw).ok()?;
    (cached.app_version == app_version).then_some(cached)
}

/// 保存に失敗しても、取得できた内容はそのまま表示できるので無視する。
fn save_cache<T: Serialize>(db: &Database, key: &str, app_version: &str, fetched_at: i64, content: &T) {
    let cached = Cached {
        app_version: app_version.to_string(),
        fetched_at,
        content,
    };
    if let Ok(json) = serde_json::to_string(&cached) {
        let _ = db.set_value(key, &json);
    }
}

/// 使い方の取得結果を確定する。取得できていれば保存して返し、できていなければ 保存済み → 同梱版 の順に使う。
pub fn resolve_guide(db: &Database, app_version: &str, fetched: Result<String>, now_ms: i64) -> Guide {
    if let Ok(markdown) = fetched {
        save_cache(db, GUIDE_CACHE_KEY, app_version, now_ms, &markdown);
        return Guide {
            markdown,
            source: ContentSource::Network,
            fetched_at: Some(now_ms),
        };
    }
    match load_cache::<String>(db, GUIDE_CACHE_KEY, app_version) {
        Some(cached) => Guide {
            markdown: cached.content,
            source: ContentSource::Cache,
            fetched_at: Some(cached.fetched_at),
        },
        None => Guide {
            markdown: BUNDLED_GUIDE.to_string(),
            source: ContentSource::Bundled,
            fetched_at: None,
        },
    }
}

/// リリースノートの取得結果を確定する（`resolve_guide` と同じ順）。
pub fn resolve_releases(
    db: &Database,
    app_version: &str,
    fetched: Result<Vec<ReleaseNote>>,
    now_ms: i64,
) -> ReleaseNotes {
    if let Ok(releases) = fetched {
        save_cache(db, RELEASES_CACHE_KEY, app_version, now_ms, &releases);
        return ReleaseNotes {
            releases,
            source: ContentSource::Network,
            fetched_at: Some(now_ms),
        };
    }
    match load_cache::<Vec<ReleaseNote>>(db, RELEASES_CACHE_KEY, app_version) {
        Some(cached) => ReleaseNotes {
            releases: cached.content,
            source: ContentSource::Cache,
            fetched_at: Some(cached.fetched_at),
        },
        None => ReleaseNotes {
            releases: parse_changelog(BUNDLED_CHANGELOG),
            source: ContentSource::Bundled,
            fetched_at: None,
        },
    }
}

/// `## 0.2.0 (2026-11-21)` の形の見出し行なら、バージョンと日付を返す。
/// `## v0.2.0（2026-11-21）` や `## [0.2.0] - 2026-11-21` の書き方も受け付ける。
fn changelog_heading(line: &str) -> Option<(String, Option<String>)> {
    let rest = line.strip_prefix("## ")?.trim().trim_start_matches('[');
    let rest = rest.strip_prefix(['v', 'V']).unwrap_or(rest);
    let version_len = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || ".-+".contains(c)))
        .unwrap_or(rest.len());
    let (version, remainder) = rest.split_at(version_len);
    if !version.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let date = remainder
        .trim_matches(|c: char| c.is_whitespace() || "()（）[]-–—".contains(c))
        .to_string();
    Some((version.to_string(), Some(date).filter(|d| !d.is_empty())))
}

/// CHANGELOG.md を `## バージョン (日付)` の節ごとに分け、リリース一覧と同じ形にする。
/// 最初の節より前の説明文と、バージョンでない見出しの節は含めない。
pub fn parse_changelog(markdown: &str) -> Vec<ReleaseNote> {
    let mut releases: Vec<ReleaseNote> = Vec::new();
    // いま読んでいる節（バージョンでない見出しの節なら None）
    let mut current: Option<(ReleaseNote, Vec<&str>)> = None;
    let mut in_code_block = false;

    let mut finish = |section: Option<(ReleaseNote, Vec<&str>)>| {
        if let Some((mut note, lines)) = section {
            note.body = lines.join("\n").trim().to_string();
            releases.push(note);
        }
    };

    for line in markdown.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_block = !in_code_block;
        }
        if !in_code_block && line.starts_with("## ") {
            finish(current.take());
            current = changelog_heading(line).map(|(version, date)| {
                let tag_name = format!("v{version}");
                let note = ReleaseNote {
                    name: tag_name.clone(),
                    tag_name,
                    version,
                    published_at: date,
                    body: String::new(),
                    html_url: None,
                };
                (note, Vec::new())
            });
            continue;
        }
        if let Some((_, lines)) = current.as_mut() {
            lines.push(line);
        }
    }
    finish(current.take());
    releases
}

/// 外部ブラウザで開いてよい URL なら、正規化した URL を返す。
/// `https://` で始まるものだけを許可する（`file:` や独自スキームでローカルのプログラムを起動させない）。
pub fn allowed_external_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.len() > MAX_URL_LEN || url.chars().any(char::is_control) {
        return None;
    }
    if !url.get(..8)?.eq_ignore_ascii_case("https://") {
        return None;
    }
    let parsed = reqwest::Url::parse(url).ok()?;
    let has_host = parsed.host_str().is_some_and(|host| !host.is_empty());
    // `https://github.com@example.com/` のように、見た目と行き先が違う URL は開かない
    let has_credentials = !parsed.username().is_empty() || parsed.password().is_some();
    (parsed.scheme() == "https" && has_host && !has_credentials).then(|| parsed.to_string())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wiremock::matchers::{header, header_exists, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const VERSION: &str = "0.2.0";
    const GUIDE_ROUTE: &str = "/s-tabana/kiroscribe/main/docs/guide.md";
    const RELEASES_ROUTE: &str = "/repos/s-tabana/kiroscribe/releases";

    fn client(server: &MockServer) -> HelpClient {
        HelpClient::new().with_base_urls(&server.uri(), &server.uri())
    }

    async fn mount_guide(server: &MockServer, response: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path(GUIDE_ROUTE))
            .respond_with(response)
            .mount(server)
            .await;
    }

    async fn mount_releases(server: &MockServer, response: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path(RELEASES_ROUTE))
            .respond_with(response)
            .mount(server)
            .await;
    }

    fn sample_releases() -> serde_json::Value {
        json!([
            {
                "tag_name": "v0.4.0-beta.1", "name": "0.4.0 beta", "draft": false, "prerelease": true,
                "published_at": "2026-12-20T00:00:00Z", "body": "試験版", "html_url": "https://github.com/s-tabana/kiroscribe/releases/tag/v0.4.0-beta.1"
            },
            {
                "tag_name": "v0.3.1", "name": "下書き", "draft": true, "prerelease": false,
                "published_at": null, "body": "まだ出していない", "html_url": "https://github.com/s-tabana/kiroscribe/releases/tag/untagged"
            },
            {
                "tag_name": "v0.3.0", "name": "Kiroscribe 0.3.0", "draft": false, "prerelease": false,
                "published_at": "2026-12-01T09:30:00Z", "body": "### 新しい機能\n\n- 話者の名前\n",
                "html_url": "https://github.com/s-tabana/kiroscribe/releases/tag/v0.3.0",
                "assets": [{"name": "latest.json"}]
            },
            {
                "tag_name": "v0.2.0", "name": null, "draft": false, "prerelease": false,
                "published_at": "2026-11-21T03:00:00Z", "body": null,
                "html_url": "https://github.com/s-tabana/kiroscribe/releases/tag/v0.2.0"
            }
        ])
    }

    // ---- 使い方 ----

    #[tokio::test]
    async fn guide_is_fetched_and_cached() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(GUIDE_ROUTE))
            .and(header_exists("user-agent"))
            .respond_with(ResponseTemplate::new(200).set_body_string("# 使い方\n\n最新版"))
            .expect(1)
            .mount(&server)
            .await;
        let db = Database::open_in_memory().unwrap();

        let fetched = client(&server).fetch_guide().await;
        let guide = resolve_guide(&db, VERSION, fetched, 1_000);
        assert_eq!(guide.markdown, "# 使い方\n\n最新版");
        assert_eq!(guide.source, ContentSource::Network);
        assert_eq!(guide.fetched_at, Some(1_000));
        assert!(db.get_value(GUIDE_CACHE_KEY).unwrap().unwrap().contains("最新版"));
    }

    #[tokio::test]
    async fn guide_falls_back_to_cache_when_fetch_fails() {
        let server = MockServer::start().await;
        let db = Database::open_in_memory().unwrap();
        mount_guide(&server, ResponseTemplate::new(200).set_body_string("前回の内容")).await;
        resolve_guide(&db, VERSION, client(&server).fetch_guide().await, 1_000);

        // サーバーがエラーを返すようになった
        server.reset().await;
        mount_guide(&server, ResponseTemplate::new(500)).await;
        let fetched = client(&server).fetch_guide().await;
        assert!(fetched.is_err());
        let guide = resolve_guide(&db, VERSION, fetched, 9_000);
        assert_eq!(guide.markdown, "前回の内容");
        assert_eq!(guide.source, ContentSource::Cache);
        assert_eq!(guide.fetched_at, Some(1_000), "前回取得した時刻を返す");
    }

    #[tokio::test]
    async fn guide_falls_back_to_bundled_copy_without_cache() {
        let server = MockServer::start().await;
        mount_guide(&server, ResponseTemplate::new(404).set_body_string("404: Not Found")).await;
        let db = Database::open_in_memory().unwrap();

        let fetched = client(&server).fetch_guide().await;
        let message = fetched.as_ref().unwrap_err().to_string();
        assert!(message.contains("HTTP 404"), "{message}");
        let guide = resolve_guide(&db, VERSION, fetched, 1_000);
        assert_eq!(guide.source, ContentSource::Bundled);
        assert_eq!(guide.fetched_at, None);
        assert_eq!(guide.markdown, BUNDLED_GUIDE);
        assert!(guide.markdown.starts_with("# Kiroscribe の使い方"));
        assert_eq!(db.get_value(GUIDE_CACHE_KEY).unwrap(), None, "失敗は保存しない");
    }

    #[tokio::test]
    async fn guide_fetch_failure_without_server_uses_bundled_copy() {
        // 接続できない（オフライン相当）
        let unreachable = HelpClient::new().with_base_urls("http://127.0.0.1:1", "http://127.0.0.1:1");
        let db = Database::open_in_memory().unwrap();
        let guide = resolve_guide(&db, VERSION, unreachable.fetch_guide().await, 1_000);
        assert_eq!(guide.source, ContentSource::Bundled);
    }

    #[tokio::test]
    async fn empty_guide_is_treated_as_failure() {
        let server = MockServer::start().await;
        mount_guide(&server, ResponseTemplate::new(200).set_body_string("  \n")).await;
        assert!(client(&server).fetch_guide().await.is_err());
    }

    #[test]
    fn cache_from_another_app_version_is_not_used() {
        // 更新した直後は、同梱版のほうが古いキャッシュより新しい
        let db = Database::open_in_memory().unwrap();
        resolve_guide(&db, "0.2.0", Ok("0.2.0 のころの使い方".into()), 1_000);
        resolve_releases(&db, "0.2.0", Ok(parse_changelog("## 0.2.0\n\n- 旧")), 1_000);
        fn offline<T>() -> Result<T> {
            Err(CoreError::Http("オフライン".into()))
        }

        assert_eq!(resolve_guide(&db, "0.2.0", offline(), 2_000).source, ContentSource::Cache);
        assert_eq!(resolve_guide(&db, "0.3.0", offline(), 2_000).source, ContentSource::Bundled);
        assert_eq!(resolve_releases(&db, "0.2.0", offline(), 2_000).source, ContentSource::Cache);
        assert_eq!(resolve_releases(&db, "0.3.0", offline(), 2_000).source, ContentSource::Bundled);
    }

    #[test]
    fn broken_cache_is_ignored() {
        let db = Database::open_in_memory().unwrap();
        db.set_value(GUIDE_CACHE_KEY, "{壊れた JSON").unwrap();
        let guide = resolve_guide(&db, VERSION, Err(CoreError::Http("オフライン".into())), 1_000);
        assert_eq!(guide.source, ContentSource::Bundled);
    }

    // ---- リリースノート ----

    #[tokio::test]
    async fn releases_are_fetched_without_drafts_and_prereleases() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(RELEASES_ROUTE))
            .and(query_param("per_page", "20"))
            .and(header("accept", "application/vnd.github+json"))
            .and(header_exists("user-agent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(sample_releases()))
            .expect(1)
            .mount(&server)
            .await;
        let db = Database::open_in_memory().unwrap();

        let fetched = client(&server).fetch_releases().await;
        let notes = resolve_releases(&db, VERSION, fetched, 5_000);
        assert_eq!(notes.source, ContentSource::Network);
        assert_eq!(notes.fetched_at, Some(5_000));
        assert_eq!(
            notes.releases,
            vec![
                ReleaseNote {
                    tag_name: "v0.3.0".into(),
                    version: "0.3.0".into(),
                    name: "Kiroscribe 0.3.0".into(),
                    published_at: Some("2026-12-01T09:30:00Z".into()),
                    body: "### 新しい機能\n\n- 話者の名前".into(),
                    html_url: Some("https://github.com/s-tabana/kiroscribe/releases/tag/v0.3.0".into()),
                },
                ReleaseNote {
                    tag_name: "v0.2.0".into(),
                    version: "0.2.0".into(),
                    name: "v0.2.0".into(),
                    published_at: Some("2026-11-21T03:00:00Z".into()),
                    body: String::new(),
                    html_url: Some("https://github.com/s-tabana/kiroscribe/releases/tag/v0.2.0".into()),
                },
            ]
        );

        let request = &server.received_requests().await.unwrap()[0];
        let user_agent = request.headers.get("user-agent").unwrap().to_str().unwrap();
        assert!(user_agent.starts_with("Kiroscribe/"), "{user_agent}");
    }

    #[tokio::test]
    async fn invalid_release_json_falls_back_to_cache_then_bundled() {
        let server = MockServer::start().await;
        let db = Database::open_in_memory().unwrap();

        // 保存済みがなければ同梱の CHANGELOG
        mount_releases(&server, ResponseTemplate::new(200).set_body_string("<html>メンテナンス中</html>")).await;
        let fetched = client(&server).fetch_releases().await;
        assert!(matches!(fetched, Err(CoreError::Serde(_))), "JSON として読めない");
        let notes = resolve_releases(&db, VERSION, fetched, 1_000);
        assert_eq!(notes.source, ContentSource::Bundled);
        assert_eq!(notes.fetched_at, None);
        assert_eq!(notes.releases, parse_changelog(BUNDLED_CHANGELOG));

        // 一度取得できたあとは、保存済みを使う
        server.reset().await;
        mount_releases(&server, ResponseTemplate::new(200).set_body_json(sample_releases())).await;
        resolve_releases(&db, VERSION, client(&server).fetch_releases().await, 2_000);
        server.reset().await;
        mount_releases(&server, ResponseTemplate::new(200).set_body_json(json!({"message": "配列ではない"}))).await;
        let notes = resolve_releases(&db, VERSION, client(&server).fetch_releases().await, 3_000);
        assert_eq!(notes.source, ContentSource::Cache);
        assert_eq!(notes.fetched_at, Some(2_000));
        assert_eq!(notes.releases[0].version, "0.3.0");
        assert_eq!(notes.releases.len(), 2);
    }

    #[tokio::test]
    async fn missing_repository_or_rate_limit_falls_back_to_bundled_changelog() {
        let db = Database::open_in_memory().unwrap();
        for status in [404, 403] {
            let server = MockServer::start().await;
            mount_releases(
                &server,
                ResponseTemplate::new(status).set_body_json(json!({"message": "Not Found"})),
            )
            .await;
            let fetched = client(&server).fetch_releases().await;
            assert!(fetched.as_ref().unwrap_err().to_string().contains(&format!("HTTP {status}")));
            let notes = resolve_releases(&db, VERSION, fetched, 1_000);
            assert_eq!(notes.source, ContentSource::Bundled);
            assert!(!notes.releases.is_empty());
        }
    }

    #[tokio::test]
    async fn empty_release_list_falls_back() {
        // リリースがまだ 1 つもない（または下書きだけ）なら、同梱の CHANGELOG を見せる
        let server = MockServer::start().await;
        mount_releases(&server, ResponseTemplate::new(200).set_body_json(json!([]))).await;
        let db = Database::open_in_memory().unwrap();
        let fetched = client(&server).fetch_releases().await;
        assert!(fetched.is_err());
        assert_eq!(resolve_releases(&db, VERSION, fetched, 1_000).source, ContentSource::Bundled);
    }

    // ---- CHANGELOG の分割 ----

    #[test]
    fn changelog_is_split_by_version_sections() {
        let changelog = "\
# 変更履歴

見出しは `## バージョン (日付)` の形で書いてください。

## 0.3.0 (2026-12-01)

### 新しい機能

- 話者の名前を付けられます

```md
## これは見出しではない
```

## v0.2.1（2026-11-25）
- 不具合の修正

## 予定

- まだ決まっていないこと

## [0.2.0] - 2026-11-21

- 会議の自動検出
## 0.1.0
- 最初のリリース
";
        let releases = parse_changelog(changelog);
        let summary: Vec<(&str, Option<&str>)> = releases
            .iter()
            .map(|r| (r.version.as_str(), r.published_at.as_deref()))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("0.3.0", Some("2026-12-01")),
                ("0.2.1", Some("2026-11-25")),
                ("0.2.0", Some("2026-11-21")),
                ("0.1.0", None),
            ]
        );
        assert_eq!(
            releases[0].body,
            "### 新しい機能\n\n- 話者の名前を付けられます\n\n```md\n## これは見出しではない\n```"
        );
        assert_eq!(releases[1].body, "- 不具合の修正", "バージョンでない節（予定）は含めない");
        assert_eq!(releases[2].body, "- 会議の自動検出");
        assert_eq!(releases[3].body, "- 最初のリリース");
        assert_eq!(releases[0].tag_name, "v0.3.0");
        assert_eq!(releases[0].name, "v0.3.0");
        assert_eq!(releases[0].html_url, None);
        assert!(parse_changelog("# 変更履歴\n\nまだありません\n").is_empty());
    }

    #[test]
    fn bundled_changelog_has_a_section_for_this_version() {
        // リリースノートで「現在のバージョン」を示せるよう、リリース前に CHANGELOG へ節を足しておく
        let releases = parse_changelog(BUNDLED_CHANGELOG);
        let current = releases
            .iter()
            .find(|r| r.version == env!("CARGO_PKG_VERSION"))
            .unwrap_or_else(|| panic!("CHANGELOG.md に {} の節がありません", env!("CARGO_PKG_VERSION")));
        assert!(!current.body.is_empty());
        assert_eq!(releases[0].version, current.version, "いちばん上が現在のバージョン");
    }

    // ---- 外部リンク ----

    #[test]
    fn only_https_urls_may_be_opened() {
        assert_eq!(
            allowed_external_url("https://github.com/s-tabana/kiroscribe/releases").as_deref(),
            Some("https://github.com/s-tabana/kiroscribe/releases")
        );
        assert_eq!(
            allowed_external_url("  HTTPS://Example.com/a?b=1#c ").as_deref(),
            Some("https://example.com/a?b=1#c")
        );
        for rejected in [
            "",
            "http://example.com/",
            "file:///C:/Windows/System32/calc.exe",
            "C:\\Windows\\System32\\calc.exe",
            "javascript:alert(1)",
            "ms-settings:privacy",
            "mailto:someone@example.com",
            "//example.com/",
            "/docs/guide.md",
            "https://",
            "https://user:pass@example.com/",
            "https://github.com@example.com/",
            "https://example.com/\n--evil",
            "https://例\u{0}.com/",
        ] {
            assert_eq!(allowed_external_url(rejected), None, "{rejected:?}");
        }
        let too_long = format!("https://example.com/{}", "a".repeat(MAX_URL_LEN));
        assert_eq!(allowed_external_url(&too_long), None);
    }
}
