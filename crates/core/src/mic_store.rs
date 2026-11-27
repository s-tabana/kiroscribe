//! Windows のマイクの使用状況の記録（ConsentStore）の生の値と、それをデバッグ用のログに書く形。
//!
//! 会議を検出しなかったときに、次を見分けられるようにする:
//! - 記録そのものを開けなかったのか
//! - 会議アプリの記録はあるが、使用の終了の時刻（Stop）が 0 にならない（使用中にならない）のか、
//!   Stop がいつのものか（数秒前か、何日も前か）
//! - マイクを使ったプログラムの名前が、会議アプリの名前に当たらないのか
//!
//! ログには、実行ファイルの名前かパッケージ名と、置き場所の区分（Program Files・AppData など）だけを書く。
//! 利用者の名前を含むフルパスは書かない。

use serde::Serialize;

use crate::meetings::{app_name, desktop_app, exe_name, MeetingApp, BROWSERS};

/// 記録 1 件の生の値。時刻は Windows の FILETIME（1601-01-01 からの 100 ナノ秒単位）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicEntry {
    /// 実行ファイルのフルパス（NonPackaged）か、パッケージ名
    pub id: String,
    /// `LastUsedTimeStart`（ないときは 0）
    pub last_start: u64,
    /// `LastUsedTimeStop`（ないときは `None`）
    pub last_stop: Option<u64>,
}

impl MicEntry {
    /// 使用中か: 使用を始めていて、終了の時刻が 0 のまま。
    pub fn in_use(&self) -> bool {
        self.last_start > 0 && self.last_stop == Some(0)
    }
}

/// マイクの使用状況の記録を読んだ結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MicStore {
    /// 記録（レジストリのキー）を開けたか
    pub opened: bool,
    pub entries: Vec<MicEntry>,
}

/// 会議アプリに見える名前に含まれる語（当たらない名前も、ログで見られるようにする）。
const LOOKS_LIKE: &[&str] = &["slack", "teams", "zoom", "discord", "webex", "meet", "skype", "msedgewebview2"];

/// ログに書く、記録 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MicEntryReport {
    /// 実行ファイルの名前（小文字）か、パッケージ名
    pub name: String,
    /// 名前の分類: `exe`（実行ファイル）・`package`（パッケージ名）・`other`
    pub name_kind: &'static str,
    /// 置き場所の区分（[`location`]）
    pub location: &'static str,
    /// 会議アプリとして当たるなら、そのアプリ
    pub app: Option<MeetingApp>,
    /// ブラウザか
    pub browser: bool,
    pub in_use: bool,
    /// `LastUsedTimeStart` の値（FILETIME。JSON の数で表せない大きさなので文字列）
    pub start: String,
    /// `LastUsedTimeStop` の値（ないときは `None`）
    pub stop: Option<String>,
    /// 使用を始めてからの秒数（ログの行にだけ入れる）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since_start_sec: Option<i64>,
    /// 使用を終えてからの秒数（使用中・記録がないときは入れない）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since_stop_sec: Option<i64>,
}

/// ログに書く、記録の様子。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MicStoreReport {
    pub opened: bool,
    /// 記録の件数（会議アプリに見えないものも含む）
    pub total: usize,
    /// 会議アプリ・ブラウザに見える記録
    pub entries: Vec<MicEntryReport>,
}

/// 名前の分類。
pub fn name_kind(id: &str) -> &'static str {
    let lower = id.to_lowercase();
    if lower.contains(['\\', '/']) {
        if lower.ends_with(".exe") {
            "exe"
        } else {
            "other"
        }
    } else if lower.ends_with(".exe") {
        "exe"
    } else if lower.contains('_') {
        // パッケージファミリ名（<名前>_<発行元の ID>）
        "package"
    } else {
        "other"
    }
}

/// 置き場所の区分（利用者の名前を含まない）。
pub fn location(id: &str) -> &'static str {
    let lower = id.to_lowercase().replace('/', "\\");
    if !lower.contains('\\') {
        return if name_kind(id) == "package" { "package" } else { "unknown" };
    }
    if lower.contains("\\windowsapps\\") {
        "WindowsApps"
    } else if lower.contains("\\program files (x86)\\") {
        "ProgramFilesX86"
    } else if lower.contains("\\program files\\") {
        "ProgramFiles"
    } else if lower.contains("\\appdata\\local\\") {
        "AppDataLocal"
    } else if lower.contains("\\appdata\\roaming\\") {
        "AppDataRoaming"
    } else if lower.contains(":\\windows\\") {
        "Windows"
    } else if lower.contains("\\users\\") {
        "UserFolder"
    } else {
        "other"
    }
}

/// FILETIME（100 ナノ秒単位）を UNIX ミリ秒にする。
pub fn filetime_to_unix_ms(filetime: u64) -> i64 {
    (filetime / 10_000) as i64 - 11_644_473_600_000
}

fn age_sec(filetime: u64, now_ms: i64) -> Option<i64> {
    (filetime > 0).then(|| (now_ms - filetime_to_unix_ms(filetime)) / 1000)
}

fn looks_like_meeting(id: &str) -> bool {
    let name = exe_name(id);
    desktop_app(id).is_some() || BROWSERS.contains(&name.as_str()) || LOOKS_LIKE.iter().any(|word| name.contains(word))
}

