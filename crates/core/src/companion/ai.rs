//! 会議の相棒の AI の呼び出し: 判定（資料を求める発言か）と選択（答えに使える資料を 1 件選ぶ）。
//!
//! どちらも Anthropic の `POST /messages` を、ストリーミングを使わずに 1 回で受け取る。
//! 決まった形（JSON Schema）で答えさせ、指示文はキャッシュの対象にし、effort は low。

use std::time::Duration;

use serde_json::{json, Value};

use super::candidates::{Candidate, Period};
use crate::claude::ClaudeClient;
use crate::error::{CoreError, Result};
use crate::retry::is_retryable;
use crate::search::normalize::normalize;
use crate::transcript::Speaker;

/// 判定・選択を打ち切るまでの時間（再試行を含む）。
pub const JUDGE_TIMEOUT: Duration = Duration::from_secs(10);
pub const SELECT_TIMEOUT: Duration = Duration::from_secs(15);
/// 出力の上限（トークン）。
pub const JUDGE_MAX_TOKENS: u32 = 2_000;
pub const SELECT_MAX_TOKENS: u32 = 4_000;
/// 判定に添える、同じノートのその前の発言（新しいほうから）の件数と、合わせた文字数の上限。
pub const CONTEXT_UTTERANCES: usize = 6;
pub const CONTEXT_CHARS: usize = 1_000;
/// 探すものの名前の長さの上限。
pub const TARGET_CHARS: usize = 20;
/// 検索語の数の下限と上限（正規化して重なりを除いたあとの数）。
pub const MIN_TERMS: usize = 3;
pub const MAX_TERMS: usize = 8;
/// 手がかりの数の上限。
pub const MAX_CLUES: usize = 3;
/// 一時的な失敗のあと、再試行するまで待つ時間（打ち切りの時間の中で 1 回だけ）。
const RETRY_DELAY: Duration = Duration::from_secs(1);

pub const JUDGE_SYSTEM_PROMPT: &str = "\
あなたは会議中の発言を見て、話し手が社内の資料や過去の記録（マニュアル・手順・スライド・議事録・Wiki・URL・前回決めたことなど）を\
求めているかを判定するアシスタントです。
- 資料や記録を探す・見せる・共有する必要がある発言なら requested を true にします。雑談・意見・資料と関係ない質問は false です。
- target は、探すものの短い名前（20 文字まで。例: 経費精算のマニュアル）です。requested が false なら空にします。
- terms は、資料を探すための検索語を 3〜8 個です。言い換えや略語、英語の表記も含めます。1 語は短くします。
- period は、時期の手がかりです。「前回」「先週」「先月」のどれかに当たるならその語、なければ「なし」にします。
- 直前の発言は、話の流れを知るためだけに使います。判定するのは「判定する発言」です。";

pub const SELECT_SYSTEM_PROMPT: &str = "\
あなたは会議中の発言に答えるため、候補の資料から 1 件を選ぶアシスタントです。
- choice は、発言の答えに使える資料の番号です。どれも使えなければ 0 にします。
- passage は、選んだ資料の抜粋の中で、答えに当たる 1 文から数文です。抜粋にある文字列をそのまま写します。抜粋がなければ空にします。
- clues は、発言と資料の両方に出てくる語（3 個まで）です。
- 候補にない資料を作らないでください。";

/// 判定する発言と、その前の発言。
#[derive(Debug, Clone)]
pub struct JudgeInput {
    pub note_title: String,
    pub speaker: Speaker,
    pub text: String,
    /// 同じノートの、その前の発言（古い順）
    pub previous: Vec<(Speaker, String)>,
}

/// 判定の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judgement {
    pub requested: bool,
    pub target: String,
    pub terms: Vec<String>,
    pub period: Option<Period>,
}

impl Judgement {
    pub fn not_requested() -> Self {
        Self { requested: false, target: String::new(), terms: Vec::new(), period: None }
    }
}

/// 選択の結果（AI が返したまま。候補と照らし合わせるのは [`resolve`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// 選んだ候補の番号（1 から）。0 は「なし」
    pub choice: usize,
    pub passage: String,
    pub clues: Vec<String>,
}

