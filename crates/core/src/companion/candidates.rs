//! 資料の候補: 探し先（指定したフォルダ・過去のノート）を同時に探して上位 8 件を選び、AI に送る形を作る。

use std::path::Path;

use chrono::{Datelike, Duration as Days, Local, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};

use crate::search::normalize::normalize;
use crate::search::store::{Hit, NoteHit};
use crate::search::SearchFolder;

/// 候補の上限。
pub const MAX_CANDIDATES: usize = 8;

/// 時期の手がかり（判定が返すもの）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Period {
    /// 前回
    Previous,
    /// 先週
    LastWeek,
    /// 先月
    LastMonth,
}

impl Period {
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "前回" => Some(Period::Previous),
            "先週" => Some(Period::LastWeek),
            "先月" => Some(Period::LastMonth),
            _ => None,
        }
    }
}

/// 探し先の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceKind {
    /// 指定したフォルダ
    Folder,
    /// Kiroscribe の過去のノート
    Note,
}

/// 資料の候補 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub kind: SourceKind,
    pub title: String,
    /// 保存して、画面の表示・コピー・開くに使う場所（ファイルのパス、またはノートの題名と作成日時）
    pub location: String,
    /// AI に送る場所（資料を特定できる最小の形）
    pub send_location: String,
    /// 一致した部分の前後（名前だけで探した資料・題名だけに一致したノートにはない）
    pub excerpt: Option<String>,
    /// 過去のノートの、一致した部分の種類（「メモ」「議事録」「文字起こし」）
    pub part: Option<&'static str>,
    /// 「中身を送らない」にしたフォルダの資料（抜粋を AI に送らない）
    pub no_content: bool,
    /// ファイルのパス（指定したフォルダの資料）
    pub path: Option<String>,
    /// ノートの ID（過去のノート）
    pub note_id: Option<String>,
}

impl Candidate {
    /// AI に送る抜粋（「中身を送らない」なら送らない。中の URL は整える）。
    pub fn send_excerpt(&self) -> Option<String> {
        if self.no_content {
            return None;
        }
        self.excerpt.as_deref().map(strip_url_queries)
    }
}

/// 録音中のノート（探す対象から除き、時期の手がかりの基準にする）。
#[derive(Debug, Clone)]
pub struct CurrentNote {
    pub id: String,
    pub title: String,
    pub created_at: i64,
}

/// URL のクエリ（「?」から後ろ）とフラグメント（「#」から後ろ）を除く。文の中の URL もすべて整える。
pub fn strip_url_queries(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let next = ["https://", "http://"].iter().filter_map(|scheme| rest.find(scheme)).min();
        let Some(start) = next else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..start]);
        let url_len = rest[start..]
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '）' | '」' | '、' | '。'))
            .unwrap_or(rest.len() - start);
        let url = &rest[start..start + url_len];
        let cut = url.find(['?', '#']).unwrap_or(url.len());
        out.push_str(&url[..cut]);
        rest = &rest[start + url_len..];
    }
}

/// AI に送るローカルのフォルダの資料の場所: 追加したフォルダの名前と、そこからの相対のパス。
pub fn folder_send_location(folder: &Path, path: &Path) -> String {
    let name = folder.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let relative = path.strip_prefix(folder).unwrap_or(path);
    let mut parts = vec![name];
    parts.extend(relative.components().map(|c| c.as_os_str().to_string_lossy().to_string()));
    parts.retain(|p| !p.is_empty());
    parts.join(std::path::MAIN_SEPARATOR_STR)
}

const WEEKDAYS: [&str; 7] = ["月", "火", "水", "木", "金", "土", "日"];

/// 作成日時の表示（「2026年11月20日（金） 15:04」。画面のノートの上部と同じ形）。
pub fn note_date_label(created_at: i64) -> String {
    match Local.timestamp_millis_opt(created_at).single() {
        Some(at) => format!(
            "{}年{}月{}日（{}） {}",
            at.year(),
            at.month(),
            at.day(),
            WEEKDAYS[at.weekday().num_days_from_monday() as usize],
            at.format("%H:%M")
        ),
        None => String::new(),
    }
}

