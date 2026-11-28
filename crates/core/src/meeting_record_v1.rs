//! 旧い形式（`kiroscribe-meeting-record/1`）の会議の検出の記録と、その記録を書いた版の判定。
//!
//! この形式は、マイクを使っているアプリ（Windows の ConsentStore）と、マイクを使っているブラウザの窓の題を残していた。
//! 判定表の印（窓の中の部品・オーディオのセッション）を含まないので、いまの判定では再生できない。
//! そこで、記録を書いた版と同じ判定（デスクトップの会議アプリのマイクの使用か、マイクを使っているブラウザの窓の題）で再生する。
//! 新しく書く記録には使わない（[`crate::meeting_record::FORMAT`]）。

use serde::Deserialize;

use crate::meetings::{exe_name, MeetingApp, MeetingEvent};

/// この形式の名前。
pub const FORMAT: &str = "kiroscribe-meeting-record/1";
/// この形式の記録を書いた版が、検出を調べた間隔（ミリ秒）。
const POLL_MS: i64 = 2_000;
/// 開始の確定までの時間と、終了の猶予（ミリ秒）。
const START_CONFIRM_MS: i64 = 4_000;
const END_GRACE_MS: i64 = 20_000;

/// この形式の記録を書いた版が、マイクを使っていると窓の題から会議を探したブラウザ。
const BROWSERS: &[&str] = &["chrome.exe", "msedge.exe", "firefox.exe", "brave.exe", "vivaldi.exe", "opera.exe", "arc.exe"];

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MicUser {
    /// 実行ファイルの名前か、ストアアプリのパッケージ名
    app: String,
    in_use: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Window {
    exe: String,
    title: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Observation {
    #[serde(default)]
    mic_users: Vec<MicUser>,
    #[serde(default)]
    windows: Vec<Window>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    at: i64,
    observation: Observation,
}

/// この形式の記録の中身。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    /// 自分自身（Kiroscribe）の実行ファイルの名前。録音中の自分は、判定に入れない
    #[serde(default)]
    own_exe: Option<String>,
    entries: Vec<Entry>,
    #[serde(default)]
    pub expected: Vec<String>,
    #[serde(default)]
    pub description: String,
}

/// パッケージ名（小文字）の先頭と、その会議アプリ。
const PACKAGE_PREFIXES: &[(&str, MeetingApp)] = &[
    ("91750d7e.slack_", MeetingApp::SlackHuddle),
    ("com.tinyspeck.slackdesktop_", MeetingApp::SlackHuddle),
    ("msteams_", MeetingApp::Teams),
    ("microsoftteams_", MeetingApp::Teams),
];

/// マイクを使ったアプリが、デスクトップの会議アプリなら、そのアプリ。
fn desktop_app(id: &str) -> Option<MeetingApp> {
    let lower = id.to_lowercase();
    if !lower.contains(['\\', '/']) && !lower.ends_with(".exe") {
        let packaged = PACKAGE_PREFIXES.iter().find_map(|(prefix, app)| {
            let rest = lower.strip_prefix(prefix)?;
            (!rest.is_empty()).then_some(*app)
        });
        if packaged.is_some() {
            return packaged;
        }
    }
    match exe_name(&lower).as_str() {
        "slack.exe" => Some(MeetingApp::SlackHuddle),
        "zoom.exe" | "cpthost.exe" => Some(MeetingApp::Zoom),
        "ms-teams.exe" | "ms-teams_modulehost.exe" | "teams.exe" => Some(MeetingApp::Teams),
        "discord.exe" | "discordptb.exe" | "discordcanary.exe" | "discorddevelopment.exe" => Some(MeetingApp::Discord),
        _ => None,
    }
}

/// ブラウザの窓の題（表示中のタブ）から会議のサービスを決める。
fn web_meeting(title: &str) -> Option<MeetingApp> {
    let t = title.to_lowercase();
    let is_meet = t.contains("google meet")
        || t.starts_with("meet - ")
        || t.starts_with("meet – ")
        || t.starts_with("meet: ")
        || t.starts_with("meet：")
        || t.contains(" - meet - ")
        || t.contains("meet.google.com");
    if is_meet {
        return Some(MeetingApp::GoogleMeet);
    }
    if t.contains("slack") && (t.contains("huddle") || t.contains("ハドル")) {
        return Some(MeetingApp::SlackHuddle);
    }
    if t.contains("microsoft teams") && (t.contains("meeting") || t.contains("会議") || t.contains("call") || t.contains("通話")) {
        return Some(MeetingApp::Teams);
    }
    if t.contains("zoom") && (t.contains("meeting") || t.contains("ミーティング")) {
        return Some(MeetingApp::Zoom);
    }
    is_discord_tab(&t).then_some(MeetingApp::Discord)
}

/// ブラウザ版 Discord の画面のタブ名か（小文字にした題）。
fn is_discord_tab(lower_title: &str) -> bool {
    let mut tab = without_browser_name(lower_title.trim());
    loop {
        if let Some(after) = tab.strip_prefix('•') {
            tab = after.trim_start();
        } else if let Some(after) = without_unread_count(tab) {
            tab = after.trim_start();
        } else {
            break;
        }
    }
    tab.starts_with("discord | ") || tab.ends_with(" | discord") || tab.ends_with(" - discord")
}

/// 窓の題の末尾に付くブラウザの名前（小文字）。
const BROWSER_TITLE_SUFFIXES: &[&str] =
    &["google chrome", "microsoft edge", "microsoft\u{200b} edge", "mozilla firefox", "brave", "vivaldi", "opera"];

fn without_browser_name(lower_title: &str) -> &str {
    let last = [" - ", " — ", " – "]
        .iter()
        .filter_map(|separator| lower_title.rfind(separator).map(|at| (at, separator.len())))
        .max();
    match last {
        Some((at, len)) if BROWSER_TITLE_SUFFIXES.contains(&lower_title[at + len..].trim()) => lower_title[..at].trim_end(),
        _ => lower_title,
    }
}

fn without_unread_count(title: &str) -> Option<&str> {
    let inner = title.strip_prefix('(')?;
    let close = inner.find(')')?;
    let count = &inner[..close];
    (!count.is_empty() && count.chars().all(|c| c.is_ascii_digit() || c == '+')).then(|| &inner[close + 1..])
}

/// 見つけた会議。ブラウザの会議なら、そのブラウザ（続いているかは、このブラウザがマイクを使っているかで決める）。
#[derive(Debug, Clone, PartialEq)]
struct Detection {
    app: MeetingApp,
    browser: Option<String>,
}

/// 自分自身を除いた、いまマイクを使っているアプリ。
fn active<'a>(observation: &'a Observation, own_exe: Option<&str>) -> Vec<&'a MicUser> {
    let own = own_exe.map(exe_name);
    observation.mic_users.iter().filter(|user| user.in_use && Some(exe_name(&user.app)) != own).collect()
}