/// 記録の様子をログに書く形にする。`now_ms` を渡すと、経過の秒数も入れる
/// （経過の秒数は調べるたびに変わるので、様子が変わったかを比べるときは `None` で作る）。
pub fn report(store: &MicStore, now_ms: Option<i64>) -> MicStoreReport {
    let mut entries: Vec<MicEntryReport> = store
        .entries
        .iter()
        .filter(|entry| looks_like_meeting(&entry.id))
        .map(|entry| MicEntryReport {
            name: app_name(&entry.id),
            name_kind: name_kind(&entry.id),
            location: location(&entry.id),
            app: desktop_app(&entry.id),
            browser: BROWSERS.contains(&exe_name(&entry.id).as_str()),
            in_use: entry.in_use(),
            start: entry.last_start.to_string(),
            stop: entry.last_stop.map(|stop| stop.to_string()),
            since_start_sec: now_ms.and_then(|now| age_sec(entry.last_start, now)),
            since_stop_sec: now_ms.and_then(|now| entry.last_stop.and_then(|stop| age_sec(stop, now))),
        })
        .collect();
    entries.sort_by(|a, b| (&a.name, &a.location).cmp(&(&b.name, &b.location)));
    MicStoreReport {
        opened: store.opened,
        total: store.entries.len(),
        entries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-27 10:00:00 UTC の FILETIME
    const NOW_FT: u64 = 134_402_472_000_000_000;

    fn now_ms() -> i64 {
        filetime_to_unix_ms(NOW_FT)
    }

    /// 会議アプリに見える記録ごとに、名前の分類・置き場所の区分・Stop の値と経過秒を書く。
    /// 利用者の名前を含むフルパスと、会議アプリに見えない記録は書かない。
    // [LOG-005]
    #[test]
    fn report_shows_meeting_like_entries_without_user_names() {
        let store = MicStore {
            opened: true,
            entries: vec![
                MicEntry {
                    id: r"C:\Users\山田 太郎\AppData\Local\slack\app-4.53.292\slack.exe".into(),
                    last_start: NOW_FT - 3 * 86_400 * 10_000_000,
                    last_stop: Some(NOW_FT - 2 * 86_400 * 10_000_000),
                },
                MicEntry {
                    id: "com.tinyspeck.slackdesktop_8yrtsj140pw4g".into(),
                    last_start: NOW_FT - 30 * 10_000_000,
                    last_stop: Some(0),
                },
                MicEntry {
                    id: r"C:\Program Files (x86)\Microsoft\EdgeWebView\Application\msedgewebview2.exe".into(),
                    last_start: NOW_FT - 10 * 10_000_000,
                    last_stop: None,
                },
                MicEntry {
                    id: r"C:\Users\山田 太郎\Apps\Audacity\Audacity.exe".into(),
                    last_start: NOW_FT,
                    last_stop: Some(0),
                },
            ],
        };
        let report = report(&store, Some(now_ms()));
        assert!(report.opened);
        assert_eq!(report.total, 4);
        let names: Vec<&str> = report.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["com.tinyspeck.slackdesktop_8yrtsj140pw4g", "msedgewebview2.exe", "slack.exe"]);

        let slack = &report.entries[2];
        assert_eq!((slack.name_kind, slack.location), ("exe", "AppDataLocal"));
        assert_eq!(slack.app, Some(MeetingApp::SlackHuddle));
        assert!(!slack.in_use);
        assert_eq!(slack.stop.as_deref(), Some((NOW_FT - 2 * 86_400 * 10_000_000).to_string().as_str()));
        assert_eq!(slack.since_stop_sec, Some(2 * 86_400));
        assert_eq!(slack.since_start_sec, Some(3 * 86_400));

        let msix = &report.entries[0];
        assert_eq!((msix.name_kind, msix.location), ("package", "package"));
        assert!(msix.in_use);
        assert_eq!(msix.since_stop_sec, None, "使用中は、終了からの秒数を入れない");
        assert_eq!(msix.since_start_sec, Some(30));

        let webview = &report.entries[1];
        assert_eq!((webview.location, webview.app, webview.stop.clone()), ("ProgramFilesX86", None, None));
        assert!(!webview.in_use, "Stop がない記録は使用中にしない");

        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("山田") && !json.contains("Audacity"), "{json}");

        // 経過の秒数なしで作ると、比べるための形になる（調べるたびに変わる値を入れない）
        let stable = super::report(&store, None);
        assert!(stable.entries.iter().all(|e| e.since_start_sec.is_none() && e.since_stop_sec.is_none()));
        assert!(!serde_json::to_string(&stable).unwrap().contains("since"));

        assert!(!super::report(&MicStore::default(), None).opened, "開けなかったことが分かる");
    }

    #[test]
    fn locations_and_name_kinds() {
        assert_eq!(location(r"C:\Program Files\WindowsApps\com.tinyspeck.slackdesktop_4.53.0.0_x64__8yrtsj140pw4g\app\Slack.exe"), "WindowsApps");
        assert_eq!(location(r"C:\Program Files\Zoom\bin\Zoom.exe"), "ProgramFiles");
        assert_eq!(location(r"C:\Users\x\AppData\Roaming\Zoom\bin\Zoom.exe"), "AppDataRoaming");
        assert_eq!(location(r"C:\Windows\System32\svchost.exe"), "Windows");
        assert_eq!(location(r"C:\Users\x\Desktop\tool.exe"), "UserFolder");
        assert_eq!(location(r"D:\tools\tool.exe"), "other");
        assert_eq!(location("MSTeams_8wekyb3d8bbwe"), "package");
        assert_eq!(name_kind("slack.exe"), "exe");
        assert_eq!(name_kind(r"C:\x\runner"), "other");
    }
}
