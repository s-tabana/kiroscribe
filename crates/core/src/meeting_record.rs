//! 会議の検出の記録。利用者が書き出して、検出がうまくいかなかったときに調べる材料と、回帰テストの材料にする。
//!
//! 調べるたびの様子（マイクを使っているアプリと、ウィンドウのタイトル）のうち、検出に使うものだけを、
//! 様子が変わったときと、会議の開始・終了を確定したときだけ残す。個人の情報を残しすぎないよう:
//! - マイクを使ったアプリは、実行ファイルの名前（利用者の名前を含むフルパスは残さない）か、ストアアプリのパッケージ名だけ
//! - ウィンドウは、いまマイクを使っているブラウザのものだけ（検出に使うのはそれだけ）。ほかのウィンドウのタイトルは残さない
//! - 記録はメモリーにだけ置き（新しいものから [`CAPACITY`] 件）、利用者が書き出すまでディスクには書かない
//!
//! 書き出したファイル（[`MeetingRecord`]）は、[`replay`] で製品と同じ間隔・同じ判定のまま再生できる。
//! `expected` に起きるはずの出来事を書き足して `crates/core/tests/fixtures/meetings/` に置くと、回帰テストになる。

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::meetings::{exe_name, BROWSERS, MeetingEvent, MeetingTracker, MicUser, Observation, WindowInfo};

/// 書き出すファイルの形式の名前。形を変えたら、末尾の番号を上げる。
pub const FORMAT: &str = "kiroscribe-meeting-record/1";
/// メモリーに残す件数（古いものから捨てる）。
pub const CAPACITY: usize = 300;
/// 製品が検出を調べる間隔（ミリ秒）。再生もこの間隔で行う。
pub const POLL_MS: i64 = 2_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedMicUser {
    /// 実行ファイルの名前（小文字）か、ストアアプリのパッケージ名
    pub app: String,
    pub in_use: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedWindow {
    pub exe: String,
    pub title: String,
}

/// 記録に残す、1 回分の様子（個人の情報を減らしたもの）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedObservation {
    pub mic_users: Vec<RecordedMicUser>,
    pub windows: Vec<RecordedWindow>,
}

impl RecordedObservation {
    /// 調べた様子から、検出に使うものだけを残す。
    pub fn from_observation(observation: &Observation) -> Self {
        let app_name = |id: &str| {
            if id.contains(['\\', '/']) {
                exe_name(id)
            } else {
                id.to_string()
            }
        };
        let browsers_in_use: Vec<String> = observation
            .mic_users
            .iter()
            .filter(|u| u.in_use)
            .map(|u| exe_name(&u.id))
            .filter(|exe| BROWSERS.contains(&exe.as_str()))
            .collect();
        let mut mic_users: Vec<RecordedMicUser> = observation
            .mic_users
            .iter()
            .map(|u| RecordedMicUser {
                app: app_name(&u.id),
                in_use: u.in_use,
            })
            .collect();
        mic_users.sort_by(|a, b| (&a.app, a.in_use).cmp(&(&b.app, b.in_use)));
        mic_users.dedup();
        let windows = observation
            .windows
            .iter()
            .filter(|w| browsers_in_use.contains(&w.exe.to_lowercase()))
            .map(|w| RecordedWindow {
                exe: w.exe.to_lowercase(),
                title: w.title.clone(),
            })
            .collect();
        Self { mic_users, windows }
    }

