//! Windows のオーディオのセッションから、マイクを使っているプロセスを読む。
//!
//! Windows は、録音のデバイス（マイク）ごとに、録音しているアプリのセッションを持っている。
//! セッションの状態が Active なら、そのプロセスはいまマイクから録音している。
//! 会議の検出は、これを第一の手がかりにする（[`crate::meetings::MicSources`]）。
//!
//! 読むのは、セッションのプロセス ID と状態だけ。音声や音量は読まない（SEC-013）。
//! ログには、実行ファイルの名前・パッケージファミリー名・状態だけを書き、フルパスは書かない。

use serde::Serialize;

use crate::meetings::{desktop_app, exe_name, MeetingApp, MicUser, BROWSERS};

/// セッションの状態（`AudioSessionState`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionState {
    /// いま録音している
    Active,
    /// 録音のストリームはあるが、止まっている
    Inactive,
    /// 終わったセッション
    Expired,
}

impl SessionState {
    pub fn name(self) -> &'static str {
        match self {
            SessionState::Active => "active",
            SessionState::Inactive => "inactive",
            SessionState::Expired => "expired",
        }
    }
}

/// 録音のセッション 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicSession {
    pub pid: u32,
    /// 実行ファイルのフルパス（読めなかったときは空）
    pub exe_path: String,
    /// パッケージのアプリ（Microsoft Store 版・MSIX 版）なら、パッケージファミリー名
    pub package: Option<String>,
    pub state: SessionState,
}

/// 実行ファイルのパスもパッケージ名も読めなかったプロセスの名前。
pub const UNKNOWN: &str = "unknown";

impl MicSession {
    /// 会議アプリの判定に使う名前: パッケージファミリー名が会議アプリに当たればパッケージファミリー名、
    /// そうでなければ実行ファイルのフルパス（読めなければパッケージファミリー名、それもなければ [`UNKNOWN`]）。
    pub fn id(&self) -> String {
        match &self.package {
            Some(package) if desktop_app(package).is_some() => package.clone(),
            _ if !self.exe_path.is_empty() => self.exe_path.clone(),
            Some(package) => package.clone(),
            None => UNKNOWN.to_string(),
        }
    }

    /// ログに書く実行ファイルの名前（小文字。フルパスは書かない）。
    pub fn exe(&self) -> String {
        if self.exe_path.is_empty() {
            UNKNOWN.to_string()
        } else {
            exe_name(&self.exe_path)
        }
    }
}

/// オーディオのセッションの一覧を読んだ結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MicSessions {
    /// 一覧を取れたか（録音のデバイスの一覧を取れなかったときは false）
    pub available: bool,
    /// 有効な録音のデバイスの数
    pub endpoints: usize,
    pub sessions: Vec<MicSession>,
    /// 取れなかった理由（HRESULT。ログに書く）
    pub error: Option<String>,
}

/// セッションの一覧から、マイクを使ったアプリの一覧にする。同じアプリのセッションが複数あれば 1 つにまとめ、
/// どれかが Active なら使用中とする。
pub fn mic_users(sessions: &MicSessions) -> Vec<MicUser> {
    let mut users: Vec<MicUser> = Vec::new();
    for session in &sessions.sessions {
        let id = session.id();
        let in_use = session.state == SessionState::Active;
        match users.iter_mut().find(|user| user.id.eq_ignore_ascii_case(&id)) {
            Some(user) => user.in_use |= in_use,
            None => users.push(MicUser { id, in_use }),
        }
    }
    users
}

/// ログに書く、セッション 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MicSessionReport {
    /// 実行ファイルの名前（小文字）
    pub exe: String,
    /// パッケージファミリー名
    pub package: Option<String>,
    pub state: SessionState,
    /// 会議アプリとして当たるなら、そのアプリ
    pub app: Option<MeetingApp>,
    /// ブラウザか
    pub browser: bool,
}

/// ログに書く、オーディオのセッションの様子。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MicSessionsReport {
    pub available: bool,
    pub endpoints: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub sessions: Vec<MicSessionReport>,
}

/// セッションの一覧を、ログに書く形にする（すべてのセッションを、名前の順に並べる）。
pub fn report(sessions: &MicSessions) -> MicSessionsReport {
    let mut list: Vec<MicSessionReport> = sessions
        .sessions
        .iter()
        .map(|session| MicSessionReport {
            exe: session.exe(),
            package: session.package.clone(),
            state: session.state,
            app: desktop_app(&session.id()),
            browser: BROWSERS.contains(&session.exe().as_str()),
        })
        .collect();
    list.sort_by(|a, b| (&a.exe, &a.package, a.state).cmp(&(&b.exe, &b.package, b.state)));
    MicSessionsReport {
        available: sessions.available,
        endpoints: sessions.endpoints,
        error: sessions.error.clone(),
        sessions: list,
    }
}

/// いまのオーディオのセッションの一覧を読む。Windows 以外では、取れない（`available` が false）。
pub fn read() -> MicSessions {
    #[cfg(windows)]
    {
        windows_sessions::read()
    }
    #[cfg(not(windows))]
    {
        MicSessions::default()
    }
}

