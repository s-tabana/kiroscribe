//! ノートの Markdown 書き出し（共有・保存用）。

use chrono::{Local, TimeZone};
use serde::{Deserialize, Serialize};

use crate::notes::Note;
use crate::transcript::{Segment, Speaker};

/// 書き出しに含める内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOptions {
    pub minutes: bool,
    pub notes: bool,
    pub transcript: bool,
}

const WEEKDAYS: [&str; 7] = ["日", "月", "火", "水", "木", "金", "土"];

fn datetime_label(ms: i64) -> String {
    use chrono::Datelike;
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| {
            format!(
                "{}（{}） {}",
                t.format("%Y年%-m月%-d日"),
                WEEKDAYS[t.weekday().num_days_from_sunday() as usize],
                t.format("%H:%M")
            )
        })
        .unwrap_or_default()
}

fn time_label(ms: i64) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_default()
}

fn title_of(note: &Note) -> &str {
    let title = note.title.trim();
    if title.is_empty() {
        "無題のノート"
    } else {
        title
    }
}

/// 共有用の Markdown。議事録の見出し（##）がそのまま節になるよう、議事録は日時の直後に置く。
pub fn note_to_markdown(note: &Note, segments: &[Segment], options: ExportOptions, my_name: &str) -> String {
    let mut out = format!("# {}\n\n{}\n", title_of(note), datetime_label(note.created_at));

    let minutes = note.enhanced_notes.as_deref().map(str::trim).filter(|m| !m.is_empty());
    if options.minutes {
        if let Some(minutes) = minutes {
            out.push_str(&format!("\n{minutes}\n"));
        }
    }

    let notes = note.user_notes.trim();
    // 議事録がないときは、議事録の代わりに自分のメモを入れる
    if (options.notes || (options.minutes && minutes.is_none())) && !notes.is_empty() {
        out.push_str(&format!("\n## 自分のメモ\n\n{notes}\n"));
    }

    if options.transcript && !segments.is_empty() {
        let me = if my_name.trim().is_empty() {
            Speaker::Me.label().to_string()
        } else {
            my_name.trim().to_string()
        };
        out.push_str("\n## 文字起こし\n\n");
        for segment in segments {
            let speaker = match segment.speaker {
                Speaker::Me => me.as_str(),
                Speaker::Them => Speaker::Them.label(),
            };
            out.push_str(&format!(
                "- [{}] **{}**: {}\n",
                time_label(segment.started_at),
                speaker,
                segment.text.trim()
            ));
        }
    }
    out
}

/// 保存時の既定のファイル名: `2026-11-20 定例.md`。Windows で使えない文字は置き換える。
pub fn suggested_file_name(note: &Note) -> String {
    let date = Local
        .timestamp_millis_opt(note.created_at)
        .single()
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_default();
    let title: String = title_of(note)
        .chars()
        .map(|c| if r#"\/:*?"<>|"#.contains(c) || c.is_control() { '_' } else { c })
        .take(80)
        .collect();
    let title = title.trim().trim_end_matches('.');
    format!("{date} {title}.md").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(title: &str, user_notes: &str, enhanced: Option<&str>) -> Note {
        let t = Local.with_ymd_and_hms(2026, 11, 20, 15, 4, 0).unwrap().timestamp_millis();
        Note {
            id: "n".into(),
            title: title.into(),
            user_notes: user_notes.into(),
            enhanced_notes: enhanced.map(str::to_string),
            enhanced_at: None,
            created_at: t,
            updated_at: t,
        }
    }

    fn segments() -> Vec<Segment> {
        let t = Local.with_ymd_and_hms(2026, 11, 20, 15, 5, 0).unwrap().timestamp_millis();
        vec![
            Segment {
                id: 1,
                note_id: "n".into(),
                speaker: Speaker::Them,
                started_at: t,
                ended_at: t + 3_000,
                text: "リリースはいつですか".into(),
            },
            Segment {
                id: 2,
                note_id: "n".into(),
                speaker: Speaker::Me,
                started_at: t + 60_000,
                ended_at: t + 63_000,
                text: "12月5日です ".into(),
            },
        ]
    }

    const ALL: ExportOptions = ExportOptions {
        minutes: true,
        notes: true,
        transcript: true,
    };

    // [SHR-004][SHR-006]
    #[test]
    fn exports_all_sections_in_order() {
        let n = note("定例", "- 日程", Some("## 決定事項\n- 12/5 リリース"));
        let md = note_to_markdown(&n, &segments(), ALL, "田花");
        assert_eq!(
            md,
            "# 定例\n\n2026年11月20日（金） 15:04\n\n## 決定事項\n- 12/5 リリース\n\n## 自分のメモ\n\n- 日程\n\n## 文字起こし\n\n- [15:05] **相手**: リリースはいつですか\n- [15:06] **田花**: 12月5日です\n"
        );
    }

    // [SHR-004][SHR-005]
    #[test]
    fn minutes_only_and_fallback_to_notes() {
        let only_minutes = ExportOptions {
            minutes: true,
            notes: false,
            transcript: false,
        };
        let with_minutes = note("", "- メモ", Some("## 概要\n- A"));
        let md = note_to_markdown(&with_minutes, &segments(), only_minutes, "");
        assert!(md.starts_with("# 無題のノート\n"));
        assert!(md.contains("## 概要\n- A"));
        assert!(!md.contains("自分のメモ"));
        assert!(!md.contains("文字起こし"));

        // 議事録がまだなければメモを入れる
        let without = note("定例", "- メモ", None);
        let md = note_to_markdown(&without, &[], only_minutes, "");
        assert!(md.contains("## 自分のメモ\n\n- メモ"));
    }

    // [SHR-006]
    #[test]
    fn transcript_uses_default_speaker_label() {
        let n = note("定例", "", None);
        let md = note_to_markdown(
            &n,
            &segments(),
            ExportOptions {
                minutes: false,
                notes: false,
                transcript: true,
            },
            " ",
        );
        assert!(md.contains("**自分**: 12月5日です"));
    }

    // [SHR-008]
    #[test]
    fn file_name_is_safe_for_windows() {
        assert_eq!(suggested_file_name(&note("定例 1/2: 予算?", "", None)), "2026-11-20 定例 1_2_ 予算_.md");
        assert_eq!(suggested_file_name(&note("", "", None)), "2026-11-20 無題のノート.md");
        assert_eq!(suggested_file_name(&note("報告書...", "", None)), "2026-11-20 報告書.md");
    }
}