/// 過去のノートの場所（「ノート「<題名>」 <作成日時>」）。
pub fn note_location(title: &str, created_at: i64) -> String {
    let title = if title.trim().is_empty() { "無題" } else { title.trim() };
    format!("ノート「{title}」 {}", note_date_label(created_at))
}

/// 記号と空白を除いて正規化した文字列（題名の似かたを比べるため）。
fn plain(text: &str) -> Vec<char> {
    normalize(text).chars().filter(|c| c.is_alphanumeric()).collect()
}

/// 2 文字ずつの重なり（Dice 係数）。議事録の色分け（MIN-012）と同じ。
pub fn title_similarity(a: &str, b: &str) -> f64 {
    let grams = |chars: &[char]| -> std::collections::HashSet<(char, char)> {
        let mut set: std::collections::HashSet<(char, char)> = chars.windows(2).map(|w| (w[0], w[1])).collect();
        if chars.len() == 1 {
            set.insert((chars[0], '\0'));
        }
        set
    };
    let (ga, gb) = (grams(&plain(a)), grams(&plain(b)));
    if ga.is_empty() || gb.is_empty() {
        return 0.0;
    }
    let shared = ga.intersection(&gb).count();
    2.0 * shared as f64 / (ga.len() + gb.len()) as f64
}

/// 期間（その日の 0 時から、終わりの日の 0 時まで。ミリ秒）。
fn day_range(from: NaiveDate, to: NaiveDate) -> (i64, i64) {
    let ms = |day: NaiveDate| {
        Local
            .from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .map(|t| t.timestamp_millis())
            .unwrap_or(0)
    };
    (ms(from), ms(to))
}

/// 「先週」（月曜から始まる前の週）と「先月」（前の月）の期間。
pub fn period_range(period: Period, now: i64) -> Option<(i64, i64)> {
    let today = Local.timestamp_millis_opt(now).single()?.date_naive();
    match period {
        Period::Previous => None,
        Period::LastWeek => {
            let this_monday = today - Days::days(today.weekday().num_days_from_monday() as i64);
            Some(day_range(this_monday - Days::days(7), this_monday))
        }
        Period::LastMonth => {
            let first = today.with_day(1)?;
            let previous = (first - Days::days(1)).with_day(1)?;
            Some(day_range(previous, first))
        }
    }
}

/// 時期の手がかりでノートを上げる段（大きいほど上）。
fn note_tier(hit: &NoteHit, period: Option<Period>, current: Option<&CurrentNote>, now: i64) -> u8 {
    match period {
        Some(Period::Previous) => {
            let Some(current) = current else { return 0 };
            if hit.created_at >= current.created_at {
                return 0;
            }
            if title_similarity(&hit.title, &current.title) >= 0.5 {
                2
            } else {
                1
            }
        }
        Some(p) => match period_range(p, now) {
            Some((from, to)) if hit.created_at >= from && hit.created_at < to => 1,
            _ => 0,
        },
        None => 0,
    }
}