impl Selection {
    pub fn none() -> Self {
        Self { choice: 0, passage: String::new(), clues: Vec::new() }
    }
}

/// 見つかった資料（表示する題名・場所・抜粋は、候補のものを使う）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub candidate: Candidate,
    /// 抜粋の中で強調する箇所（抜粋の中にないときは None）
    pub passage: Option<String>,
    pub clues: Vec<String>,
}

fn speaker_label(speaker: Speaker) -> &'static str {
    match speaker {
        Speaker::Me => "自分",
        Speaker::Them => "相手",
    }
}

/// その前の発言を、新しいほうから 6 件、合わせて 1,000 文字まで選ぶ（古い順にして返す）。
pub fn context_utterances(previous: &[(Speaker, String)]) -> Vec<(Speaker, String)> {
    let mut picked = Vec::new();
    let mut chars = 0;
    for (speaker, text) in previous.iter().rev().take(CONTEXT_UTTERANCES) {
        let len = text.chars().count();
        if chars + len > CONTEXT_CHARS {
            break;
        }
        chars += len;
        picked.push((*speaker, text.clone()));
    }
    picked.reverse();
    picked
}

pub fn judge_user_message(input: &JudgeInput) -> String {
    let mut out = format!("ノートの題名: {}\n", if input.note_title.trim().is_empty() { "（なし）" } else { input.note_title.trim() });
    let context = context_utterances(&input.previous);
    if !context.is_empty() {
        out.push_str("直前の発言（古い順）:\n");
        for (speaker, text) in &context {
            out.push_str(&format!("- {}: {}\n", speaker_label(*speaker), text));
        }
    }
    out.push_str(&format!("判定する発言: {}: {}", speaker_label(input.speaker), input.text));
    out
}

pub fn judge_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "requested": { "type": "boolean" },
            "target": { "type": "string" },
            "terms": { "type": "array", "items": { "type": "string" } },
            "period": { "type": "string", "enum": ["前回", "先週", "先月", "なし"] }
        },
        "required": ["requested", "target", "terms", "period"],
        "additionalProperties": false
    })
}

pub fn select_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "choice": { "type": "integer" },
            "passage": { "type": "string" },
            "clues": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["choice", "passage", "clues"],
        "additionalProperties": false
    })
}

pub fn select_user_message(utterance: &str, target: &str, candidates: &[Candidate]) -> String {
    let mut out = format!("発言: {utterance}\n探すもの: {target}\n候補:\n");
    for (i, c) in candidates.iter().enumerate() {
        let source = match c.kind {
            super::candidates::SourceKind::Folder => "ローカルのフォルダ",
            super::candidates::SourceKind::Note => "Kiroscribe の過去のノート",
        };
        out.push_str(&format!("[{}] 題名: {}\n探し先: {source}\n場所: {}\n", i + 1, c.send_title(), c.send_place()));
        match c.send_excerpt() {
            Some(excerpt) => match c.part {
                Some(part) => out.push_str(&format!("抜粋（{part}）: {excerpt}\n")),
                None => out.push_str(&format!("抜粋: {excerpt}\n")),
            },
            None => out.push_str("抜粋: （なし）\n"),
        }
    }
    out
}

/// 決まった形の出力を求める `POST /messages` の本文（モデルは送るときに入れる）。
pub fn request_body(system: &str, user: String, schema: Value, max_tokens: u32) -> Value {
    json!({
        "max_tokens": max_tokens,
        "system": [{ "type": "text", "text": system, "cache_control": { "type": "ephemeral" } }],
        "messages": [{ "role": "user", "content": user }],
        "output_config": { "effort": "low", "format": { "type": "json_schema", "schema": schema } }
    })
}

/// 応答の JSON から、決まった形の答えを取り出す。内容を理由に控えたら None。
fn answer(response: &Value) -> Result<Option<Value>> {
    if response["stop_reason"] == "refusal" {
        return Ok(None);
    }
    let text = response["content"]
        .as_array()
        .and_then(|blocks| blocks.iter().find(|b| b["type"] == "text"))
        .and_then(|b| b["text"].as_str())
        .ok_or_else(|| CoreError::Other("相棒: 応答の形が違います".into()))?;
    serde_json::from_str(text).map(Some).map_err(|_| CoreError::Other("相棒: 応答の形が違います".into()))
}

