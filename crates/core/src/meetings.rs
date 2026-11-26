//! 会議の自動検出。
//!
//! 「どのアプリがマイクを使っているか」を手がかりにする。Windows はタスクバーのマイク表示のために、
//! アプリごとのマイク使用状況をレジストリ（CapabilityAccessManager）に記録しているので、それを数秒ごとに読む。
//! - Slack（ハドル）・Zoom・Teams・Discord のデスクトップ版は、マイクを使い始めたら会議中とみなす
//! - ブラウザがマイクを使っているときは、ウィンドウタイトル（表示中のタブ）が Google Meet などなら会議中とみなす
//!
//! 判定（[`detect`]）と開始・終了の確定（[`MeetingTracker`]）は OS に依存しないのでテストできる。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MeetingApp {
    GoogleMeet,
    SlackHuddle,
    Zoom,
    Teams,
    Discord,
}

impl MeetingApp {
    pub fn label(self) -> &'static str {
        match self {
            MeetingApp::GoogleMeet => "Google Meet",
            MeetingApp::SlackHuddle => "Slack ハドル",
            MeetingApp::Zoom => "Zoom",
            MeetingApp::Teams => "Teams",
            MeetingApp::Discord => "Discord",
        }
    }
}

/// マイクを使った記録のあるアプリ。`id` は実行ファイルのフルパス、またはストアアプリのパッケージ名。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicUser {
    pub id: String,
    pub in_use: bool,
}

/// 表示中のトップレベルウィンドウ。`exe` は実行ファイル名（小文字）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub exe: String,
    pub title: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observation {
    pub mic_users: Vec<MicUser>,
    pub windows: Vec<WindowInfo>,
}

const BROWSERS: &[&str] = &[
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "brave.exe",
    "vivaldi.exe",
    "opera.exe",
    "arc.exe",
];

/// パスの最後の要素（実行ファイル名）を小文字で返す。
pub fn exe_name(id: &str) -> String {
    id.rsplit(['\\', '/']).next().unwrap_or(id).to_lowercase()
}

/// パッケージ名（小文字）の先頭と、その会議アプリ。ストアアプリ・MSIX のアプリは、パッケージ名で記録される。
const PACKAGE_PREFIXES: &[(&str, MeetingApp)] = &[
    // Microsoft Store 版の Slack（https://slack.com/help/articles/360048367814 ）
    ("91750d7e.slack_", MeetingApp::SlackHuddle),
    // MSIX 版の Slack（https://slack.com/help/articles/212475728 ）
    ("com.tinyspeck.slackdesktop_", MeetingApp::SlackHuddle),
    ("msteams_", MeetingApp::Teams),
    ("microsoftteams_", MeetingApp::Teams),
];

/// パッケージ名（小文字）から会議アプリを判定する。パス（区切りを含むもの）と実行ファイル名（.exe で終わるもの）は、
/// パッケージ名ではないので対象にしない（記録では、フルパスを実行ファイル名だけに縮めるため）。
fn packaged_app(lower_id: &str) -> Option<MeetingApp> {
    if lower_id.contains(['\\', '/']) || lower_id.ends_with(".exe") {
        return None;
    }
    PACKAGE_PREFIXES.iter().find_map(|(prefix, app)| {
        let rest = lower_id.strip_prefix(prefix)?;
        (!rest.is_empty()).then_some(*app)
    })
}

fn desktop_app(id: &str) -> Option<MeetingApp> {
    let lower = id.to_lowercase();
    if let Some(app) = packaged_app(&lower) {
        return Some(app);
    }
    match exe_name(&lower).as_str() {
        "slack.exe" => Some(MeetingApp::SlackHuddle),
        "zoom.exe" | "cpthost.exe" => Some(MeetingApp::Zoom),
        // ms-teams_modulehost.exe は、新しい Teams の通話を受け持つプロセス（Microsoft 365 メッセージセンター MC1189656）
        "ms-teams.exe" | "ms-teams_modulehost.exe" | "teams.exe" => Some(MeetingApp::Teams),
        // Discord はボイスチャンネル・通話に入っている間だけマイクを使う（PTB・Canary・Development 版も対象）
        "discord.exe" | "discordptb.exe" | "discordcanary.exe" | "discorddevelopment.exe" => Some(MeetingApp::Discord),
        _ => None,
    }
}