/// フォルダと過去のノートの検索結果を合わせ、上位 8 件までの候補にする。
/// 並びは、時期の手がかりで上げる段、一致した検索語の数、題名やファイル名に一致したか、新しさの順。
pub fn merge(
    hits: Vec<Hit>,
    note_hits: Vec<NoteHit>,
    folders: &[SearchFolder],
    period: Option<Period>,
    current: Option<&CurrentNote>,
    now: i64,
) -> Vec<Candidate> {
    struct Ranked {
        key: (u8, usize, bool, i64),
        candidate: Candidate,
    }
    let mut ranked: Vec<Ranked> = Vec::new();
    for hit in hits {
        let no_content = folders.iter().any(|f| f.no_content && Path::new(&f.path) == hit.folder);
        let location = hit.path.to_string_lossy().to_string();
        ranked.push(Ranked {
            key: (0, hit.matched_terms, hit.named, hit.modified),
            candidate: Candidate {
                kind: SourceKind::Folder,
                title: hit.title.clone(),
                send_location: folder_send_location(&hit.folder, &hit.path),
                location: location.clone(),
                excerpt: hit.excerpt.clone(),
                part: None,
                no_content,
                path: Some(location),
                note_id: None,
            },
        });
    }
    for hit in note_hits {
        let tier = note_tier(&hit, period, current, now);
        let location = note_location(&hit.title, hit.created_at);
        ranked.push(Ranked {
            key: (tier, hit.matched_terms, hit.in_title, hit.created_at),
            candidate: Candidate {
                kind: SourceKind::Note,
                title: if hit.title.trim().is_empty() { "無題".into() } else { hit.title.clone() },
                send_location: location.clone(),
                location,
                excerpt: hit.excerpt.as_ref().map(|(_, text)| text.clone()),
                part: hit.excerpt.as_ref().map(|(part, _)| part.label()),
                no_content: false,
                path: None,
                note_id: Some(hit.note_id.clone()),
            },
        });
    }
    // 並べ替えは安定なので、同じ順位どうしは、探し先ごとの並びのまま
    ranked.sort_by(|a, b| b.key.cmp(&a.key));
    ranked.into_iter().take(MAX_CANDIDATES).map(|r| r.candidate).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ms(y: i32, m: u32, d: u32, h: u32) -> i64 {
        Local.with_ymd_and_hms(y, m, d, h, 0, 0).unwrap().timestamp_millis()
    }

    fn note_hit(id: &str, title: &str, created_at: i64, matched: usize) -> NoteHit {
        NoteHit {
            note_id: id.into(),
            title: title.into(),
            created_at,
            excerpt: Some((crate::search::store::NotePart::Minutes, format!("{title}の議事録"))),
            matched_terms: matched,
            in_title: false,
        }
    }

    fn folder_hit(path: &str, matched: usize) -> Hit {
        Hit {
            path: PathBuf::from(path),
            folder: PathBuf::from("/home/u/共有資料"),
            title: "経費精算".into(),
            file_name: "経費精算.docx".into(),
            excerpt: Some("申請は https://wiki.example.com/page?id=1&token=x#top を見る".into()),
            modified: 1,
            matched_terms: matched,
            named: false,
        }
    }

    // [CMP-043]
    #[test]
    fn send_forms_drop_full_paths_and_url_queries() {
        let folder = Path::new("/home/tabana/共有資料");
        let sep = std::path::MAIN_SEPARATOR_STR;
        assert_eq!(
            folder_send_location(folder, &folder.join("経理").join("経費精算.docx")),
            format!("共有資料{sep}経理{sep}経費精算.docx")
        );
        assert_eq!(
            strip_url_queries("見て https://a.example.com/x?y=1#z と http://b.example.com/p#frag。"),
            "見て https://a.example.com/x と http://b.example.com/p。"
        );
        assert_eq!(strip_url_queries("URL なし"), "URL なし");

        let candidates = merge(vec![folder_hit("/home/u/共有資料/経理/経費精算.docx", 1)], vec![], &[], None, None, 0);
        let c = &candidates[0];
        assert!(!c.send_location.contains("/home/u"), "フルパスは送らない: {}", c.send_location);
        assert_eq!(c.location, "/home/u/共有資料/経理/経費精算.docx", "画面の表示・開くには保存した場所を使う");
        assert_eq!(c.send_excerpt().unwrap(), "申請は https://wiki.example.com/page を見る");
        assert!(c.excerpt.as_ref().unwrap().contains("token=x"), "保存する抜粋はそのまま");
    }

    // [CMP-013][IDX-034]
    #[test]
    fn no_content_folders_send_no_excerpt() {
        let folders = vec![SearchFolder { path: "/home/u/共有資料".into(), no_content: true, place: None }];
        let candidates = merge(vec![folder_hit("/home/u/共有資料/a.docx", 1)], vec![], &folders, None, None, 0);
        assert!(candidates[0].no_content);
        assert_eq!(candidates[0].send_excerpt(), None);
        assert!(candidates[0].excerpt.is_some(), "画面には抜粋を出す");
    }

    // [CMP-012]
    #[test]
    fn merges_sources_into_the_top_eight() {
        let hits: Vec<Hit> = (0..6).map(|i| folder_hit(&format!("/home/u/共有資料/{i}.docx"), 1)).collect();
        let notes: Vec<NoteHit> = (0..6).map(|i| note_hit(&format!("n{i}"), "定例", i, if i == 5 { 2 } else { 1 })).collect();
        let candidates = merge(hits, notes, &[], None, None, 0);
        assert_eq!(candidates.len(), MAX_CANDIDATES);
        assert_eq!(candidates[0].note_id.as_deref(), Some("n5"), "一致した検索語の多いものが上");
        assert!(candidates.iter().any(|c| c.kind == SourceKind::Folder) && candidates.iter().any(|c| c.kind == SourceKind::Note));
        let note = candidates.iter().find(|c| c.kind == SourceKind::Note).unwrap();
        assert_eq!(note.part, Some("議事録"));
        assert!(note.location.starts_with("ノート「定例」 "), "{}", note.location);
    }

    // [IDX-018]
    #[test]
    fn previous_meeting_prefers_earlier_notes_with_a_similar_title() {
        let current = CurrentNote { id: "now".into(), title: "週次定例（11月27日）".into(), created_at: ms(2026, 11, 27, 10) };
        let notes = vec![
            note_hit("later", "週次定例（12月）", ms(2026, 11, 28, 10), 1),
            note_hit("other", "採用面談", ms(2026, 11, 26, 10), 1),
            note_hit("similar", "週次定例（11月20日）", ms(2026, 11, 20, 10), 1),
            note_hit("older", "予算", ms(2026, 11, 1, 10), 1),
        ];
        let ids: Vec<String> = merge(vec![], notes, &[], Some(Period::Previous), Some(&current), ms(2026, 11, 27, 11))
            .into_iter()
            .map(|c| c.note_id.unwrap())
            .collect();
        assert_eq!(ids, ["similar", "other", "older", "later"]);
        assert!(title_similarity("週次定例（11月27日）", "週次定例（11月20日）") >= 0.5);
        assert!(title_similarity("週次定例", "採用面談") < 0.5);
    }

    // [IDX-018]
    #[test]
    fn last_week_and_last_month_prefer_notes_in_that_period() {
        // 2026-11-27 は金曜日。先週は 11/16（月）〜 11/22（日）、先月は 10 月
        let now = ms(2026, 11, 27, 12);
        assert_eq!(period_range(Period::LastWeek, now), Some((ms(2026, 11, 16, 0), ms(2026, 11, 23, 0))));
        assert_eq!(period_range(Period::LastMonth, now), Some((ms(2026, 10, 1, 0), ms(2026, 11, 1, 0))));
        let notes = || {
            vec![
                note_hit("this-week", "a", ms(2026, 11, 24, 10), 1),
                note_hit("last-week", "b", ms(2026, 11, 18, 10), 1),
                note_hit("last-month", "c", ms(2026, 10, 15, 10), 1),
            ]
        };
        let first = |period| merge(vec![], notes(), &[], Some(period), None, now)[0].note_id.clone().unwrap();
        assert_eq!(first(Period::LastWeek), "last-week");
        assert_eq!(first(Period::LastMonth), "last-month");
        assert_eq!(merge(vec![], notes(), &[], None, None, now)[0].note_id.as_deref(), Some("this-week"), "手がかりがなければ新しい順");
    }

    // [IDX-019]
    #[test]
    fn note_date_label_matches_the_note_header() {
        assert_eq!(note_date_label(ms(2026, 11, 20, 15) + 4 * 60_000), "2026年11月20日（金） 15:04");
        assert_eq!(note_location("", ms(2026, 11, 20, 15)), "ノート「無題」 2026年11月20日（金） 15:00");
    }
}
