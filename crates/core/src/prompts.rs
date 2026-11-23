//! 議事録生成と AI チャットのプロンプト。

use chrono::{Local, TimeZone};

use crate::error::{CoreError, Result};
use crate::transcript::{Segment, Speaker};

/// プロンプトに渡す会議の情報。
pub struct MeetingContext<'a> {
    pub title: &'a str,
    pub created_at: i64,
    pub user_notes: &'a str,
    pub enhanced_notes: Option<&'a str>,
    pub segments: &'a [Segment],
    /// 議事録での自分の呼び名（空なら「自分」）
    pub my_name: &'a str,
}

const WEEKDAYS: [&str; 7] = ["日", "月", "火", "水", "木", "金", "土"];

fn local_datetime(ms: i64) -> String {
    use chrono::Datelike;
    match Local.timestamp_millis_opt(ms).single() {
        Some(t) => format!(
            "{}（{}） {}",
            t.format("%Y年%-m月%-d日"),
            WEEKDAYS[t.weekday().num_days_from_sunday() as usize],
            t.format("%H:%M")
        ),
        None => String::new(),
    }
}

fn local_time(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_default()
}

impl MeetingContext<'_> {
    fn speaker_name(&self, speaker: Speaker) -> String {
        match speaker {
            Speaker::Me if !self.my_name.trim().is_empty() => format!("自分（{}）", self.my_name.trim()),
            other => other.label().to_string(),
        }
    }

    /// `[15:04] 相手: 〜` の形の文字起こし。
    pub fn transcript_text(&self) -> String {
        self.segments
            .iter()
            .map(|s| format!("[{}] {}: {}", local_time(s.started_at), self.speaker_name(s.speaker), s.text))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn meeting_block(&self, include_enhanced: bool) -> String {
        let title = if self.title.trim().is_empty() {
            "（未設定）"
        } else {
            self.title.trim()
        };
        let notes = if self.user_notes.trim().is_empty() {
            "（なし）".to_string()
        } else {
            self.user_notes.trim().to_string()
        };
        let transcript = if self.segments.is_empty() {
            "（なし）".to_string()
        } else {
            self.transcript_text()
        };
        let mut out = format!(
            "<meeting>\nタイトル: {title}\n日時: {}\n</meeting>\n\n<my_notes>\n{notes}\n</my_notes>\n",
            local_datetime(self.created_at)
        );
        if include_enhanced {
            if let Some(enhanced) = self.enhanced_notes.filter(|e| !e.trim().is_empty()) {
                out.push_str(&format!("\n<minutes>\n{}\n</minutes>\n", enhanced.trim()));
            }
        }
        out.push_str(&format!("\n<transcript>\n{transcript}\n</transcript>"));
        out
    }
}

pub const ENHANCE_SYSTEM_PROMPT: &str = "\
あなたは会議の議事録を仕上げるアシスタントです。利用者は会議中、自分にとって大事な点だけを走り書きしています。\
そのメモと会議の文字起こしを突き合わせ、あとから読み返して役に立つ議事録にしてください。

大事にしてほしいこと:
- 利用者のメモは「何が重要か」を示しています。メモにある論点は必ず取り上げ、議事録の骨組みにしてください。\
メモの言い回しは、意味を変えない範囲でなるべく残してください（利用者が自分の書いた部分を見分けられるようにするため）。
- 文字起こしから、各論点の背景・数字・結論・担当者・期限を補ってください。メモになくても重要な決定事項や依頼は加えてください。
- 文字起こしは自動認識なので、誤変換や聞き取り違いがあり得ます。文脈から明らかな誤りは直し、判断できないものは推測で埋めずに省いてください。\
会議で話されていないことは書かないでください。
- 話者は「自分」（利用者）と「相手」（PC から聞こえた音声で、複数人のこともある）しか区別できません。相手の名前は会話の中で明らかな場合だけ使ってください。
- 文字起こしがない場合は、メモを整理して読みやすくするだけにしてください。

出力の形式:
- 会議で使われた言語（日本語の会議なら日本語）の Markdown で書いてください。
- 1 行目は「# 」で始まる短い会議タイトル（20 字程度）にしてください。
- 続けて、内容に合う見出し（## ）を立てて箇条書きでまとめてください。目安として、概要、論点ごとの要点、決定事項、\
ToDo（「- [ ] 担当: 内容（期限）」の形）、未決事項を使い、該当する内容がない見出しは省いてください。
- 簡潔に書いてください。1 項目は 1〜2 行を目安にし、前置きや結びの言葉は書かないでください。";

