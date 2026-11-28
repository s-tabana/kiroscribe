//! 会議の検出の記録。利用者が書き出して、検出がうまくいかなかったときに調べる材料と、回帰テストの材料にする。
//!
//! 調べるたびの判定表の当てはめの結果（サービスごとの文脈と、立っている印。[`Evaluation`]）と、確定した出来事とその理由を、
//! 様子が変わったときと、出来事があったときだけ残す。個人の情報を残しすぎないよう:
//! - 文脈は、アプリの実行ファイルの名前と、窓の識別子（HWND の値）だけ。印は判定表の名前と強さだけ
//! - 窓やタブの題は、「詳しい記録」がオンのときだけ、印の立っている窓のものを残す
//! - 記録はメモリーにだけ置き（新しいものから [`CAPACITY`] 件）、利用者が書き出すまでディスクには書かない
//! - 書き出す（保存する・提出する）ときは、[`MeetingRecord::redacted`] で題の API キーの形の文字列・URL のクエリ・
//!   利用者のフォルダの名前を伏せる
//!
//! 書き出したファイル（[`MeetingRecord`]）は、[`replay`] で製品と同じ判定のまま、実際に調べた時刻だけで再生できる。
//! 旧い形式の記録は、[`replay_json`] で形式を先に読み、その形式の判定と再生のしかたで再生する。
//! `expected` に起きるはずの出来事を書き足して `crates/core/tests/fixtures/meetings/` に置くと、回帰テストになる。

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::meeting_facts::Facts;
use crate::meeting_rules::{Evaluation, Rules, ServiceEvaluation};
use crate::meetings::{Decision, Judge, MeetingEvent, POLL_MS};

/// 書き出すファイルの形式の名前。形か再生のしかたを変えたら、末尾の番号を上げる。
pub const FORMAT: &str = "kiroscribe-meeting-record/3";
/// 1 つ前の形式（記録 1 件の `at` から `until` まで 2 秒ごとに調べ、次の記録までの間は、窓の中を調べずに同じ印が
/// 続いていたものとして再生する。最後の記録のあとも、終了の猶予がたつまで進める）。
pub const FORMAT_V2: &str = "kiroscribe-meeting-record/2";
/// メモリーに残す件数（古いものから捨てる）。
pub const CAPACITY: usize = 300;

/// 確定した出来事と、その理由。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedDecision {
    /// [`event_name`] の形
    pub event: String,
    /// 理由の説明（開始は立っていた印、終了は猶予の経過・打ち消しの印・別の会議への切り替え）
    pub reason: String,
}

/// 印の立っている窓の題（「詳しい記録」のときだけ残す）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedTitle {
    pub app: String,
    pub window: u64,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordEntry {
    /// 調べた時刻（UNIX ミリ秒）
    pub at: i64,
    /// 同じ様子を続けて調べた最後の時刻（UNIX ミリ秒。`at` の 1 回だけなら無し）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<i64>,
    /// `at` から `until` までに調べた回数（`at` の 1 回だけなら無し）
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub count: u32,
    /// 判定表の当てはめの結果（印の立っている文脈のあるサービスだけ）
    pub services: Vec<ServiceEvaluation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub titles: Vec<RecordedTitle>,
    /// そのときに確定した出来事
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<RecordedDecision>,
}

fn one() -> u32 {
    1
}

fn is_one(count: &u32) -> bool {
    *count == 1
}

/// 書き出すファイルの中身。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingRecord {
    pub format: String,
    pub app_version: String,
    /// 判定表の版（[`Rules::version`]）
    pub rules_version: u32,
    pub entries: Vec<RecordEntry>,
    /// 回帰テストで、再生したときに起きるはずの出来事（[`event_name`] の形）。書き出したときは空
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected: Vec<String>,
    /// 回帰テストの説明（書き出したときは空）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

impl MeetingRecord {
    /// 外に出す形にする: 窓やタブの題を [`crate::debug_log::mask_text`] で伏せる。題は判定には使わないので、再生の結果は変わらない。
    pub fn redacted(&self) -> Self {
        let mut record = self.clone();
        for entry in &mut record.entries {
            for title in &mut entry.titles {
                title.title = crate::debug_log::mask_text(&title.title);
            }
        }
        record
    }
}

/// 出来事の名前。例: `started:googleMeet`・`ended:zoom`
pub fn event_name(event: MeetingEvent) -> String {
    let (kind, app) = match event {
        MeetingEvent::Started(app) => ("started", app),
        MeetingEvent::Ended(app) => ("ended", app),
    };
    let app = serde_json::to_value(app).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    format!("{kind}:{app}")
}