/// ブラウザのウィンドウタイトル（表示中のタブ）から会議サービスを判定する。
pub fn web_meeting(title: &str) -> Option<MeetingApp> {
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
    // ブラウザ版 Discord。ブラウザがマイクを使っていれば通話中
    if is_discord_web_title(&t) {
        return Some(MeetingApp::Discord);
    }
    None
}

/// ブラウザ版 Discord の画面のタブ名か（小文字にしたタイトルを渡す）。
///
/// Discord の画面のタブ名は「Discord | #雑談 | 開発チーム」か「#雑談 | 開発チーム - Discord」の形で、
/// 未読があると「(3) 」「• 」が前に付く。ウィンドウのタイトルでは、その後ろにブラウザの名前が付く。
/// 「discord」を含むだけのタイトル（Discord についての記事・検索結果・ヘルプのページ）は、会議とみなさない。
fn is_discord_web_title(lower_title: &str) -> bool {
    let mut tab = strip_browser_name(lower_title.trim());
    loop {
        if let Some(after) = tab.strip_prefix('•') {
            tab = after.trim_start();
        } else if let Some(after) = unread_count_prefix(tab) {
            tab = after.trim_start();
        } else {
            break;
        }
    }
    tab.starts_with("discord | ") || tab.ends_with(" | discord") || tab.ends_with(" - discord")
}

/// ウィンドウのタイトルの末尾に付くブラウザの名前（小文字）。Edge は、名前の途中に見えない空白を入れることがある。
const BROWSER_TITLE_SUFFIXES: &[&str] = &[
    "google chrome",
    "microsoft edge",
    "microsoft\u{200b} edge",
    "mozilla firefox",
    "brave",
    "vivaldi",
    "opera",
];

/// ウィンドウのタイトルの末尾の「 - Google Chrome」「 — Mozilla Firefox」などを除いて、タブ名だけにする。
fn strip_browser_name(lower_title: &str) -> &str {
    let last_separator = [" - ", " — ", " – "]
        .iter()
        .filter_map(|separator| lower_title.rfind(separator).map(|at| (at, separator.len())))
        .max();
    match last_separator {
        Some((at, len)) if BROWSER_TITLE_SUFFIXES.contains(&lower_title[at + len..].trim()) => {
            lower_title[..at].trim_end()
        }
        _ => lower_title,
    }
}

/// 先頭の「(3)」「(99+)」のような未読の数を除いた残りを返す。
fn unread_count_prefix(title: &str) -> Option<&str> {
    let inner = title.strip_prefix('(')?;
    let close = inner.find(')')?;
    let count = &inner[..close];
    let is_count = !count.is_empty() && count.chars().all(|c| c.is_ascii_digit() || c == '+');
    is_count.then(|| &inner[close + 1..])
}

/// 検出した会議。`via_browser` ならブラウザでの会議（タブのタイトルで判定した）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detection {
    pub app: MeetingApp,
    pub via_browser: bool,
    /// ブラウザでの会議なら、その会議を開いているブラウザの実行ファイル名（小文字。例: `chrome.exe`）。
    /// 会議が続いているかどうかは、このブラウザがマイクを使っているかで決める
    pub browser: Option<&'static str>,
}

impl Detection {
    /// デスクトップの会議アプリでの会議。
    pub fn desktop(app: MeetingApp) -> Self {
        Self {
            app,
            via_browser: false,
            browser: None,
        }
    }

    /// ブラウザでの会議。`exe` は、会議を開いているブラウザの実行ファイル名（一覧にないブラウザなら `None` になる）。
    pub fn in_browser(app: MeetingApp, exe: &str) -> Self {
        let exe = exe.to_lowercase();
        Self {
            app,
            via_browser: true,
            browser: BROWSERS.iter().copied().find(|browser| *browser == exe),
        }
    }
}