fn browsers_in_use(active: &[&MicUser]) -> Vec<String> {
    active.iter().map(|user| exe_name(&user.app)).filter(|exe| BROWSERS.contains(&exe.as_str())).collect()
}

fn detect(observation: &Observation, own_exe: Option<&str>) -> Option<Detection> {
    let active = active(observation, own_exe);
    if let Some(app) = active.iter().find_map(|user| desktop_app(&user.app)) {
        return Some(Detection { app, browser: None });
    }
    let browsers = browsers_in_use(&active);
    observation
        .windows
        .iter()
        .filter(|window| browsers.contains(&window.exe.to_lowercase()))
        .find_map(|window| web_meeting(&window.title).map(|app| Detection { app, browser: Some(window.exe.to_lowercase()) }))
}

fn still_active(observation: &Observation, meeting: &Detection, own_exe: Option<&str>) -> bool {
    let active = active(observation, own_exe);
    match &meeting.browser {
        Some(browser) => browsers_in_use(&active).iter().any(|exe| exe == browser),
        None => active.iter().any(|user| desktop_app(&user.app) == Some(meeting.app)),
    }
}

/// この形式の記録を書いた版の、開始・終了の確定。
#[derive(Default)]
struct Tracker {
    candidate: Option<(Detection, i64)>,
    active: Option<Detection>,
    last_seen_ms: i64,
}