/// 議事録生成のユーザーメッセージ。メモも文字起こしもなければエラー。
pub fn enhance_user_message(ctx: &MeetingContext<'_>) -> Result<String> {
    if ctx.user_notes.trim().is_empty() && ctx.segments.is_empty() {
        return Err(CoreError::Other(
            "メモも文字起こしもないため、議事録を作れません。メモを書くか録音してから試してください".into(),
        ));
    }
    Ok(format!(
        "{}\n\n上の会議の議事録を作成してください。",
        ctx.meeting_block(false)
    ))
}

/// AI チャットのシステムプロンプト（会議の情報を含む）。会話中は変わらないのでキャッシュが効く。
pub fn chat_system_prompt(ctx: &MeetingContext<'_>) -> String {
    format!(
        "あなたは、ある会議ノートについて利用者の質問に答えるアシスタントです。\
以下の会議の情報（利用者のメモ、議事録、文字起こし）だけを根拠に答えてください。

- 利用者が使った言語で、簡潔に答えてください。Markdown を使ってかまいません。
- 会議で話されていない内容は推測せず、話されていないと伝えてください。
- 根拠になる発言があれば、その時刻（[15:04] など）を添えてください。
- メール文案やタスク一覧などを頼まれたら、そのまま使える形に整えてください。
- 話者は「自分」（利用者）と「相手」（PC から聞こえた音声で、複数人のこともある）しか区別できません。

{}",
        ctx.meeting_block(true)
    )
}

/// 生成結果の先頭の「# タイトル」を取り出し、本文と分ける。
pub fn split_title(markdown: &str) -> (Option<String>, String) {
    let trimmed = markdown.trim_start();
    if let Some(rest) = trimmed.strip_prefix("# ") {
        let (title, body) = rest.split_once('\n').unwrap_or((rest, ""));
        let title = title.trim().trim_matches('#').trim();
        return (
            (!title.is_empty()).then(|| title.to_string()),
            body.trim().to_string(),
        );
    }
    (None, markdown.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(id: i64, speaker: Speaker, text: &str) -> Segment {
        Segment {
            id,
            note_id: "n".into(),
            speaker,
            started_at: 1_700_000_000_000 + id * 60_000,
            ended_at: 1_700_000_000_000 + id * 60_000 + 5_000,
            text: text.into(),
        }
    }

    // [MIN-013]
    #[test]
    fn enhance_message_contains_notes_and_transcript() {
        let segments = [
            segment(1, Speaker::Them, "リリースは12月5日でいいですか"),
            segment(2, Speaker::Me, "はい、それで進めます"),
        ];
        let ctx = MeetingContext {
            title: "",
            created_at: 1_700_000_000_000,
            user_notes: "- リリース日\n",
            enhanced_notes: None,
            segments: &segments,
            my_name: "田花",
        };
        let msg = enhance_user_message(&ctx).unwrap();
        assert!(msg.contains("タイトル: （未設定）"));
        assert!(msg.contains("<my_notes>\n- リリース日\n</my_notes>"));
        assert!(msg.contains("相手: リリースは12月5日でいいですか"));
        assert!(msg.contains("自分（田花）: はい、それで進めます"));
        assert!(msg.contains("日時: 20"));
    }

    // [MIN-002]
    #[test]
    fn enhance_requires_some_content() {
        let ctx = MeetingContext {
            title: "定例",
            created_at: 0,
            user_notes: "  ",
            enhanced_notes: None,
            segments: &[],
            my_name: "",
        };
        assert!(enhance_user_message(&ctx).is_err());
    }

    // [CHT-008][MIN-013]
    #[test]
    fn chat_prompt_includes_minutes_and_uses_default_speaker_name() {
        let segments = [segment(1, Speaker::Me, "見積もりは来週出します")];
        let ctx = MeetingContext {
            title: "商談",
            created_at: 0,
            user_notes: "",
            enhanced_notes: Some("## 決定事項\n- 来週見積もり"),
            segments: &segments,
            my_name: "",
        };
        let prompt = chat_system_prompt(&ctx);
        assert!(prompt.contains("<minutes>\n## 決定事項\n- 来週見積もり\n</minutes>"));
        assert!(prompt.contains("] 自分: 見積もりは来週出します"));
        assert!(prompt.contains("<my_notes>\n（なし）\n</my_notes>"));
    }

    // [MIN-006]
    #[test]
    fn split_title_extracts_leading_heading() {
        assert_eq!(
            split_title("\n# 予算会議\n\n## 決定事項\n- A"),
            (Some("予算会議".into()), "## 決定事項\n- A".into())
        );
        assert_eq!(split_title("## 決定事項\n- A"), (None, "## 決定事項\n- A".into()));
        assert_eq!(split_title("# 題名だけ"), (Some("題名だけ".into()), "".into()));
    }
}
