//! 本物の API（OpenAI の文字起こし・Anthropic の Messages）が、いまもアプリの送り方を受け付けることを確かめる。
//!
//! 模擬サーバーのテストでは、API の側の変更（モデルの廃止・名前の変更・応答の形の変更）に気づけない。
//! アプリと同じクライアント・同じプロンプトで、設定画面で選べるすべてのモデルに、ごく短い問い合わせを送る
//! （料金がかかる。1 回で数円）。
//!
//! 資格情報は、サービスごとに次のどちらか。どちらもなければ、そのサービスには問い合わせずに通る。
//! - API キー: 環境変数 `KIROSCRIBE_LIVE_OPENAI_KEY`・`KIROSCRIBE_LIVE_ANTHROPIC_KEY`（手元で走らせるとき）
//! - ID の連携（Workload Identity Federation）: GitHub Actions の中で、GitHub が発行するジョブの ID のトークン
//!   （OIDC）を、各社の短い期限のアクセストークンに交換する。保管する鍵がない。
//!   Anthropic: `KIROSCRIBE_LIVE_ANTHROPIC_FEDERATION_RULE_ID`・`KIROSCRIBE_LIVE_ANTHROPIC_ORGANIZATION_ID`
//!   （と、ルールが複数のワークスペースに効くときは `KIROSCRIBE_LIVE_ANTHROPIC_WORKSPACE_ID`）。
//!   OpenAI: `KIROSCRIBE_LIVE_OPENAI_IDENTITY_PROVIDER_ID`・`KIROSCRIBE_LIVE_OPENAI_SERVICE_ACCOUNT_ID`。
//!
//! CI では `.github/workflows/live-api.yml` が、手動または週に 1 回走らせる。設定の手順は docs/development.md。

use std::sync::atomic::AtomicBool;

use kiroscribe_core::audio::wav::encode_wav_mono_i16;
use kiroscribe_core::claude::{ClaudeClient, Effort, MessageRequest, Role, Turn};
use kiroscribe_core::prompts::{
    chat_system_prompt, enhance_user_message, split_title, MeetingContext, ENHANCE_SYSTEM_PROMPT,
};
use kiroscribe_core::retry::RetryPolicy;
use kiroscribe_core::transcribe::{OpenAiTranscriber, Transcribe};
use kiroscribe_core::transcript::{Segment, Speaker};

const SETTINGS_DIALOG: &str = include_str!("../../../src/components/SettingsDialog.tsx");

fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Anthropic に問い合わせる資格情報（API キーか、ID の連携で得たアクセストークン）
enum AnthropicCredential {
    ApiKey(String),
    AccessToken(String),
}

impl AnthropicCredential {
    fn client(&self, model: &str) -> ClaudeClient {
        match self {
            Self::ApiKey(key) => ClaudeClient::new(key, model),
            Self::AccessToken(token) => ClaudeClient::with_access_token(token, model),
        }
    }
}

async fn anthropic_credential() -> Option<AnthropicCredential> {
    if let Some(key) = env("KIROSCRIBE_LIVE_ANTHROPIC_KEY") {
        return Some(AnthropicCredential::ApiKey(key));
    }
    let (Some(rule), Some(organization), Some(github)) = (
        env("KIROSCRIBE_LIVE_ANTHROPIC_FEDERATION_RULE_ID"),
        env("KIROSCRIBE_LIVE_ANTHROPIC_ORGANIZATION_ID"),
        federation::GitHubIdToken::from_env(),
    ) else {
        eprintln!("Anthropic の API キーも ID の連携の設定もないので、本物の Anthropic の API には問い合わせません");
        return None;
    };
    let federation = federation::AnthropicFederation {
        token_url: federation::ANTHROPIC_TOKEN_URL.into(),
        federation_rule_id: rule,
        organization_id: organization,
        workspace_id: env("KIROSCRIBE_LIVE_ANTHROPIC_WORKSPACE_ID"),
    };
    Some(AnthropicCredential::AccessToken(
        federation.access_token(&github).await,
    ))
}

/// OpenAI に問い合わせる資格情報。API キーも、ID の連携で得たアクセストークンも、同じく `Authorization: Bearer` で送る
async fn openai_credential() -> Option<String> {
    if let Some(key) = env("KIROSCRIBE_LIVE_OPENAI_KEY") {
        return Some(key);
    }
    let (Some(provider), Some(service_account), Some(github)) = (
        env("KIROSCRIBE_LIVE_OPENAI_IDENTITY_PROVIDER_ID"),
        env("KIROSCRIBE_LIVE_OPENAI_SERVICE_ACCOUNT_ID"),
        federation::GitHubIdToken::from_env(),
    ) else {
        eprintln!("OpenAI の API キーも ID の連携の設定もないので、本物の OpenAI の API には問い合わせません");
        return None;
    };
    let federation = federation::OpenAiFederation {
        token_url: federation::OPENAI_TOKEN_URL.into(),
        audience: env("KIROSCRIBE_LIVE_OPENAI_AUDIENCE")
            .unwrap_or_else(|| federation::OPENAI_AUDIENCE.into()),
        identity_provider_id: provider,
        service_account_id: service_account,
    };
    Some(federation.access_token(&github).await)
}