/// 自分自身（録音中の Kiroscribe）を除いた、いまマイクを使っているアプリ。
fn active_mic_users<'a>(observation: &'a Observation, own_exe: Option<&str>) -> Vec<&'a MicUser> {
    let own = own_exe.map(str::to_lowercase);
    let own_name = own.as_deref().map(exe_name);
    observation
        .mic_users
        .iter()
        .filter(|u| u.in_use)
        .filter(|u| {
            let id = u.id.to_lowercase();
            Some(&id) != own.as_ref() && Some(exe_name(&id)) != own_name
        })
        .collect()
}

/// いまマイクを使っているブラウザの実行ファイル名（小文字）。
fn browsers_in_use(active: &[&MicUser]) -> Vec<String> {
    active
        .iter()
        .map(|u| exe_name(&u.id))
        .filter(|exe| BROWSERS.contains(&exe.as_str()))
        .collect()
}


/// いま会議が始まっているかどうか。`own_exe` は自分自身（録音中の Kiroscribe）を除外するためのパス。
pub fn detect(observation: &Observation, own_exe: Option<&str>) -> Option<Detection> {
    let active = active_mic_users(observation, own_exe);

    // デスクトップの会議アプリが優先
    if let Some(app) = active.iter().find_map(|u| desktop_app(&u.id)) {
        return Some(Detection::desktop(app));
    }

    // ブラウザがマイクを使っているなら、そのブラウザのウィンドウタイトル（表示中のタブ）を見る。
    // マイクを使っているのとは別のブラウザのタブ名では、会議とみなさない
    let mic_browsers = browsers_in_use(&active);
    if mic_browsers.is_empty() {
        return None;
    }
    observation
        .windows
        .iter()
        .filter(|w| mic_browsers.contains(&w.exe.to_lowercase()))
        .find_map(|w| web_meeting(&w.title).map(|app| Detection::in_browser(app, &w.exe)))
}