/// `timeout` の中で呼び、一時的な失敗は 1 回だけ再試行する。
async fn call(client: &ClaudeClient, body: Value, timeout: Duration) -> Result<Value> {
    let deadline = tokio::time::Instant::now() + timeout;
    let run = async {
        let mut retried = false;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match client.create_message(body.clone(), remaining).await {
                Err(err) if !retried && is_retryable(&err) => {
                    retried = true;
                    tokio::time::sleep(RETRY_DELAY.min(deadline.saturating_duration_since(tokio::time::Instant::now()))).await;
                }
                other => return other,
            }
        }
    };
    match tokio::time::timeout_at(deadline, run).await {
        Ok(result) => result,
        Err(_) => Err(CoreError::Other(format!("相棒: {} 秒以内に応答がありませんでした", timeout.as_secs()))),
    }
}

pub fn parse_judgement(value: &Value) -> Result<Judgement> {
    let bad = || CoreError::Other("相棒: 応答の形が違います".into());
    let requested = value["requested"].as_bool().ok_or_else(bad)?;
    if !requested {
        return Ok(Judgement::not_requested());
    }
    let target: String = value["target"].as_str().ok_or_else(bad)?.trim().chars().take(TARGET_CHARS).collect();
    // 正規化して同じになる語（全角と半角・大文字と小文字の違いだけの語）は、1 つに数える
    let mut terms: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for term in value["terms"].as_array().ok_or_else(bad)? {
        let term = term.as_str().ok_or_else(bad)?.trim().to_string();
        let key = normalize(&term);
        if !key.trim().is_empty() && !seen.contains(&key) && terms.len() < MAX_TERMS {
            seen.push(key);
            terms.push(term);
        }
    }
    // 検索語は 3〜8 個（CMP-006）。足りなければ、決まった形の応答ではないものとして扱う
    if target.is_empty() || terms.len() < MIN_TERMS {
        return Err(bad());
    }
    Ok(Judgement { requested, target, terms, period: value["period"].as_str().and_then(Period::from_label) })
}

pub fn parse_selection(value: &Value) -> Result<Selection> {
    let bad = || CoreError::Other("相棒: 応答の形が違います".into());
    let choice = value["choice"].as_i64().ok_or_else(bad)?;
    let passage = value["passage"].as_str().ok_or_else(bad)?.to_string();
    let clues = value["clues"]
        .as_array()
        .ok_or_else(bad)?
        .iter()
        .map(|c| c.as_str().map(str::to_string).ok_or_else(bad))
        .collect::<Result<Vec<_>>>()?;
    Ok(Selection { choice: usize::try_from(choice).unwrap_or(0), passage, clues })
}

/// 判定。内容を理由に控えられたら「求めていない」と同じに扱う。
pub async fn judge(client: &ClaudeClient, input: &JudgeInput) -> Result<Judgement> {
    let body = request_body(JUDGE_SYSTEM_PROMPT, judge_user_message(input), judge_schema(), JUDGE_MAX_TOKENS);
    match answer(&call(client, body, JUDGE_TIMEOUT).await?)? {
        Some(value) => parse_judgement(&value),
        None => Ok(Judgement::not_requested()),
    }
}

/// 選択。内容を理由に控えられたら「なし」と同じに扱う。
pub async fn select(client: &ClaudeClient, utterance: &str, target: &str, candidates: &[Candidate]) -> Result<Selection> {
    let body = request_body(SELECT_SYSTEM_PROMPT, select_user_message(utterance, target, candidates), select_schema(), SELECT_MAX_TOKENS);
    match answer(&call(client, body, SELECT_TIMEOUT).await?)? {
        Some(value) => parse_selection(&value),
        None => Ok(Selection::none()),
    }
}