    /// 再生のために、判定が読む形に戻す。
    pub fn to_observation(&self) -> Observation {
        Observation {
            mic_users: self
                .mic_users
                .iter()
                .map(|u| MicUser {
                    id: u.app.clone(),
                    in_use: u.in_use,
                })
                .collect(),
            windows: self
                .windows
                .iter()
                .map(|w| WindowInfo {
                    exe: w.exe.clone(),
                    title: w.title.clone(),
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordEntry {
    /// 調べた時刻（UNIX ミリ秒）
    pub at: i64,
    pub observation: RecordedObservation,
    /// そのときに確定した出来事（[`event_name`] の形）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
}

/// 書き出すファイルの中身。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeetingRecord {
    pub format: String,
    pub app_version: String,
    /// 自分自身（Kiroscribe）の実行ファイルの名前。録音中の自分は、判定に入れない
    pub own_exe: Option<String>,
    pub entries: Vec<RecordEntry>,
    /// 回帰テストで、再生したときに起きるはずの出来事（[`event_name`] の形）。書き出したときは空
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected: Vec<String>,
    /// 回帰テストの説明（書き出したときは空）
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// 出来事の名前。例: `started:googleMeet`・`ended:zoom`
pub fn event_name(event: MeetingEvent) -> String {
    let (kind, app) = match event {
        MeetingEvent::Started(app) => ("started", app),
        MeetingEvent::Ended(app) => ("ended", app),
    };
    let app = serde_json::to_value(app)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    format!("{kind}:{app}")
}

/// 会議の検出の記録（メモリーの中だけ）。
#[derive(Debug, Default)]
pub struct MeetingRecorder {
    entries: VecDeque<RecordEntry>,
}

impl MeetingRecorder {
    /// 1 回分の様子を記録する。前の記録と同じ様子で、出来事もなければ残さない。
    pub fn record(&mut self, at: i64, observation: &Observation, event: Option<MeetingEvent>) {
        let recorded = RecordedObservation::from_observation(observation);
        let unchanged = self.entries.back().is_some_and(|last| last.observation == recorded);
        if unchanged && event.is_none() {
            return;
        }
        if self.entries.len() == CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back(RecordEntry {
            at,
            observation: recorded,
            event: event.map(event_name),
        });
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 書き出す中身。`own_exe` は自分自身の実行ファイルのパス（名前だけを残す）。
    pub fn export(&self, app_version: &str, own_exe: Option<&str>) -> MeetingRecord {
        MeetingRecord {
            format: FORMAT.to_string(),
            app_version: app_version.to_string(),
            own_exe: own_exe.map(exe_name),
            entries: self.entries.iter().cloned().collect(),
            expected: Vec::new(),
            description: String::new(),
        }
    }
}

/// 記録を、製品と同じ間隔（[`POLL_MS`]）・同じ判定（[`MeetingTracker`] の既定の待ち時間）で再生し、
/// 確定した出来事を順に返す。記録のあいだの時刻は、直前の記録の様子が続いていたものとして扱う。
/// 最後の記録のあとも、会議の終わりの猶予がたつまで同じ様子のまま進める。
pub fn replay(record: &MeetingRecord) -> Vec<String> {
    let Some(first) = record.entries.first() else {
        return Vec::new();
    };
    let mut tracker = MeetingTracker::default();
    let end = record.entries.last().map(|e| e.at).unwrap_or(first.at) + tracker.end_grace_ms + POLL_MS * 2;
    let own_exe = record.own_exe.as_deref();
    let mut events = Vec::new();
    let mut index = 0;
    let mut current = first.observation.to_observation();
    let mut now = first.at;
    while now <= end {
        while index + 1 < record.entries.len() && record.entries[index + 1].at <= now {
            index += 1;
            current = record.entries[index].observation.to_observation();
        }
        if let Some(event) = tracker.observe(&current, own_exe, now) {
            events.push(event_name(event));
        }
        now += POLL_MS;
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meetings::MeetingApp;

    fn observation(mic: &[(&str, bool)], windows: &[(&str, &str)]) -> Observation {
        Observation {
            mic_users: mic
                .iter()
                .map(|(id, in_use)| MicUser {
                    id: (*id).into(),
                    in_use: *in_use,
                })
                .collect(),
            windows: windows
                .iter()
                .map(|(exe, title)| WindowInfo {
                    exe: (*exe).into(),
                    title: (*title).into(),
                })
                .collect(),
        }
    }

    /// 記録には、フルパス（利用者の名前を含む）や、検出に使わないウィンドウのタイトルを残さない。
    // [MTG-025]
    #[test]
    fn the_record_keeps_only_what_detection_uses() {
        let seen = observation(
            &[
                (r"C:\Users\山田 太郎\AppData\Local\Google\Chrome\Application\chrome.exe", true),
                (r"C:\Users\山田 太郎\AppData\Roaming\Zoom\bin\Zoom.exe", false),
                ("MSTeams_8wekyb3d8bbwe", false),
            ],
            &[
                ("chrome.exe", "Meet - abc-defg-hij - Google Chrome"),
                ("msedge.exe", "受信トレイ (3) - yamada@example.com - Outlook"),
                ("explorer.exe", "山田 太郎のドキュメント"),
            ],
        );
        let recorded = RecordedObservation::from_observation(&seen);
        let json = serde_json::to_string(&recorded).unwrap();
        assert!(!json.contains("山田"), "{json}");
        assert!(!json.contains("example.com"), "{json}");
        assert_eq!(
            recorded,
            RecordedObservation {
                mic_users: vec![
                    RecordedMicUser { app: "MSTeams_8wekyb3d8bbwe".into(), in_use: false },
                    RecordedMicUser { app: "chrome.exe".into(), in_use: true },
                    RecordedMicUser { app: "zoom.exe".into(), in_use: false },
                ],
                windows: vec![RecordedWindow {
                    exe: "chrome.exe".into(),
                    title: "Meet - abc-defg-hij - Google Chrome".into(),
                }],
            }
        );
    }

    /// 様子が変わったときと、出来事があったときだけ残し、古いものから捨てる。
    // [MTG-025]
    #[test]
    fn only_changes_and_events_are_kept_up_to_the_capacity() {
        let mut recorder = MeetingRecorder::default();
        let idle = observation(&[(r"C:\x\zoom.exe", false)], &[]);
        let busy = observation(&[(r"C:\x\zoom.exe", true)], &[]);
        recorder.record(0, &idle, None);
        recorder.record(2_000, &idle, None);
        recorder.record(4_000, &busy, None);
        recorder.record(8_000, &busy, Some(MeetingEvent::Started(MeetingApp::Zoom)));
        let record = recorder.export("0.4.0", Some(r"C:\Users\someone\AppData\Local\Kiroscribe\kiroscribe.exe"));
        assert_eq!(record.format, FORMAT);
        assert_eq!(record.own_exe.as_deref(), Some("kiroscribe.exe"));
        assert_eq!(record.entries.iter().map(|e| e.at).collect::<Vec<_>>(), vec![0, 4_000, 8_000]);
        assert_eq!(record.entries[2].event.as_deref(), Some("started:zoom"));

        for i in 0..(CAPACITY as i64 + 10) {
            let flip = observation(&[(r"C:\x\zoom.exe", i % 2 == 0)], &[]);
            recorder.record(10_000 + i * 2_000, &flip, None);
        }
        assert_eq!(recorder.len(), CAPACITY);
    }

    /// 書き出した記録を再生すると、製品と同じ判定で、同じ出来事が起きる。
    // [MTG-025]
    #[test]
    fn a_record_replays_with_the_product_timing() {
        let mut recorder = MeetingRecorder::default();
        let quiet = observation(&[(r"C:\b\chrome.exe", false)], &[("chrome.exe", "Meet - abc-defg-hij - Google Chrome")]);
        let meeting = observation(&[(r"C:\b\chrome.exe", true)], &[("chrome.exe", "Meet - abc-defg-hij - Google Chrome")]);
        let other_tab = observation(&[(r"C:\b\chrome.exe", true)], &[("chrome.exe", "資料 - Google ドキュメント - Google Chrome")]);
        recorder.record(0, &quiet, None);
        recorder.record(10_000, &meeting, None);
        recorder.record(30_000, &other_tab, None); // 会議中に別のタブへ（マイクは使ったまま）
        recorder.record(60_000, &quiet, None); // 会議を抜けた
        let record = recorder.export("0.4.0", None);
        let json = serde_json::to_string_pretty(&record).unwrap();
        let read: MeetingRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(replay(&read), vec!["started:googleMeet", "ended:googleMeet"]);
    }
}