/// ID の連携（Workload Identity Federation）: GitHub Actions のジョブの ID のトークン（OIDC の JWT）を、
/// 各社の短い期限のアクセストークンに交換する。
/// 交換の形は、Anthropic はドキュメント（Workload Identity Federation の「Token exchange request」）、
/// OpenAI は公式の SDK（openai-python の auth/_workload.py）と同じ
mod federation {
    use serde_json::{json, Value};

    pub const ANTHROPIC_TOKEN_URL: &str = "https://api.anthropic.com/v1/oauth/token";
    pub const ANTHROPIC_AUDIENCE: &str = "https://api.anthropic.com";
    pub const OPENAI_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
    pub const OPENAI_AUDIENCE: &str = "https://api.openai.com/v1";

    /// GitHub Actions が、`id-token: write` の権限を持つジョブに渡す、ID のトークンの発行先
    pub struct GitHubIdToken {
        pub request_url: String,
        pub request_token: String,
    }

    impl GitHubIdToken {
        pub fn from_env() -> Option<Self> {
            Some(Self {
                request_url: super::env("ACTIONS_ID_TOKEN_REQUEST_URL")?,
                request_token: super::env("ACTIONS_ID_TOKEN_REQUEST_TOKEN")?,
            })
        }

        /// 受け取り手（audience）を指定して、ID のトークンを発行してもらう。
        /// トークンは 1 回の交換にしか使えない（再利用は拒まれる）ので、交換のたびに発行してもらう
        pub async fn issue(&self, audience: &str) -> String {
            // 発行先の URL には、すでに問い合わせの文字列（?api-version=…）が付いているので、後ろに足す
            let mut url = reqwest::Url::parse(&self.request_url)
                .expect("ACTIONS_ID_TOKEN_REQUEST_URL が URL ではありません");
            url.query_pairs_mut().append_pair("audience", audience);
            let response = kiroscribe_core::http::client()
                .get(url)
                .bearer_auth(&self.request_token)
                .send()
                .await
                .unwrap_or_else(|err| {
                    panic!("GitHub に ID のトークンを発行してもらえません: {err}")
                });
            let body = read(response, "GitHub の ID のトークンの発行").await;
            let token = body["value"]
                .as_str()
                .expect("GitHub の応答に value がありません")
                .to_string();
            mask(&token);
            token
        }
    }

    /// GitHub Actions の中では、受け取ったトークンを実行記録で伏せ字にするよう伝える
    /// （テストは `--nocapture` で走らせ、公開のリポジトリの実行記録はだれでも読めるため）
    fn mask(token: &str) {
        if std::env::var_os("GITHUB_ACTIONS").is_some() {
            println!("::add-mask::{token}");
        }
    }

    async fn read(response: reqwest::Response, what: &str) -> Value {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        assert!(
            status.is_success(),
            "{what}が失敗しました（HTTP {status}）: {text}"
        );
        serde_json::from_str(&text)
            .unwrap_or_else(|err| panic!("{what}の応答が JSON として読めません: {err}"))
    }