#[cfg(windows)]
mod windows_sessions {
    use windows::core::Interface;
    use windows::Win32::Media::Audio::{
        eCapture, AudioSessionStateActive, AudioSessionStateExpired, IAudioSessionControl2,
        IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    use super::{MicSession, MicSessions, SessionState};

    /// このスレッドで COM を使えるようにし、終わったら戻す（初期化できたときだけ）。
    struct Com(bool);

    impl Com {
        fn init() -> Self {
            // すでに別の方式で初期化されていても（RPC_E_CHANGED_MODE）、COM は使える
            Com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
        }
    }

    impl Drop for Com {
        fn drop(&mut self) {
            if self.0 {
                unsafe { CoUninitialize() };
            }
        }
    }

    pub fn read() -> MicSessions {
        let _com = Com::init();
        match unsafe { list_sessions() } {
            Ok((endpoints, sessions)) => MicSessions {
                available: true,
                endpoints,
                sessions,
                error: None,
            },
            Err(err) => MicSessions {
                available: false,
                endpoints: 0,
                sessions: Vec::new(),
                error: Some(format!("0x{:08X}", err.code().0 as u32)),
            },
        }
    }

    /// すべての有効な録音のデバイスの、セッションを読む。デバイス 1 つ・セッション 1 つが読めなくても、ほかは読む。
    unsafe fn list_sessions() -> windows::core::Result<(usize, Vec<MicSession>)> {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let devices = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
        let count = devices.GetCount()?;
        let mut sessions = Vec::new();
        for index in 0..count {
            let Ok(device) = devices.Item(index) else { continue };
            let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
            let Ok(list) = manager.GetSessionEnumerator() else { continue };
            let Ok(total) = list.GetCount() else { continue };
            for at in 0..total {
                let Ok(control) = list.GetSession(at) else { continue };
                let Ok(control2) = control.cast::<IAudioSessionControl2>() else { continue };
                // システムの音のセッション（プロセスに結び付かない）は除く
                if control2.IsSystemSoundsSession().0 == 0 {
                    continue;
                }
                let Ok(state) = control.GetState() else { continue };
                let state = if state == AudioSessionStateActive {
                    SessionState::Active
                } else if state == AudioSessionStateExpired {
                    SessionState::Expired
                } else {
                    SessionState::Inactive
                };
                let pid = control2.GetProcessId().unwrap_or(0);
                if pid == 0 {
                    continue;
                }
                let (exe_path, package) = process_identity(pid);
                sessions.push(MicSession {
                    pid,
                    exe_path,
                    package,
                    state,
                });
            }
        }
        Ok((count as usize, sessions))
    }

    /// プロセスの実行ファイルのフルパスと、パッケージのアプリならパッケージファミリー名。
    fn process_identity(pid: u32) -> (String, Option<String>) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::Storage::Packaging::Appx::GetPackageFamilyName;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let mut path = String::new();
        let mut package = None;
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return (path, package);
            }
            let mut buf = [0u16; 1024];
            let mut size = buf.len() as u32;
            if QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut size) != 0 {
                path = String::from_utf16_lossy(&buf[..size as usize]);
            }
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            // パッケージのないプロセスでは APPMODEL_ERROR_NO_PACKAGE が返る
            if GetPackageFamilyName(handle, &mut len, name.as_mut_ptr()) == 0 && len > 0 {
                let end = name.iter().position(|&c| c == 0).unwrap_or(len as usize);
                package = Some(String::from_utf16_lossy(&name[..end]));
            }
            CloseHandle(handle);
        }
        (path, package)
    }

    #[cfg(test)]
    mod tests {
        // [MTG-020]
        #[test]
        fn reading_sessions_does_not_fail() {
            // 実際のオーディオのセッションを読んでもパニックしない。録音のデバイスがない PC でも、一覧は取れる
            let sessions = super::read();
            assert!(sessions.available, "{:?}", sessions.error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(exe_path: &str, package: Option<&str>, state: SessionState) -> MicSession {
        MicSession {
            pid: 100,
            exe_path: exe_path.into(),
            package: package.map(str::to_string),
            state,
        }
    }

    const STORE_SLACK: &str = r"C:\Program Files\WindowsApps\91750D7E.Slack_4.47.69.0_x64__8she8kybcnzg4\app\Slack.exe";

    /// Active のセッションのアプリを使用中とする。パッケージのアプリは、パッケージファミリー名でも判定できる。
    // [MTG-020][MTG-001]
    #[test]
    fn active_sessions_are_mic_users() {
        let sessions = MicSessions {
            available: true,
            endpoints: 2,
            sessions: vec![
                session(STORE_SLACK, Some("91750D7E.Slack_8she8kybcnzg4"), SessionState::Active),
                session(r"C:\Program Files\Google\Chrome\Application\chrome.exe", None, SessionState::Inactive),
                session(r"C:\Program Files\Google\Chrome\Application\chrome.exe", None, SessionState::Active),
                session(r"C:\Users\山田\AppData\Roaming\Zoom\bin\Zoom.exe", None, SessionState::Expired),
            ],
            error: None,
        };
        let users = mic_users(&sessions);
        assert_eq!(
            users,
            vec![
                MicUser { id: "91750D7E.Slack_8she8kybcnzg4".into(), in_use: true },
                MicUser { id: r"C:\Program Files\Google\Chrome\Application\chrome.exe".into(), in_use: true },
                MicUser { id: r"C:\Users\山田\AppData\Roaming\Zoom\bin\Zoom.exe".into(), in_use: false },
            ],
            "同じアプリのセッションは 1 つにまとめ、どれかが Active なら使用中"
        );
        let observation = crate::meetings::Observation { mic_users: users, windows: vec![] };
        assert_eq!(
            crate::meetings::detect(&observation, None).map(|d| d.app),
            Some(MeetingApp::SlackHuddle)
        );
    }

    /// パッケージ名が会議アプリに当たらないパッケージのアプリは、実行ファイルの名前で判定する。
    // [MTG-020][MTG-001][MTG-003]
    #[test]
    fn session_id_prefers_a_matching_package_then_the_exe() {
        let teams = session(r"C:\Program Files\WindowsApps\MSTeams_25.1.0.0_x64__8wekyb3d8bbwe\ms-teams.exe", Some("MSTeams_8wekyb3d8bbwe"), SessionState::Active);
        assert_eq!(teams.id(), "MSTeams_8wekyb3d8bbwe");
        let packaged_zoom = session(r"C:\Program Files\WindowsApps\Zoom.Workplace_6.0.0.0_x64__abc\Zoom.exe", Some("Zoom.Workplace_abc"), SessionState::Active);
        assert_eq!(desktop_app(&packaged_zoom.id()), Some(MeetingApp::Zoom));
        let recorder = session(r"C:\Program Files\WindowsApps\Microsoft.WindowsSoundRecorder_1.0.0.0_x64__8wekyb3d8bbwe\SoundRec.exe", Some("Microsoft.WindowsSoundRecorder_8wekyb3d8bbwe"), SessionState::Active);
        assert_eq!(desktop_app(&recorder.id()), None, "会議アプリ以外は当たらない");
        assert_eq!(session("", Some("Contoso.App_x1"), SessionState::Active).id(), "Contoso.App_x1");
        assert_eq!(session("", None, SessionState::Active).id(), UNKNOWN);
    }

    /// ログに書くのは、実行ファイルの名前・パッケージファミリー名・状態・会議アプリとして当たるか・ブラウザかだけ。
    /// フルパス（利用者の名前を含む）とプロセス ID は書かない。
    // [LOG-005]
    #[test]
    fn report_lists_sessions_without_full_paths() {
        let sessions = MicSessions {
            available: true,
            endpoints: 1,
            sessions: vec![
                session(r"C:\Users\山田 太郎\AppData\Local\slack\app-4.47.69\slack.exe", None, SessionState::Inactive),
                session(STORE_SLACK, Some("91750D7E.Slack_8she8kybcnzg4"), SessionState::Active),
                session(r"C:\Program Files\Google\Chrome\Application\chrome.exe", None, SessionState::Active),
            ],
            error: None,
        };
        let report = report(&sessions);
        assert!(report.available);
        assert_eq!(report.endpoints, 1);
        let rows: Vec<(&str, Option<&str>, SessionState, Option<MeetingApp>, bool)> = report
            .sessions
            .iter()
            .map(|s| (s.exe.as_str(), s.package.as_deref(), s.state, s.app, s.browser))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("chrome.exe", None, SessionState::Active, None, true),
                ("slack.exe", None, SessionState::Inactive, Some(MeetingApp::SlackHuddle), false),
                ("slack.exe", Some("91750D7E.Slack_8she8kybcnzg4"), SessionState::Active, Some(MeetingApp::SlackHuddle), false),
            ]
        );
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("山田") && !json.contains("WindowsApps") && !json.contains("pid"), "{json}");
        assert!(!json.contains("error"), "取れたときは理由を書かない: {json}");
        assert!(json.contains(r#""state":"active""#), "{json}");

        let failed = super::report(&MicSessions { error: Some("0x80070005".into()), ..Default::default() });
        assert!(!failed.available);
        assert_eq!(failed.error.as_deref(), Some("0x80070005"));
    }

    /// Windows 以外では、オーディオのセッションは取れない。
    // [MTG-021]
    #[cfg(not(windows))]
    #[test]
    fn sessions_are_unavailable_outside_windows() {
        assert_eq!(read(), MicSessions::default());
    }

    /// オーディオのセッションから読むのは、プロセスと状態だけ。音声と音量を読む部品は使わない。
    // [SEC-013]
    #[test]
    fn reads_no_audio_or_levels() {
        let source = include_str!("mic_sessions.rs").replace("\r\n", "\n");
        let code = &source[..source.find("\n#[cfg(test)]\nmod tests").expect("テストの手前まで")];
        for forbidden in ["IAudioCaptureClient", "IAudioClient", "IAudioMeterInformation", "ISimpleAudioVolume", "crate::audio::", "reqwest"] {
            assert!(!code.contains(forbidden), "オーディオのセッションの読み取りが {forbidden} を使っている");
        }
    }
}