impl Tracker {
    fn tick(&mut self, observation: &Observation, own_exe: Option<&str>, now: i64) -> Option<MeetingEvent> {
        let detected = detect(observation, own_exe);
        if let Some(active) = self.active.clone() {
            let continuing = still_active(observation, &active, own_exe);
            if continuing || detected.as_ref().is_some_and(|detection| detection.app == active.app) {
                self.last_seen_ms = now;
                self.candidate = None;
                return None;
            }
            match (detected, &self.candidate) {
                (Some(detection), Some((candidate, since))) if candidate.app == detection.app => {
                    if now - since >= START_CONFIRM_MS {
                        self.active = None;
                        return Some(MeetingEvent::Ended(active.app));
                    }
                }
                (Some(detection), _) => self.candidate = Some((detection, now)),
                (None, _) => self.candidate = None,
            }
            if now - self.last_seen_ms >= END_GRACE_MS {
                self.active = None;
                return Some(MeetingEvent::Ended(active.app));
            }
            return None;
        }
        match detected {
            Some(detection) => match &self.candidate {
                Some((candidate, since)) if candidate.app == detection.app => {
                    if now - since >= START_CONFIRM_MS {
                        self.active = Some(detection.clone());
                        self.last_seen_ms = now;
                        self.candidate = None;
                        return Some(MeetingEvent::Started(detection.app));
                    }
                    None
                }
                _ => {
                    self.candidate = Some((detection, now));
                    None
                }
            },
            None => {
                self.candidate = None;
                None
            }
        }
    }
}

/// この形式の記録を、記録を書いた版と同じ間隔・同じ判定で再生する（記録のあいだは、直前の記録の様子が続いていたものとする。
/// 最後の記録のあとも、終了の猶予がたつまで同じ様子のまま進める）。
pub fn replay(record: &Record) -> Vec<MeetingEvent> {
    let (Some(first), Some(last)) = (record.entries.first(), record.entries.last()) else {
        return Vec::new();
    };
    let own_exe = record.own_exe.as_deref();
    let end = last.at + END_GRACE_MS + POLL_MS * 2;
    let mut tracker = Tracker::default();
    let mut events = Vec::new();
    let mut index = 0;
    let mut now = first.at;
    while now <= end {
        while index + 1 < record.entries.len() && record.entries[index + 1].at <= now {
            index += 1;
        }
        events.extend(tracker.tick(&record.entries[index].observation, own_exe, now));
        now += POLL_MS;
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(json: serde_json::Value) -> Record {
        serde_json::from_value(json).unwrap()
    }

    /// 旧い形式の記録は、記録を書いた版の判定（マイクを使っているブラウザの、会議のタブの題）で再生する。
    // [MTG-025]
    #[test]
    fn a_format_1_record_replays_with_the_judgment_of_its_version() {
        let meet = record(serde_json::json!({
            "ownExe": "kiroscribe.exe",
            "entries": [
                { "at": 0, "observation": { "micUsers": [{ "app": "msedge.exe", "inUse": true }],
                    "windows": [{ "exe": "msedge.exe", "title": "Meet - abc-defg-hij - Microsoft Edge" }] } },
                { "at": 60_000, "observation": { "micUsers": [{ "app": "msedge.exe", "inUse": false }], "windows": [] } }
            ]
        }));
        assert_eq!(replay(&meet), vec![MeetingEvent::Started(MeetingApp::GoogleMeet), MeetingEvent::Ended(MeetingApp::GoogleMeet)]);
        let own = record(serde_json::json!({
            "ownExe": "kiroscribe.exe",
            "entries": [{ "at": 0, "observation": { "micUsers": [{ "app": "kiroscribe.exe", "inUse": true }] } }]
        }));
        assert!(replay(&own).is_empty(), "録音中の自分は判定に入れない");
    }
}