    async fn exchange(url: &str, body: Value, what: &str) -> String {
        let response = kiroscribe_core::http::client()
            .post(url)
            .json(&body)
            .send()
            .await
            .unwrap_or_else(|err| panic!("{what}に接続できません: {err}"));
        let body = read(response, what).await;
        let token = body["access_token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .unwrap_or_else(|| panic!("{what}の応答に access_token がありません"))
            .to_string();
        mask(&token);
        token
    }

    /// Anthropic の連携の設定（Claude Console の「Workload identity」で作るルール）
    pub struct AnthropicFederation {
        pub token_url: String,
        pub federation_rule_id: String,
        pub organization_id: String,
        /// ルールが 1 つのワークスペースだけに効くなら、なくてよい
        pub workspace_id: Option<String>,
    }

    impl AnthropicFederation {
        pub async fn access_token(&self, github: &GitHubIdToken) -> String {
            let mut body = json!({
                "grant_type": "urn:ietf:params:oauth:grant-type:jwt-bearer",
                "assertion": github.issue(ANTHROPIC_AUDIENCE).await,
                "federation_rule_id": self.federation_rule_id,
                "organization_id": self.organization_id,
            });
            if let Some(workspace) = &self.workspace_id {
                body["workspace_id"] = json!(workspace);
            }
            exchange(&self.token_url, body, "Anthropic のトークンの交換").await
        }
    }

    /// OpenAI の連携の設定（OpenAI の管理画面で作る ID プロバイダとサービス アカウント）
    pub struct OpenAiFederation {
        pub token_url: String,
        pub audience: String,
        pub identity_provider_id: String,
        pub service_account_id: String,
    }

    impl OpenAiFederation {
        pub async fn access_token(&self, github: &GitHubIdToken) -> String {
            let body = json!({
                "grant_type": "urn:ietf:params:oauth:grant-type:token-exchange",
                "subject_token": github.issue(&self.audience).await,
                "subject_token_type": "urn:ietf:params:oauth:token-type:jwt",
                "identity_provider_id": self.identity_provider_id,
                "service_account_id": self.service_account_id,
            });
            exchange(&self.token_url, body, "OpenAI のトークンの交換").await
        }
    }
}

/// 設定画面の選択肢（`const <name>: ModelOption[] = [ { value: "…" }, … ]`）から、モデルの名前を取り出す。
/// 画面と同じ一覧を確かめるため、ここには書き写さない
fn models(list_name: &str) -> Vec<String> {
    let start = SETTINGS_DIALOG
        .find(&format!("const {list_name}"))
        .unwrap_or_else(|| panic!("SettingsDialog.tsx に {list_name} がありません"));
    let rest = &SETTINGS_DIALOG[start..];
    let end = rest.find("];").expect("一覧の終わりがありません");
    let found: Vec<String> = rest[..end]
        .split("value: \"")
        .skip(1)
        .filter_map(|part| part.split('"').next().map(str::to_string))
        .collect();
    assert!(!found.is_empty(), "{list_name} にモデルがありません");
    found
}

fn segments() -> Vec<Segment> {
    let start = 1_795_000_000_000;
    let line = |i: i64, speaker: Speaker, text: &str| Segment {
        id: i,
        note_id: "live".into(),
        speaker,
        started_at: start + i * 10_000,
        ended_at: start + i * 10_000 + 8_000,
        text: text.into(),
    };
    vec![
        line(1, Speaker::Me, "来週のリリースは 12 月 5 日でいきましょう。"),
        line(2, Speaker::Them, "了解です。テストは水曜までに終わらせます。"),
        line(3, Speaker::Me, "では、私がリリースノートを書きます。"),
    ]
}

/// 設定画面で選べるすべての Claude のモデルに、アプリと同じ送り方（ストリーミング）で短い質問を送り、回答が返ること
// [CHT-013][SET-009]
#[tokio::test]
async fn every_selectable_claude_model_answers_a_chat_question() {
    let Some(credential) = anthropic_credential().await else { return };
    let segments = segments();
    let context = MeetingContext {
        title: "リリースの打ち合わせ",
        created_at: 1_795_000_000_000,
        user_notes: "",
        enhanced_notes: None,
        segments: &segments,
        my_name: "",
    };
    for model in models("CLAUDE_MODELS") {
        let request = MessageRequest {
            system: chat_system_prompt(&context),
            messages: vec![Turn {
                role: Role::User,
                content: "リリースの日付だけを答えてください。".into(),
            }],
            max_tokens: 200,
            effort: Effort::Low,
        };
        let mut streamed = String::new();
        let result = credential
            .client(&model)
            .stream(&request, &mut |text: &str| streamed.push_str(text), &AtomicBool::new(false), &RetryPolicy::default())
            .await
            .unwrap_or_else(|err| panic!("{model}: 回答が返りません: {err}"));
        assert!(!result.text.trim().is_empty(), "{model}: 回答が空です");
        assert_eq!(streamed, result.text, "{model}: 少しずつ届いた文字と、最後の回答が違います");
        assert!(result.text.contains('5'), "{model}: 会議の内容に沿っていません: {}", result.text);
        eprintln!("{model}: {}（応答したモデル: {:?}）", result.text.trim(), result.model);
    }
}

/// 既定のモデルで、アプリと同じプロンプトから議事録ができる（題名と本文に分けられる）
// [MIN-020]
#[tokio::test]
async fn the_default_model_writes_minutes_from_the_app_prompt() {
    let Some(credential) = anthropic_credential().await else { return };
    let segments = segments();
    let context = MeetingContext {
        title: "",
        created_at: 1_795_000_000_000,
        user_notes: "リリース日を決める",
        enhanced_notes: None,
        segments: &segments,
        my_name: "",
    };
    let request = MessageRequest {
        system: ENHANCE_SYSTEM_PROMPT.to_string(),
        messages: vec![Turn {
            role: Role::User,
            content: enhance_user_message(&context).unwrap(),
        }],
        max_tokens: 4_000,
        effort: Effort::Medium,
    };
    let model = models("CLAUDE_MODELS").remove(0);
    let result = credential
        .client(&model)
        .stream(&request, &mut |_: &str| {}, &AtomicBool::new(false), &RetryPolicy::default())
        .await
        .unwrap_or_else(|err| panic!("{model}: 議事録が返りません: {err}"));
    let (title, body) = split_title(&result.text);
    assert!(title.is_some_and(|t| !t.trim().is_empty()), "題名がありません:\n{}", result.text);
    assert!(body.contains("12") && body.contains('5'), "議事録が会議の内容に沿っていません:\n{body}");
}

/// 設定画面で選べるすべての文字起こしのモデルが、アプリと同じ送り方（16 ビットの WAV）を受け付ける
// [TRN-022][SET-008]
#[tokio::test]
async fn every_selectable_transcription_model_accepts_the_app_audio() {
    let Some(api_key) = openai_credential().await else { return };
    // 2 秒の、声の高さの音（言葉ではないので、文字起こしの結果は問わない。受け付けられて、応答の形が読めればよい）
    let rate = 16_000;
    let samples: Vec<f32> = (0..rate * 2)
        .map(|i| {
            let t = i as f32 / rate as f32;
            0.3 * (2.0 * std::f32::consts::PI * 170.0 * t).sin() * (0.6 + 0.4 * (2.0 * std::f32::consts::PI * 4.0 * t).sin())
        })
        .collect();
    let wav = encode_wav_mono_i16(&samples, rate as u32);
    for model in models("TRANSCRIPTION_MODELS") {
        let text = OpenAiTranscriber::new(&api_key, &model, "ja")
            .transcribe(wav.clone(), None)
            .await
            .unwrap_or_else(|err| panic!("{model}: 文字起こしの API が受け付けません: {err}"));
        eprintln!("{model}: 「{text}」");
    }
}

/// ID の連携の交換が、各社の決めた形で送られる（本物の API には問い合わせない。模擬サーバーで確かめる）。
/// 設定を間違えたときではなく、ここの送り方を間違えたときに、本物の CI を待たずに気づくため
#[tokio::test]
async fn federation_exchanges_github_id_tokens_in_each_documented_form() {
    use serde_json::json;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    for (audience, jwt) in [
        (federation::ANTHROPIC_AUDIENCE, "jwt-for-anthropic"),
        (federation::OPENAI_AUDIENCE, "jwt-for-openai"),
    ] {
        Mock::given(method("GET"))
            .and(path("/github-id-token"))
            .and(query_param("api-version", "2.0"))
            .and(query_param("audience", audience))
            .and(header("authorization", "Bearer github-request-token"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "count": 1, "value": jwt })),
            )
            .expect(1)
            .mount(&server)
            .await;
    }
    Mock::given(method("POST"))
        .and(path("/anthropic/v1/oauth/token"))
        .and(body_json(json!({
            "grant_type": "urn:ietf:params:oauth:grant-type:jwt-bearer",
            "assertion": "jwt-for-anthropic",
            "federation_rule_id": "fdrl_test",
            "organization_id": "00000000-0000-0000-0000-000000000000",
            "workspace_id": "wrkspc_test",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "sk-ant-oat01-test", "token_type": "Bearer", "expires_in": 600, "scope": "workspace:developer"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/openai/oauth/token"))
        .and(body_json(json!({
            "grant_type": "urn:ietf:params:oauth:grant-type:token-exchange",
            "subject_token": "jwt-for-openai",
            "subject_token_type": "urn:ietf:params:oauth:token-type:jwt",
            "identity_provider_id": "idp_test",
            "service_account_id": "svc_acct_test",
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({ "access_token": "openai-access-token", "expires_in": 3600 }),
            ),
        )
        .expect(1)
        .mount(&server)
        .await;

    // GitHub は、発行先の URL にすでに問い合わせの文字列（?api-version=2.0）を付けて渡す
    let github = federation::GitHubIdToken {
        request_url: format!("{}/github-id-token?api-version=2.0", server.uri()),
        request_token: "github-request-token".into(),
    };
    let anthropic = federation::AnthropicFederation {
        token_url: format!("{}/anthropic/v1/oauth/token", server.uri()),
        federation_rule_id: "fdrl_test".into(),
        organization_id: "00000000-0000-0000-0000-000000000000".into(),
        workspace_id: Some("wrkspc_test".into()),
    };
    assert_eq!(anthropic.access_token(&github).await, "sk-ant-oat01-test");
    let openai = federation::OpenAiFederation {
        token_url: format!("{}/openai/oauth/token", server.uri()),
        audience: federation::OPENAI_AUDIENCE.into(),
        identity_provider_id: "idp_test".into(),
        service_account_id: "svc_acct_test".into(),
    };
    assert_eq!(openai.access_token(&github).await, "openai-access-token");
}