/// 印の立っている窓の題。
pub fn titles(evaluation: &Evaluation, facts: &Facts) -> Vec<RecordedTitle> {
    let mut titles: Vec<RecordedTitle> = Vec::new();
    for context in evaluation.services.iter().flat_map(|service| service.contexts.iter()) {
        let Some(hwnd) = context.window else { continue };
        let Some(window) = facts.windows.iter().find(|window| window.hwnd == hwnd) else { continue };
        if !titles.iter().any(|known| known.window == hwnd) {
            titles.push(RecordedTitle { app: window.exe.clone(), window: hwnd, title: window.title.clone() });
        }
    }
    titles
}

/// 会議の検出の記録（メモリーの中だけ）。
#[derive(Debug, Default)]
pub struct MeetingRecorder {
    entries: VecDeque<RecordEntry>,
}

impl MeetingRecorder {
    /// 1 回分を記録する。前の記録と同じ様子で、出来事もなければ、新しくは残さず、前の記録の `until` を延ばす。
    /// `verbose`（詳しい記録）のときだけ、題も残す。
    pub fn record(&mut self, at: i64, evaluation: &Evaluation, facts: &Facts, decisions: &[Decision], verbose: bool) {
        let titles = if verbose { titles(evaluation, facts) } else { Vec::new() };
        if decisions.is_empty() {
            if let Some(last) = self.entries.back_mut().filter(|last| last.services == evaluation.services && last.titles == titles) {
                last.until = Some(at);
                last.count += 1;
                return;
            }
        }
        if self.entries.len() == CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back(RecordEntry {
            at,
            until: None,
            count: 1,
            services: evaluation.services.clone(),
            titles,
            decisions: decisions
                .iter()
                .map(|decision| RecordedDecision { event: event_name(decision.event), reason: decision.reason.describe() })
                .collect(),
        });
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 書き出す中身。
    pub fn export(&self, app_version: &str) -> MeetingRecord {
        MeetingRecord {
            format: FORMAT.to_string(),
            app_version: app_version.to_string(),
            rules_version: Rules::builtin().version,
            entries: self.entries.iter().cloned().collect(),
            expected: Vec::new(),
            description: String::new(),
        }
    }
}

/// 記録を、製品と同じ判定（組み込みの判定表の時間と回数）で再生し、確定した出来事を順に返す。
/// 判定を進めるのは、実際に調べた時刻だけ: 記録 1 件の `at` から `until` までの `count` 回を、その間に等しい間隔で並べる。
/// 調べていない時刻（記録のあいだと、最後の記録のあと）には進めない（調べていない時刻の様子で、開始・終了を決めない）。
/// 形式が [`FORMAT_V2`] の記録は、その形式の再生のしかたで再生する。
pub fn replay(record: &MeetingRecord) -> Vec<String> {
    if record.format == FORMAT_V2 {
        return replay_v2(record);
    }
    let mut judge = Judge::new(Rules::builtin());
    let mut events = Vec::new();
    for entry in &record.entries {
        let evaluation = Evaluation { services: entry.services.clone() };
        for now in observed_times(entry) {
            events.extend(judge.tick(&evaluation, now).into_iter().map(|decision| event_name(decision.event)));
        }
    }
    events
}

/// 記録 1 件の、調べた時刻。
fn observed_times(entry: &RecordEntry) -> Vec<i64> {
    let until = entry.until.unwrap_or(entry.at).max(entry.at);
    let count = i64::from(entry.count.max(1));
    if count == 1 {
        return vec![entry.at];
    }
    (0..count).map(|i| entry.at + (until - entry.at) * i / (count - 1)).collect()
}

/// [`FORMAT_V2`] の記録の再生。
fn replay_v2(record: &MeetingRecord) -> Vec<String> {
    let Some(last) = record.entries.last() else {
        return Vec::new();
    };
    let mut judge = Judge::new(Rules::builtin());
    let end = last.until.unwrap_or(last.at).max(last.at) + judge.config.end_after_ms + POLL_MS * 2;
    let mut events = Vec::new();
    for (index, entry) in record.entries.iter().enumerate() {
        let next = record.entries.get(index + 1).map(|next| next.at).unwrap_or(end + 1);
        let observed = Evaluation { services: entry.services.clone() };
        let unobserved = Evaluation { services: without_scans(&entry.services) };
        let until = entry.until.unwrap_or(entry.at);
        let mut now = entry.at;
        loop {
            let current = if now <= until { &observed } else { &unobserved };
            for decision in judge.tick(current, now) {
                events.push(event_name(decision.event));
            }
            now += POLL_MS;
            if now >= next || now > end {
                break;
            }
        }
    }
    events
}

/// 同じ印のまま、窓の中は調べなかった回の様子。
fn without_scans(services: &[ServiceEvaluation]) -> Vec<ServiceEvaluation> {
    let mut services = services.to_vec();
    for context in services.iter_mut().flat_map(|service| service.contexts.iter_mut()) {
        context.scanned = false;
    }
    services
}

/// 形式を問わず読んだ記録を再生した結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replayed {
    pub format: String,
    pub description: String,
    pub expected: Vec<String>,
    pub events: Vec<String>,
}