/// 選択の結果を候補と照らし合わせる。候補にない番号は「なし」。表示する題名・場所・抜粋は候補のものを使う。
/// 該当する箇所が抜粋の中にないときは強調しない。発言にも資料にもない手がかりは除く。
pub fn resolve(selection: &Selection, candidates: &[Candidate], utterance: &str) -> Option<Found> {
    let candidate = selection.choice.checked_sub(1).and_then(|i| candidates.get(i))?.clone();
    let passage = selection.passage.trim();
    let passage = (!passage.is_empty() && candidate.excerpt.as_deref().is_some_and(|e| e.contains(passage))).then(|| passage.to_string());
    let spoken = normalize(utterance);
    let material = normalize(&format!("{} {} {}", candidate.title, candidate.location, candidate.excerpt.as_deref().unwrap_or("")));
    let mut clues: Vec<String> = Vec::new();
    for clue in &selection.clues {
        let clue = clue.trim();
        let n = normalize(clue);
        if !n.is_empty() && spoken.contains(&n) && material.contains(&n) && !clues.iter().any(|c| c == clue) && clues.len() < MAX_CLUES {
            clues.push(clue.to_string());
        }
    }
    Some(Found { candidate, passage, clues })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion::candidates::SourceKind;

    fn candidate(title: &str, excerpt: Option<&str>) -> Candidate {
        Candidate {
            kind: SourceKind::Folder,
            title: title.into(),
            location: format!("C:\\Users\\u\\共有資料\\{title}.docx"),
            send_location: format!("共有資料\\{title}.docx"),
            excerpt: excerpt.map(str::to_string),
            sent_excerpt: None,
            part: None,
            no_content: false,
            path: None,
            note_id: None,
        }
    }

    // [CMP-006]
    #[test]
    fn judge_message_has_title_the_utterance_and_up_to_six_previous() {
        let previous: Vec<(Speaker, String)> = (1..=8).map(|i| (Speaker::Them, format!("発言{i}"))).collect();
        let message = judge_user_message(&JudgeInput {
            note_title: "週次定例".into(),
            speaker: Speaker::Me,
            text: "経費精算のマニュアルどこ？".into(),
            previous,
        });
        assert!(message.contains("ノートの題名: 週次定例"));
        assert!(message.contains("判定する発言: 自分: 経費精算のマニュアルどこ？"));
        assert!(!message.contains("発言2\n") && message.contains("発言3") && message.contains("発言8"), "新しいほうから 6 件: {message}");
        assert!(message.find("発言3").unwrap() < message.find("発言8").unwrap(), "古い順");

        let long: Vec<(Speaker, String)> = vec![(Speaker::Me, "古".repeat(400)), (Speaker::Them, "中".repeat(400)), (Speaker::Me, "新".repeat(400))];
        let picked = context_utterances(&long);
        assert_eq!(picked.len(), 2, "合わせて 1,000 文字まで");
        assert!(picked[1].1.starts_with('新'));
    }

    // [CMP-006][CMP-007]
    #[test]
    fn judgement_is_parsed_and_limited() {
        let value = json!({ "requested": true, "target": "経費精算のマニュアルの最新版と古い版の違い", "terms": ["経費精算", "経費精算", " 精算 ", "", "1","2","3","4","5","6","7"], "period": "前回" });
        let judgement = parse_judgement(&value).unwrap();
        assert_eq!(judgement.target.chars().count(), TARGET_CHARS);
        assert_eq!(judgement.terms.len(), MAX_TERMS);
        assert_eq!(&judgement.terms[..2], ["経費精算", "精算"]);
        assert_eq!(judgement.period, Some(Period::Previous));
        let none = parse_judgement(&json!({ "requested": false, "target": "", "terms": [], "period": "なし" })).unwrap();
        assert_eq!(none, Judgement::not_requested(), "求めていなければ、何も探さない");
        assert!(parse_judgement(&json!({ "requested": "yes" })).is_err(), "決まった形でない");
        assert!(parse_judgement(&json!({ "requested": true, "target": "x", "terms": [], "period": "なし" })).is_err());
    }

    // [CMP-006]
    #[test]
    fn judgement_needs_three_to_eight_terms_after_normalizing() {
        let judged = |terms: Value| parse_judgement(&json!({ "requested": true, "target": "VPN の設定", "terms": terms, "period": "なし" }));
        assert!(judged(json!(["VPN", "ＶＰＮ", "vpn", " ", "設定"])).is_err(), "正規化して重なりを除くと 2 個");
        assert!(judged(json!(["VPN", "設定"])).is_err(), "2 個");
        assert_eq!(judged(json!(["VPN", "ＶＰＮ", "設定", "接続"])).unwrap().terms, ["VPN", "設定", "接続"], "3 個");
        let eight: Vec<String> = (1..=8).map(|i| format!("語{i}")).collect();
        assert_eq!(judged(json!(eight)).unwrap().terms.len(), 8, "8 個");
        let nine: Vec<String> = (1..=9).map(|i| format!("語{i}")).collect();
        assert_eq!(judged(json!(nine)).unwrap().terms.len(), MAX_TERMS, "9 個目からは使わない");
    }

    // [CMP-013]
    #[test]
    fn select_message_has_candidates_in_the_send_form() {
        let mut secret = candidate("人事評価", Some("評価の手順 https://hr.example.com/a?token=1"));
        let mut titled = candidate("HTTPS://Sheets.example.com/s?Key=9", Some("先頭が切れた URL"));
        titled.sent_excerpt = Some("先頭が切れた URL".into());
        secret.no_content = true;
        let message = select_user_message("評価の手順どこ？", "評価の手順", &[candidate("経費精算", Some("申請は https://wiki.example.com/x?id=3#top")), secret, titled]);
        assert!(message.contains("発言: 評価の手順どこ？\n探すもの: 評価の手順"));
        assert!(message.contains("[1] 題名: 経費精算\n探し先: ローカルのフォルダ\n場所: 共有資料\\経費精算.docx\n抜粋: 申請は https://wiki.example.com/x\n"));
        assert!(message.contains("[2] 題名: 人事評価") && message.contains("抜粋: （なし）"), "「中身を送らない」のフォルダは抜粋を送らない");
        assert!(!message.contains("C:\\Users"), "フルパスは送らない");
        assert!(!message.contains("token=1"));
        assert!(message.contains("[3] 題名: HTTPS://Sheets.example.com/s\n"), "題名の中の URL も整える: {message}");
        assert!(!message.contains("Key=9"), "場所の中の URL も整える");
    }

    // [CMP-014]
    #[test]
    fn selection_uses_the_candidate_and_checks_passage_and_clues() {
        let candidates = vec![candidate("経費精算", Some("申請は月末まで。上限は 5 万円。")), candidate("出張", None)];
        let found = resolve(
            &Selection { choice: 1, passage: "上限は 5 万円。".into(), clues: vec!["経費".into(), "上限".into(), "宇宙".into(), "経費".into()] },
            &candidates,
            "経費の上限ってどこに書いてある？",
        )
        .unwrap();
        assert_eq!(found.candidate, candidates[0], "題名・場所・抜粋は候補のもの");
        assert_eq!(found.passage.as_deref(), Some("上限は 5 万円。"));
        assert_eq!(found.clues, ["経費", "上限"], "発言にも資料にもない語は除く");

        let unhighlighted = resolve(&Selection { choice: 1, passage: "AI が作った文".into(), clues: vec![] }, &candidates, "x").unwrap();
        assert_eq!(unhighlighted.passage, None, "抜粋にない箇所は強調しない");
        assert_eq!(resolve(&Selection { choice: 3, ..Selection::none() }, &candidates, "x"), None, "候補にない番号は「なし」");
        assert_eq!(resolve(&Selection::none(), &candidates, "x"), None);
        assert_eq!(parse_selection(&json!({ "choice": -1, "passage": "", "clues": [] })).unwrap().choice, 0);
    }

    // [CMP-019]
    #[test]
    fn refusals_are_treated_as_no_answer() {
        assert_eq!(answer(&json!({ "stop_reason": "refusal", "content": [] })).unwrap(), None);
        let text = json!({ "stop_reason": "end_turn", "content": [{ "type": "thinking", "thinking": "" }, { "type": "text", "text": "{\"a\":1}" }] });
        assert_eq!(answer(&text).unwrap(), Some(json!({ "a": 1 })));
        assert!(answer(&json!({ "stop_reason": "end_turn", "content": [{ "type": "text", "text": "JSON ではない" }] })).is_err());
        // 別のモデルが続きを書く仕組みは使わない
        let body = request_body(JUDGE_SYSTEM_PROMPT, "x".into(), judge_schema(), JUDGE_MAX_TOKENS);
        assert!(body.get("fallbacks").is_none());
    }
}
