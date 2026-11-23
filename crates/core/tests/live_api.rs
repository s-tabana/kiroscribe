//! 本物の API（OpenAI の文字起こし・Anthropic の Messages）が、いまもアプリの送り方を受け付けることを確かめる。
//!
//! 模擬サーバーのテストでは、API の側の変更（モデルの廃止・名前の変更・応答の形の変更）に気づけない。
//! アプリと同じクライアント・同じプロンプトで、設定画面で選べるすべてのモデルに、ごく短い問い合わせを送る。
//! API キーを、環境変数 `KIROSCRIBE_LIVE_OPENAI_KEY`・`KIROSCRIBE_LIVE_ANTHROPIC_KEY` で渡したときだけ問い合わせる
//! （料金がかかる。1 回で数円）。渡さなければ、何もせずに通る。CI では `.github/workflows/live-api.yml` が、
//! リポジトリの Secret（登録してあれば）を渡して、手動または週に 1 回走らせる。

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

fn key(name: &str) -> Option<String> {
    let value = std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    if value.is_none() {
        eprintln!("{name} がないので、本物の API には問い合わせません");
    }
    value
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
    let Some(api_key) = key("KIROSCRIBE_LIVE_ANTHROPIC_KEY") else { return };
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
        let result = ClaudeClient::new(&api_key, &model)
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
    let Some(api_key) = key("KIROSCRIBE_LIVE_ANTHROPIC_KEY") else { return };
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
    let result = ClaudeClient::new(&api_key, &model)
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
    let Some(api_key) = key("KIROSCRIBE_LIVE_OPENAI_KEY") else { return };
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