/// 書き出した記録（JSON）を、まず形式を読み、その形式の判定と再生のしかたで再生する。
/// - [`FORMAT`]・[`FORMAT_V2`]: 判定表の当てはめの結果を、いまの判定で再生する（[`replay`]）
/// - [`crate::meeting_record_v1::FORMAT`]: マイクの使用と窓の題を、その記録を書いた版の判定で再生する
pub fn replay_json(text: &str) -> Result<Replayed, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|err| format!("JSON として読めません: {err}"))?;
    let format = value.get("format").and_then(|format| format.as_str()).ok_or("format がありません")?.to_string();
    if format == FORMAT || format == FORMAT_V2 {
        let record: MeetingRecord = serde_json::from_value(value).map_err(|err| format!("{format} として読めません: {err}"))?;
        let events = replay(&record);
        return Ok(Replayed { format, description: record.description, expected: record.expected, events });
    }
    if format == crate::meeting_record_v1::FORMAT {
        let record: crate::meeting_record_v1::Record =
            serde_json::from_value(value).map_err(|err| format!("{format} として読めません: {err}"))?;
        let events = crate::meeting_record_v1::replay(&record).into_iter().map(event_name).collect();
        return Ok(Replayed { format, description: record.description, expected: record.expected, events });
    }
    Err(format!("知らない形式です: {format}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meeting_facts::WindowFact;
    use crate::meeting_rules::{Context, Held, Kind, Strength};
    use crate::meetings::MeetingApp;

    fn context(app: &str, window: Option<u64>, signals: &[(&str, Strength, Kind)]) -> Context {
        Context {
            app: app.into(),
            window,
            scanned: true,
            signals: signals
                .iter()
                .map(|(name, strength, kind)| Held { name: (*name).into(), strength: *strength, kind: *kind, after_start: false })
                .collect(),
        }
    }

    fn evaluation(services: Vec<(MeetingApp, Vec<Context>)>) -> Evaluation {
        Evaluation { services: services.into_iter().map(|(app, contexts)| ServiceEvaluation { app, contexts }).collect() }
    }

    fn meet_tab() -> Evaluation {
        evaluation(vec![(
            MeetingApp::GoogleMeet,
            vec![context("chrome.exe", Some(42), &[("退出ボタン", Strength::Strong, Kind::Control), ("Meet のタブの題", Strength::Medium, Kind::Window)])],
        )])
    }

    fn facts_with_title(title: &str) -> Facts {
        Facts {
            windows: vec![WindowFact {
                hwnd: 42,
                exe: "chrome.exe".into(),
                class: "Chrome_WidgetWin_1".into(),
                title: title.into(),
                visible: true,
                site: Some("meet.google.com".into()),
            }],
            ..Default::default()
        }
    }

    /// 記録には、文脈（実行ファイルの名前と窓の識別子）と印の名前・強さを残す。窓の題は、詳しい記録のときだけ残す。
    // [MTG-025]
    #[test]
    fn the_record_keeps_signals_and_titles_only_when_verbose() {
        let facts = facts_with_title("Meet - abc-defg-hij - 山田 太郎 - Google Chrome");
        let mut recorder = MeetingRecorder::default();
        recorder.record(0, &meet_tab(), &facts, &[], false);
        let record = recorder.export("0.4.4");
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("山田") && !json.contains("abc-defg-hij"), "{json}");
        assert!(json.contains("退出ボタン") && json.contains("\"window\":42") && json.contains("\"app\":\"chrome.exe\""), "{json}");
        assert_eq!(record.format, FORMAT);
        assert_eq!(record.rules_version, Rules::builtin().version);

        let mut verbose = MeetingRecorder::default();
        verbose.record(0, &meet_tab(), &facts, &[], true);
        let record = verbose.export("0.4.4");
        assert_eq!(record.entries[0].titles, vec![RecordedTitle { app: "chrome.exe".into(), window: 42, title: facts.windows[0].title.clone() }]);
    }

    /// 様子が変わったときと、出来事があったときだけ残し、古いものから捨てる。出来事には理由を添える。
    // [MTG-025]
    #[test]
    fn only_changes_and_decisions_are_kept_up_to_the_capacity() {
        let mut recorder = MeetingRecorder::default();
        let facts = Facts::default();
        let mut judge = Judge::new(Rules::builtin());
        recorder.record(0, &Evaluation::default(), &facts, &[], false);
        recorder.record(2_000, &Evaluation::default(), &facts, &[], false);
        let decisions = judge.tick(&meet_tab(), 4_000);
        recorder.record(4_000, &meet_tab(), &facts, &decisions, false);
        recorder.record(6_000, &meet_tab(), &facts, &[], false);
        let record = recorder.export("0.4.4");
        assert_eq!(record.entries.iter().map(|e| e.at).collect::<Vec<_>>(), vec![0, 4_000]);
        assert_eq!(record.entries[1].decisions[0].event, "started:googleMeet");
        assert_eq!(record.entries[1].decisions[0].reason, "強い印（chrome.exe の窓）: 退出ボタン");

        for i in 0..(CAPACITY as i64 + 10) {
            let flip = if i % 2 == 0 { meet_tab() } else { Evaluation::default() };
            recorder.record(10_000 + i * 2_000, &flip, &facts, &[], false);
        }
        assert_eq!(recorder.len(), CAPACITY);
    }

    /// 書き出した記録を読み直して再生すると、製品と同じ判定で、同じ出来事が起きる。
    /// 最後の記録のあとは進めないので、終了は、終了の猶予がたつまで調べ続けた記録でだけ起きる。
    // [MTG-025]
    #[test]
    fn a_record_replays_with_the_product_timing() {
        let mut recorder = MeetingRecorder::default();
        let facts = Facts::default();
        recorder.record(0, &Evaluation::default(), &facts, &[], false);
        recorder.record(10_000, &meet_tab(), &facts, &[], false);
        recorder.record(60_000, &Evaluation::default(), &facts, &[], false); // 会議を抜けた
        let record = recorder.export("0.4.4");
        assert_eq!(replay(&record), vec!["started:googleMeet"], "抜けたあと 1 回しか調べていない");
        for at in (62_000..=80_000).step_by(2_000) {
            recorder.record(at, &Evaluation::default(), &facts, &[], false);
        }
        let record = recorder.export("0.4.4");
        assert_eq!((record.entries[2].until, record.entries[2].count), (Some(80_000), 11));
        let json = serde_json::to_string_pretty(&record).unwrap();
        let read: MeetingRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(replay(&read), vec!["started:googleMeet", "ended:googleMeet"]);
        assert_eq!(replay_json(&json).unwrap().events, vec!["started:googleMeet", "ended:googleMeet"]);
    }

    /// 再生では、調べた時刻だけで判定する。0 秒と 2.3 秒に組み合わせを調べ、4.6 秒に印がなければ、製品と同じく始めない。
    // [MTG-007][MTG-025]
    #[test]
    fn replay_judges_only_at_the_observed_times() {
        let entry = |at, until, count, services: Evaluation| RecordEntry { at, until, count, services: services.services, titles: vec![], decisions: vec![] };
        let record = MeetingRecord {
            entries: vec![entry(0, Some(2_300), 2, meet_combination()), entry(4_600, None, 1, Evaluation::default())],
            ..MeetingRecorder::default().export("0.4.4")
        };
        assert!(replay(&record).is_empty());
        assert_eq!(observed_times(&record.entries[0]), vec![0, 2_300]);
        // 同じ様子を 3 回（0・2.3・4.6 秒）調べていれば、4.6 秒で始める
        let longer = MeetingRecord { entries: vec![entry(0, Some(4_600), 3, meet_combination())], ..record };
        assert_eq!(replay(&longer), vec!["started:googleMeet"]);
    }

    /// 1 つ前の形式（/2）の記録は、形式を先に読み、その形式の再生のしかた（記録のあいだは調べずに同じ印が続き、
    /// 最後の記録のあとも終了の猶予がたつまで進める）で再生する。知らない形式は、理由を返す。
    // [MTG-025]
    #[test]
    fn older_formats_replay_in_their_own_way() {
        let entry = |at, until| RecordEntry { at, until, count: 1, services: meet_tab().services, titles: vec![], decisions: vec![] };
        let v2 = MeetingRecord {
            format: FORMAT_V2.into(),
            entries: vec![entry(0, Some(10_000)), RecordEntry { services: vec![], ..entry(12_000, None) }],
            ..MeetingRecorder::default().export("0.4.4")
        };
        let json = serde_json::to_string(&v2).unwrap();
        let replayed = replay_json(&json).unwrap();
        assert_eq!(replayed.format, FORMAT_V2);
        assert_eq!(replayed.events, vec!["started:googleMeet", "ended:googleMeet"]);
        assert!(replay_json(r#"{"format":"kiroscribe-meeting-record/9","entries":[]}"#).unwrap_err().contains("知らない形式"));
        assert!(replay_json(r#"{"entries":[]}"#).unwrap_err().contains("format"));
    }

    fn meet_combination() -> Evaluation {
        evaluation(vec![(
            MeetingApp::GoogleMeet,
            vec![context("chrome.exe", Some(42), &[("Meet のタブの題", Strength::Medium, Kind::Window), ("ブラウザのマイク", Strength::Weak, Kind::Session)])],
        )])
    }

    /// 再生では、記録のない時刻で、窓の中を調べた回数を増やさない。製品で始めなかった組み合わせを、再生で始めない。
    // [MTG-007][MTG-025]
    #[test]
    fn replay_counts_only_the_recorded_scans() {
        // 0 秒に組み合わせを 1 回だけ調べ、次に調べた 5 秒には印がない（製品では始めない）
        let entry = |at, until: Option<i64>, services: Evaluation| RecordEntry {
            at,
            until,
            count: until.map_or(1, |until| ((until - at) / POLL_MS + 1) as u32),
            services: services.services,
            titles: vec![],
            decisions: vec![],
        };
        let once = MeetingRecord {
            entries: vec![entry(0, None, meet_combination()), entry(5_000, None, Evaluation::default())],
            ..MeetingRecorder::default().export("0.4.4")
        };
        assert!(replay(&once).is_empty());
        // 同じ組み合わせを 2 秒ごとに 6 秒まで調べていれば、4 秒で始める
        let kept = MeetingRecord {
            entries: vec![entry(0, Some(6_000), meet_combination()), entry(8_000, None, Evaluation::default())],
            ..once
        };
        assert_eq!(replay(&kept), vec!["started:googleMeet"]);

        // 記録するときは、同じ様子が続いた最後の時刻を残す
        let mut recorder = MeetingRecorder::default();
        for at in [0, 2_000, 4_000] {
            recorder.record(at, &meet_combination(), &Facts::default(), &[], false);
        }
        let record = recorder.export("0.4.4");
        assert_eq!((record.entries.len(), record.entries[0].until), (1, Some(4_000)));
    }

    /// 書き出す形では、題の秘密になりうる部分を伏せる。題は判定に使わないので、再生の結果は変わらない。
    // [MTG-026]
    #[test]
    fn a_redacted_record_hides_secrets_and_replays_the_same() {
        let mut recorder = MeetingRecorder::default();
        let secret = facts_with_title(r"https://meet.google.com/abc-defg-hij?authuser=SECRETQUERY C:\Users\山田 太郎\x sk-proj-SECRETKEY123 - Google Chrome");
        recorder.record(0, &Evaluation::default(), &Facts::default(), &[], true);
        recorder.record(10_000, &meet_tab(), &secret, &[], true);
        for at in (40_000..=62_000).step_by(2_000) {
            recorder.record(at, &Evaluation::default(), &Facts::default(), &[], true);
        }
        let record = recorder.export("0.4.4");
        let redacted = record.redacted();
        let json = serde_json::to_string(&redacted).unwrap();
        // 失敗したときの表示には、伏せるべき値を出さず、何が残っていたかの名前だけを出す
        for (label, secret) in [("URL のクエリ", "SECRETQUERY"), ("API キー", "SECRETKEY"), ("利用者の名前", "山田")] {
            assert!(!json.contains(secret), "書き出す形に{label}が残っている");
        }
        // 元の記録（メモリーの中）は変えない
        assert!(serde_json::to_string(&record).unwrap().contains("SECRETQUERY"));
        let expected = vec!["started:googleMeet", "ended:googleMeet"];
        assert_eq!(replay(&record), expected);
        assert_eq!(replay(&redacted), expected);
    }
}