/// 始まった会議が続いているか。ブラウザでの会議は、別のタブに切り替えてタイトルが変わっても、
/// 会議を始めたブラウザがマイクを使い続けている間は続いているとみなす
/// （別のブラウザがマイクを使っていても、続きにはしない）。
pub fn still_active(observation: &Observation, meeting: &Detection, own_exe: Option<&str>) -> bool {
    let active = active_mic_users(observation, own_exe);
    if meeting.via_browser {
        let in_use = browsers_in_use(&active);
        match meeting.browser {
            Some(browser) => in_use.iter().any(|exe| exe == browser),
            // どのブラウザの会議か分からないとき（通常は起きない）は、どれかのブラウザが使っていれば続きとする
            None => !in_use.is_empty(),
        }
    } else {
        active.iter().any(|u| desktop_app(&u.id) == Some(meeting.app))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeetingEvent {
    Started(MeetingApp),
    Ended(MeetingApp),
}

/// 検出結果の揺れを吸収して、会議の開始・終了を確定させる。
///
/// - 開始: 同じ会議が `start_confirm_ms` 続いたら（一瞬マイクを使っただけでは反応しない）
/// - 終了: 会議が見えなくなってから `end_grace_ms` たったら（タブの切り替えや一時的な中断では終わらせない）
/// - 切り替え: 会議が見えなくなって `end_grace_ms` たたないうちに、別のアプリの会議が `start_confirm_ms` 続いたら、
///   前の会議の終了を知らせ、次の回に新しい会議の開始を知らせる（別の会議を、前の会議の続きにしない）
#[derive(Debug, Clone)]
pub struct MeetingTracker {
    pub start_confirm_ms: i64,
    pub end_grace_ms: i64,
    candidate: Option<(Detection, i64)>,
    active: Option<Detection>,
    last_seen_ms: i64,
}

impl Default for MeetingTracker {
    fn default() -> Self {
        Self {
            start_confirm_ms: 4_000,
            end_grace_ms: 20_000,
            candidate: None,
            active: None,
            last_seen_ms: 0,
        }
    }
}

impl MeetingTracker {
    /// 進行中の会議。
    pub fn active(&self) -> Option<Detection> {
        self.active
    }

    /// `detected` はいま始まりそうな会議、`continuing` は進行中の会議がまだ続いているか（[`still_active`]）。
    pub fn tick(&mut self, detected: Option<Detection>, continuing: bool, now_ms: i64) -> Option<MeetingEvent> {
        if let Some(active) = self.active {
            // 進行中の会議が続いている（同じアプリの会議がまた見えたときも、続きとみなす）。
            // その間に別の会議が見えても、新しい会議の開始にはしない
            if continuing || detected.is_some_and(|detection| detection.app == active.app) {
                self.last_seen_ms = now_ms;
                self.candidate = None;
                return None;
            }
            // 進行中の会議は見えなくなっている。その間に別のアプリの会議が続けて見えたら、前の会議を終わらせる
            // （新しい会議の開始は、候補を持ったまま、次の回に知らせる）
            match (detected, self.candidate) {
                (Some(detection), Some((candidate, since))) if candidate.app == detection.app => {
                    if now_ms - since >= self.start_confirm_ms {
                        self.active = None;
                        return Some(MeetingEvent::Ended(active.app));
                    }
                }
                (Some(detection), _) => self.candidate = Some((detection, now_ms)),
                (None, _) => self.candidate = None,
            }
            if now_ms - self.last_seen_ms >= self.end_grace_ms {
                self.active = None;
                return Some(MeetingEvent::Ended(active.app));
            }
            return None;
        }

        match detected {
            Some(detection) => match self.candidate {
                Some((candidate, since)) if candidate.app == detection.app => {
                    if now_ms - since >= self.start_confirm_ms {
                        self.active = Some(detection);
                        self.last_seen_ms = now_ms;
                        self.candidate = None;
                        return Some(MeetingEvent::Started(detection.app));
                    }
                    None
                }
                _ => {
                    self.candidate = Some((detection, now_ms));
                    None
                }
            },
            None => {
                self.candidate = None;
                None
            }
        }
    }

    /// 観測結果から 1 回分を進める。
    pub fn observe(&mut self, observation: &Observation, own_exe: Option<&str>, now_ms: i64) -> Option<MeetingEvent> {
        let detected = detect(observation, own_exe);
        let continuing = self
            .active
            .is_some_and(|active| still_active(observation, &active, own_exe));
        self.tick(detected, continuing, now_ms)
    }
}

/// 現在のマイク使用状況とウィンドウを調べる。Windows 以外では何も検出しない。
pub fn observe() -> Observation {
    #[cfg(windows)]
    {
        windows_observer::observe()
    }
    #[cfg(not(windows))]
    {
        Observation::default()
    }
}

#[cfg(windows)]
pub mod windows_observer {
    use std::collections::HashMap;

    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    use super::{exe_name, MicUser, Observation, WindowInfo};

    pub const CONSENT_STORE: &str =
        r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\microphone";

    fn in_use(key: &RegKey) -> bool {
        let start: u64 = key.get_value("LastUsedTimeStart").unwrap_or(0);
        let stop: u64 = key.get_value("LastUsedTimeStop").unwrap_or(1);
        // 使用中は終了時刻が 0 のまま
        start > 0 && stop == 0
    }

    /// `root\path` 以下のマイク使用記録を読む（テストでは別のキーを渡す）。
    pub fn read_mic_users(root: &RegKey, path: &str) -> Vec<MicUser> {
        let Ok(base) = root.open_subkey(path) else {
            return Vec::new();
        };
        let mut users = Vec::new();
        for name in base.enum_keys().flatten() {
            let Ok(key) = base.open_subkey(&name) else {
                continue;
            };
            if name.eq_ignore_ascii_case("NonPackaged") {
                // 通常のデスクトップアプリ。キー名はパスの「\」を「#」に置き換えたもの
                for app in key.enum_keys().flatten() {
                    if let Ok(app_key) = key.open_subkey(&app) {
                        users.push(MicUser {
                            id: app.replace('#', "\\"),
                            in_use: in_use(&app_key),
                        });
                    }
                }
            } else {
                users.push(MicUser {
                    in_use: in_use(&key),
                    id: name,
                });
            }
        }
        users
    }

    fn process_exe(pid: u32, cache: &mut HashMap<u32, String>) -> String {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
            PROCESS_QUERY_LIMITED_INFORMATION,
        };
        if let Some(name) = cache.get(&pid) {
            return name.clone();
        }
        let mut name = String::new();
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if !handle.is_null() {
                let mut buf = [0u16; 1024];
                let mut size = buf.len() as u32;
                if QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut size) != 0 {
                    name = exe_name(&String::from_utf16_lossy(&buf[..size as usize]));
                }
                CloseHandle(handle);
            }
        }
        cache.insert(pid, name.clone());
        name
    }

    unsafe extern "system" fn collect_window(
        hwnd: windows_sys::Win32::Foundation::HWND,
        lparam: windows_sys::Win32::Foundation::LPARAM,
    ) -> windows_sys::core::BOOL {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
        };
        let list = &mut *(lparam as *mut Vec<(u32, String)>);
        if IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return 1;
        }
        let mut buf = vec![0u16; len as usize + 1];
        let copied = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if copied <= 0 {
            return 1;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        list.push((pid, String::from_utf16_lossy(&buf[..copied as usize])));
        1
    }

    /// 表示中のトップレベルウィンドウのタイトルと実行ファイル名。
    pub fn list_windows() -> Vec<WindowInfo> {
        use windows_sys::Win32::UI::WindowsAndMessaging::EnumWindows;
        let mut raw: Vec<(u32, String)> = Vec::new();
        unsafe {
            EnumWindows(Some(collect_window), &mut raw as *mut _ as isize);
        }
        let mut cache = HashMap::new();
        raw.into_iter()
            .map(|(pid, title)| WindowInfo {
                exe: process_exe(pid, &mut cache),
                title,
            })
            .collect()
    }

    pub fn observe() -> Observation {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let mic_users = read_mic_users(&hkcu, CONSENT_STORE);
        // ウィンドウを列挙するのは、ブラウザがマイクを使っているときだけ
        let windows = if mic_users.iter().any(|u| u.in_use) {
            list_windows()
        } else {
            Vec::new()
        };
        Observation { mic_users, windows }
    }

    #[cfg(test)]
    mod tests {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;

        use super::*;

        /// 本物と同じ形のキーをテスト用の場所に作って読む
        // [MTG-020]
        #[test]
        fn reads_consent_store_layout() {
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            let path = format!(r"Software\Kiroscribe-test-{}\microphone", std::process::id());
            let (base, _) = hkcu.create_subkey(&path).unwrap();
            let (np, _) = base.create_subkey("NonPackaged").unwrap();
            let (slack, _) = np.create_subkey(r"C:#Users#me#AppData#Local#slack#slack.exe").unwrap();
            slack.set_value("LastUsedTimeStart", &133_000_000_000_000_000u64).unwrap();
            slack.set_value("LastUsedTimeStop", &0u64).unwrap();
            let (chrome, _) = np.create_subkey(r"C:#Program Files#Google#Chrome#Application#chrome.exe").unwrap();
            chrome.set_value("LastUsedTimeStart", &133_000_000_000_000_000u64).unwrap();
            chrome.set_value("LastUsedTimeStop", &133_000_000_000_100_000u64).unwrap();
            let (teams, _) = base.create_subkey("MSTeams_8wekyb3d8bbwe").unwrap();
            teams.set_value("LastUsedTimeStart", &133_000_000_000_000_000u64).unwrap();
            teams.set_value("LastUsedTimeStop", &0u64).unwrap();

            let mut users = read_mic_users(&hkcu, &path);
            users.sort_by(|a, b| a.id.cmp(&b.id));
            hkcu.delete_subkey_all(path.rsplit_once('\\').unwrap().0).unwrap();

            assert_eq!(users.len(), 3);
            let slack = users.iter().find(|u| u.id.ends_with("slack.exe")).unwrap();
            assert_eq!(slack.id, r"C:\Users\me\AppData\Local\slack\slack.exe");
            assert!(slack.in_use);
            assert!(!users.iter().find(|u| u.id.ends_with("chrome.exe")).unwrap().in_use);
            assert!(users.iter().find(|u| u.id.starts_with("MSTeams")).unwrap().in_use);
        }

        // [MTG-020]
        #[test]
        fn observe_does_not_fail() {
            // 実際のレジストリとウィンドウを読んでもパニックしない
            let _ = observe();
            let _ = list_windows();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mic(id: &str, in_use: bool) -> MicUser {
        MicUser {
            id: id.into(),
            in_use,
        }
    }

    fn win(exe: &str, title: &str) -> WindowInfo {
        WindowInfo {
            exe: exe.into(),
            title: title.into(),
        }
    }

    const CHROME: &str = r"C:\Program Files\Google\Chrome\Application\chrome.exe";

    fn app(d: Option<Detection>) -> Option<MeetingApp> {
        d.map(|d| d.app)
    }

    fn found(app: MeetingApp, via_browser: bool) -> Option<Detection> {
        Some(if via_browser {
            Detection::in_browser(app, "chrome.exe")
        } else {
            Detection::desktop(app)
        })
    }

    // [MTG-001][MTG-003]
    #[test]
    fn desktop_apps_using_mic_are_meetings() {
        let obs = |id: &str| Observation {
            mic_users: vec![mic(id, true)],
            windows: vec![],
        };
        assert_eq!(detect(&obs(r"C:\Users\me\AppData\Local\slack\slack.exe"), None), found(MeetingApp::SlackHuddle, false));
        assert_eq!(app(detect(&obs(r"C:\Users\me\AppData\Roaming\Zoom\bin\Zoom.exe"), None)), Some(MeetingApp::Zoom));
        assert_eq!(app(detect(&obs("MSTeams_8wekyb3d8bbwe"), None)), Some(MeetingApp::Teams));
        assert_eq!(
            detect(&obs(r"C:\Users\me\AppData\Local\Discord\app-1.0.9187\Discord.exe"), None),
            found(MeetingApp::Discord, false)
        );
        assert_eq!(
            app(detect(&obs(r"C:\Users\me\AppData\Local\DiscordPTB\app-1.0.1100\DiscordPTB.exe"), None)),
            Some(MeetingApp::Discord)
        );
        assert_eq!(detect(&obs(r"C:\Program Files\Audacity\Audacity.exe"), None), None, "会議アプリ以外は無視");
    }

    // [MTG-004]
    #[test]
    fn apps_not_using_mic_now_are_ignored() {
        let obs = Observation {
            mic_users: vec![mic(r"C:\slack\slack.exe", false)],
            windows: vec![],
        };
        assert_eq!(detect(&obs, None), None);
    }

    // [MTG-002][MTG-003]
    #[test]
    fn browser_meetings_are_detected_by_window_title() {
        let base = |title: &str| Observation {
            mic_users: vec![mic(CHROME, true)],
            windows: vec![win("explorer.exe", "エクスプローラー"), win("chrome.exe", title)],
        };
        assert_eq!(detect(&base("Meet - abc-defg-hij - Google Chrome"), None), found(MeetingApp::GoogleMeet, true));
        assert_eq!(app(detect(&base("Meet – 週次定例 - Google Chrome"), None)), Some(MeetingApp::GoogleMeet));
        assert_eq!(app(detect(&base("ハドル: #dev - Acme - Slack - Google Chrome"), None)), Some(MeetingApp::SlackHuddle));
        assert_eq!(detect(&base("Discord | #雑談 | 開発チーム - Google Chrome"), None), found(MeetingApp::Discord, true));
        assert_eq!(app(detect(&base("#general | Acme - Discord - Google Chrome"), None)), Some(MeetingApp::Discord));
        assert_eq!(detect(&base("YouTube - Google Chrome"), None), None, "会議以外のサイトでのマイク使用は無視");
    }

    // [MTG-004]
    #[test]
    fn browser_title_needs_browser_using_mic() {
        let obs = Observation {
            mic_users: vec![mic(CHROME, false)],
            windows: vec![win("chrome.exe", "Meet - abc-defg-hij - Google Chrome")],
        };
        assert_eq!(detect(&obs, None), None, "Meet を開いているだけでは会議中としない");
    }

    // [MTG-005]
    #[test]
    fn own_process_is_excluded() {
        let own = r"C:\Users\me\AppData\Local\Kiroscribe\kiroscribe.exe";
        let obs = Observation {
            mic_users: vec![mic(own, true), mic(CHROME, true)],
            windows: vec![win("chrome.exe", "Gmail - Google Chrome")],
        };
        assert_eq!(detect(&obs, Some(own)), None);
        let obs = Observation {
            mic_users: vec![mic(own, true), mic(r"C:\slack\slack.exe", true)],
            windows: vec![],
        };
        assert_eq!(app(detect(&obs, Some(&own.to_uppercase()))), Some(MeetingApp::SlackHuddle));
    }

    // [MTG-009]
    #[test]
    fn browser_meeting_continues_after_switching_tabs() {
        let meet = Detection::in_browser(MeetingApp::GoogleMeet, "chrome.exe");
        // 別のタブ（資料）を見ていても、ブラウザがマイクを使っていれば続いている
        let switched = Observation {
            mic_users: vec![mic(CHROME, true)],
            windows: vec![win("chrome.exe", "企画書 - Google ドキュメント - Google Chrome")],
        };
        assert_eq!(detect(&switched, None), None);
        assert!(still_active(&switched, &meet, None));
        // 通話を抜けてマイクを放したら終わり
        let left = Observation {
            mic_users: vec![mic(CHROME, false)],
            windows: switched.windows.clone(),
        };
        assert!(!still_active(&left, &meet, None));
        // 会議を始めたブラウザがマイクを放したら、別のブラウザがマイクを使っていても終わり
        let other_browser = Observation {
            mic_users: vec![mic(CHROME, false), mic(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe", true)],
            windows: vec![win("msedge.exe", "音声入力のテスト - Microsoft Edge")],
        };
        assert!(!still_active(&other_browser, &meet, None), "別のブラウザのマイクの使用を、会議の続きにしている");
        // 2 つのブラウザが使っているときは、会議を始めたほうが使っているかで決まる
        let both = Observation {
            mic_users: vec![mic(CHROME, true), mic(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe", true)],
            windows: vec![],
        };
        assert!(still_active(&both, &meet, None));
        assert!(!still_active(&both, &Detection::in_browser(MeetingApp::GoogleMeet, "firefox.exe"), None));
    }

    // [MTG-009]
    #[test]
    fn desktop_meeting_continues_only_while_that_app_uses_mic() {
        let zoom = Detection::desktop(MeetingApp::Zoom);
        let obs = |zoom_in_use: bool| Observation {
            mic_users: vec![mic(r"C:\Zoom\bin\Zoom.exe", zoom_in_use), mic(CHROME, true)],
            windows: vec![],
        };
        assert!(still_active(&obs(true), &zoom, None));
        assert!(!still_active(&obs(false), &zoom, None), "ブラウザがマイクを使っていても Zoom の会議は終わり");
    }

    // [MTG-007][MTG-008][MTG-009]
    #[test]
    fn tracker_follows_observations_end_to_end() {
        let mut t = MeetingTracker::default();
        let in_meet = Observation {
            mic_users: vec![mic(CHROME, true)],
            windows: vec![win("chrome.exe", "Meet - abc-defg-hij - Google Chrome")],
        };
        let other_tab = Observation {
            mic_users: vec![mic(CHROME, true)],
            windows: vec![win("chrome.exe", "Slack - Google Chrome")],
        };
        let left = Observation::default();
        assert_eq!(t.observe(&in_meet, None, 0), None);
        assert_eq!(t.observe(&in_meet, None, 4_000), Some(MeetingEvent::Started(MeetingApp::GoogleMeet)));
        // タブを切り替えて 1 分たっても終わらない
        for s in 1..=30 {
            assert_eq!(t.observe(&other_tab, None, 4_000 + s * 2_000), None);
        }
        assert_eq!(t.observe(&left, None, 70_000), None);
        assert_eq!(t.observe(&left, None, 84_000), Some(MeetingEvent::Ended(MeetingApp::GoogleMeet)));
    }

    // [MTG-007]
    #[test]
    fn tracker_confirms_start_after_a_few_seconds() {
        let mut t = MeetingTracker::default();
        let meet = found(MeetingApp::GoogleMeet, true);
        assert_eq!(t.tick(meet, false, 0), None);
        assert_eq!(t.tick(meet, false, 2_000), None);
        assert_eq!(t.tick(meet, false, 4_000), Some(MeetingEvent::Started(MeetingApp::GoogleMeet)));
        assert_eq!(app(t.active()), Some(MeetingApp::GoogleMeet));
        assert_eq!(t.tick(meet, true, 6_000), None, "開始は一度だけ");
    }

    // [MTG-007]
    #[test]
    fn brief_mic_use_does_not_start() {
        let mut t = MeetingTracker::default();
        let slack = found(MeetingApp::SlackHuddle, false);
        assert_eq!(t.tick(slack, false, 0), None);
        assert_eq!(t.tick(None, false, 2_000), None);
        assert_eq!(t.tick(slack, false, 4_000), None, "途切れたら数え直す");
        assert_eq!(t.tick(slack, false, 8_000), Some(MeetingEvent::Started(MeetingApp::SlackHuddle)));
    }

    // [MTG-008]
    #[test]
    fn tracker_ends_after_grace_period() {
        let mut t = MeetingTracker::default();
        let zoom = found(MeetingApp::Zoom, false);
        t.tick(zoom, false, 0);
        t.tick(zoom, false, 4_000);
        // 15 秒途切れても終わらない
        assert_eq!(t.tick(None, false, 10_000), None);
        assert_eq!(t.tick(None, false, 19_000), None);
        assert_eq!(t.tick(None, true, 20_000), None, "戻ってきたら続き");
        assert_eq!(t.tick(None, false, 30_000), None);
        assert_eq!(t.tick(None, false, 40_000), Some(MeetingEvent::Ended(MeetingApp::Zoom)));
        assert_eq!(t.active(), None);
        assert_eq!(t.tick(None, false, 50_000), None);
    }

    /// 会議の検出が調べるのは、マイクを使っているアプリの名前と、表示中のウィンドウのタイトルだけ。
    /// 調べるものを増やすとき（項目を足すとき）は、ここがコンパイルできなくなるので、仕様（SEC-013）と一緒に見直す。
    // [SEC-013]
    #[test]
    fn detection_looks_only_at_mic_users_and_window_titles() {
        let Observation { mic_users, windows } = observe();
        for MicUser { id, in_use } in mic_users {
            let _: (String, bool) = (id, in_use);
        }
        for WindowInfo { exe, title } in windows {
            let _: (String, String) = (exe, title);
        }
        // 調べるのは PC の中だけ: 通信・音声・画面の取り込みの部品を、この部品（meetings.rs）は使わない
        // （Windows では改行が CRLF で取り出されることがあるので、そろえてから探す）
        let source = include_str!("meetings.rs").replace("\r\n", "\n");
        let code = &source[..source.find("\n#[cfg(test)]\nmod tests").expect("テストの手前まで")];
        for forbidden in ["reqwest", "crate::http", "crate::audio", "crate::transcribe", "crate::claude", "BitBlt", "PrintWindow"] {
            assert!(!code.contains(forbidden), "会議の検出が {forbidden} を使っている");
        }
    }

    // [MTG-001]
    #[test]
    fn exe_name_handles_both_separators() {
        assert_eq!(exe_name(r"C:\A\Slack.EXE"), "slack.exe");
        assert_eq!(exe_name("/usr/bin/zoom"), "zoom");
        assert_eq!(exe_name("MSTeams_8wekyb3d8bbwe"), "msteams_8wekyb3d8bbwe");
    }
}
