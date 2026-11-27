//! Windows のオーディオのセッションから、マイクを使っているプロセスを読む。
//!
//! Windows は、録音のデバイス（マイク）ごとに、録音しているアプリのセッションを持っている。
//! セッションの状態が Active なら、そのプロセスはいまマイクから録音している。
//! 会議の検出は、これを第一の手がかりにする（[`crate::meetings::MicSources`]）。
//!
//! 読むのは、セッションのプロセス ID と状態だけ。音声や音量は読まない（SEC-013）。
//! ログには、実行ファイルの名前・パッケージファミリー名・状態だけを書き、フルパスは書かない。

use serde::Serialize;

use crate::meetings::{desktop_app, exe_name, is_helper_exe, MeetingApp, MicUser, BROWSERS};

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
    /// 会議アプリと一緒に入る更新・起動のためのプロセスと、ほかのアプリも使う部品（MTG-003）は、
    /// パッケージファミリー名が会議アプリに当たっても、実行ファイルのパスで判定する（会議とみなさない）。
    pub fn id(&self) -> String {
        let package = self.package.as_deref().filter(|package| !package.is_empty());
        let helper = !self.exe_path.is_empty() && is_helper_exe(&self.exe_path);
        match package {
            Some(package) if !helper && desktop_app(package).is_some() => package.to_string(),
            _ if !self.exe_path.is_empty() => self.exe_path.clone(),
            Some(package) => package.to_string(),
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
    /// 一覧を取れたか（[`summarize`] の決まりで決める。false なら ConsentStore を使う）
    pub available: bool,
    /// 有効な録音のデバイスの数
    pub endpoints: usize,
    /// セッションを読めなかったデバイスの数
    pub failed_endpoints: usize,
    /// 情報（状態・プロセス ID など）を読めなかったセッションの数
    pub failed_sessions: usize,
    pub sessions: Vec<MicSession>,
    /// 取れなかった理由か、一部を読めなかったときの最初の理由（HRESULT。ログに書く）
    pub error: Option<String>,
}

/// 一覧の読み取りの結果の区分（ログに書く）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ReadStatus {
    /// すべてのデバイスとセッションを読めた
    Ok,
    /// 有効な録音のデバイスがない（一覧は取れていて、空）
    NoDevices,
    /// 一部のデバイスかセッションを読めなかった（読めたものを使う）
    Partial,
    /// 一覧を取れなかった（ConsentStore を使う）
    Failed,
}

impl MicSessions {
    pub fn status(&self) -> ReadStatus {
        if !self.available {
            ReadStatus::Failed
        } else if self.endpoints == 0 {
            ReadStatus::NoDevices
        } else if self.failed_endpoints > 0 || self.failed_sessions > 0 {
            ReadStatus::Partial
        } else {
            ReadStatus::Ok
        }
    }
}

/// 録音のデバイス 1 台から読んだセッション。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EndpointSessions {
    pub sessions: Vec<MicSession>,
    /// 情報を読めなかったセッションごとの理由（HRESULT）
    pub failed: Vec<String>,
}

/// デバイスごとの読み取りの結果をまとめる（`Err` はセッションを読めなかったデバイスと、その理由）。
/// - デバイスがない: 一覧は取れた（空）とする
/// - すべてのデバイスを読めなかった、またはセッションがあるのに、どのセッションの情報も読めなかった: 取れなかったとする
/// - 一部だけ読めなかった: 読めたものを使い、読めなかった数と最初の理由を残す
pub fn summarize(endpoints: Vec<Result<EndpointSessions, String>>) -> MicSessions {
    let mut result = MicSessions { endpoints: endpoints.len(), ..Default::default() };
    for endpoint in endpoints {
        match endpoint {
            Ok(read) => {
                result.failed_sessions += read.failed.len();
                if result.error.is_none() {
                    result.error = read.failed.into_iter().next();
                }
                result.sessions.extend(read.sessions);
            }
            Err(err) => {
                result.failed_endpoints += 1;
                result.error.get_or_insert(err);
            }
        }
    }
    let all_endpoints_failed = result.endpoints > 0 && result.failed_endpoints == result.endpoints;
    let all_sessions_failed = result.sessions.is_empty() && result.failed_sessions > 0;
    result.available = !(all_endpoints_failed || all_sessions_failed);
    result
}

/// HRESULT を、ログに書く形（`0x8007001F` など）にする。
pub fn hresult(code: i32) -> String {
    format!("0x{:08X}", code as u32)
}

/// 読んだパッケージファミリー名を整える（空のときは、パッケージのアプリではないとする）。
pub fn package_name(raw: &[u16]) -> Option<String> {
    let end = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    let name = String::from_utf16_lossy(&raw[..end]);
    let name = name.trim();
    (!name.is_empty()).then(|| name.to_string())
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

/// ログに書く、セッション 1 件。ないもの（パッケージ・会議アプリ）と、ブラウザでないことは書かない
/// （値の数の上限の中で、多くのセッションを残すため）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MicSessionReport {
    /// 実行ファイルの名前（小文字）
    pub exe: String,
    pub state: SessionState,
    /// 会議アプリとして当たるなら、そのアプリ
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<MeetingApp>,
    /// ブラウザか
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub browser: bool,
    /// パッケージファミリー名
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
}

impl MicSessionReport {
    /// 判定に関わるセッションか: いま録音しているか、会議アプリかブラウザ。
    pub fn relevant(&self) -> bool {
        self.state == SessionState::Active || self.app.is_some() || self.browser
    }
}

/// ログに書く、オーディオのセッションの様子。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MicSessionsReport {
    pub status: ReadStatus,
    pub available: bool,
    pub endpoints: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub failed_endpoints: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub failed_sessions: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// セッションの数（一覧に書かなかったものも含む）
    pub total: usize,
    pub sessions: Vec<MicSessionReport>,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// ふだんのログの一覧に書く、セッションの数の上限（値の数の上限の中で、ConsentStore の様子も残すため）。
pub const MAX_LOGGED_SESSIONS: usize = 8;

/// セッションの一覧をログ用に整える。Active を先に並べ、同じ状態の中では会議アプリとブラウザを優先する。
/// `all` が false（ふだんのログ）なら、判定に関わるセッション（[`MicSessionReport::relevant`]）だけを、
/// [`MAX_LOGGED_SESSIONS`] 件まで書く。true なら（確認用の道具）、すべて書く。
pub fn report(sessions: &MicSessions, all: bool) -> MicSessionsReport {
    let mut list: Vec<MicSessionReport> = sessions
        .sessions
        .iter()
        .map(|session| MicSessionReport {
            exe: session.exe(),
            package: session.package.clone().filter(|package| !package.is_empty()),
            state: session.state,
            app: desktop_app(&session.id()),
            browser: BROWSERS.contains(&session.exe().as_str()),
        })
        .collect();
    list.sort_by(|a, b| {
        let rank = |s: &MicSessionReport| (s.state != SessionState::Active, s.app.is_none(), !s.browser, s.state);
        (rank(a), &a.exe, &a.package).cmp(&(rank(b), &b.exe, &b.package))
    });
    if !all {
        list.dedup();
        list.retain(MicSessionReport::relevant);
        list.truncate(MAX_LOGGED_SESSIONS);
    }
    MicSessionsReport {
        status: sessions.status(),
        available: sessions.available,
        endpoints: sessions.endpoints,
        failed_endpoints: sessions.failed_endpoints,
        failed_sessions: sessions.failed_sessions,
        error: sessions.error.clone(),
        total: sessions.sessions.len(),
        sessions: list,
    }
}

/// いまの録音のオーディオのセッションの一覧を読む。Windows 以外では、取れない（`available` が false）。
pub fn read() -> MicSessions {
    #[cfg(windows)]
    {
        windows_sessions::read(windows_sessions::Flow::Capture)
    }
    #[cfg(not(windows))]
    {
        MicSessions::default()
    }
}

/// いまの再生のオーディオのセッションの一覧を読む（確認用の道具で、会議アプリが音を鳴らしているかを見るため）。
/// `endpoints` は有効な再生のデバイスの数。Windows 以外では、取れない。
pub fn read_playback() -> MicSessions {
    #[cfg(windows)]
    {
        windows_sessions::read(windows_sessions::Flow::Render)
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
        eCapture, eRender, AudioSessionStateActive, AudioSessionStateExpired, IAudioSessionControl2,
        IAudioSessionEnumerator, IAudioSessionManager2, IMMDeviceCollection, IMMDeviceEnumerator, MMDeviceEnumerator,
        DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    use super::{EndpointSessions, MicSession, MicSessions, SessionState};

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

    /// 読むデバイスの向き。
    #[derive(Clone, Copy)]
    pub enum Flow {
        /// 録音（マイク）
        Capture,
        /// 再生（スピーカー）
        Render,
    }

    pub fn read(flow: Flow) -> MicSessions {
        let _com = Com::init();
        match unsafe { list_endpoints(flow) } {
            Ok(endpoints) => super::summarize(endpoints),
            Err(err) => MicSessions {
                available: false,
                error: Some(super::hresult(err.code().0)),
                ..Default::default()
            },
        }
    }

    fn code(err: &windows::core::Error) -> String {
        super::hresult(err.code().0)
    }

    /// すべての有効なデバイスの、セッションを読む。デバイス 1 台・セッション 1 つが読めなくても、ほかは読む
    /// （読めなかったことは、デバイスとセッションごとに残す）。デバイスの一覧を取れないときだけ `Err`。
    unsafe fn list_endpoints(flow: Flow) -> windows::core::Result<Vec<Result<EndpointSessions, String>>> {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let flow = match flow {
            Flow::Capture => eCapture,
            Flow::Render => eRender,
        };
        let devices = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)?;
        let count = devices.GetCount()?;
        Ok((0..count).map(|index| endpoint_sessions(&devices, index)).collect())
    }

    unsafe fn endpoint_sessions(devices: &IMMDeviceCollection, index: u32) -> Result<EndpointSessions, String> {
        let device = devices.Item(index).map_err(|err| code(&err))?;
        let manager = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None).map_err(|err| code(&err))?;
        let list = manager.GetSessionEnumerator().map_err(|err| code(&err))?;
        let total = list.GetCount().map_err(|err| code(&err))?;
        let mut read = EndpointSessions::default();
        for at in 0..total {
            match session(&list, at) {
                Ok(Some(session)) => read.sessions.push(session),
                // システムの音のセッション（プロセスに結び付かない）
                Ok(None) => {}
                Err(err) => read.failed.push(err),
            }
        }
        Ok(read)
    }

    /// セッション 1 つ。システムの音のセッションは `None`。
    unsafe fn session(list: &IAudioSessionEnumerator, at: i32) -> Result<Option<MicSession>, String> {
        let control = list.GetSession(at).map_err(|err| code(&err))?;
        let control2 = control.cast::<IAudioSessionControl2>().map_err(|err| code(&err))?;
        // S_OK ならシステムの音のセッション、S_FALSE ならそうでない
        let system = control2.IsSystemSoundsSession();
        if system.is_err() {
            return Err(super::hresult(system.0));
        }
        if system.0 == 0 {
            return Ok(None);
        }
        let state = control.GetState().map_err(|err| code(&err))?;
        let state = if state == AudioSessionStateActive {
            SessionState::Active
        } else if state == AudioSessionStateExpired {
            SessionState::Expired
        } else {
            SessionState::Inactive
        };
        let pid = control2.GetProcessId().map_err(|err| code(&err))?;
        if pid == 0 {
            // プロセスに結び付かないセッションは、どのアプリか分からないので、読めなかったものとして数える
            return Err("pid=0".to_string());
        }
        let (exe_path, package) = process_identity(pid).map_err(|err| code(&err))?;
        Ok(Some(MicSession {
            pid,
            exe_path,
            package,
            state,
        }))
    }

    /// プロセスの実行ファイルのフルパスと、パッケージのアプリならパッケージファミリー名。
    fn process_identity(pid: u32) -> windows::core::Result<(String, Option<String>)> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::Storage::Packaging::Appx::GetPackageFamilyName;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return Err(windows::core::Error::from_thread());
            }
            let mut buf = [0u16; 1024];
            let mut size = buf.len() as u32;
            let path = if QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut size) != 0 {
                Ok(String::from_utf16_lossy(&buf[..size as usize]))
            } else {
                Err(windows::core::Error::from_thread())
            };
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            // パッケージのないプロセスでは APPMODEL_ERROR_NO_PACKAGE が返る
            let package = (GetPackageFamilyName(handle, &mut len, name.as_mut_ptr()) == 0)
                .then(|| super::package_name(&name[..(len as usize).min(name.len())]))
                .flatten();
            CloseHandle(handle);
            match package {
                Some(package) => Ok((path.unwrap_or_default(), Some(package))),
                None => Ok((path?, None)),
            }
        }
    }

    #[cfg(test)]
    mod tests {
        // [MTG-020]
        #[test]
        fn reading_sessions_does_not_fail() {
            // 実際のオーディオのセッションを読んでもパニックしない。録音のデバイスがない PC でも、一覧は取れる
            let sessions = super::read(super::Flow::Capture);
            assert!(sessions.available, "{:?}", sessions.error);
            let playback = super::read(super::Flow::Render);
            assert!(playback.available, "{:?}", playback.error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meetings::{detect, Detection, Observation, WindowInfo};

    fn session(exe_path: &str, package: Option<&str>, state: SessionState) -> MicSession {
        MicSession {
            pid: 100,
            exe_path: exe_path.into(),
            package: package.map(str::to_string),
            state,
        }
    }

    fn available(sessions: Vec<MicSession>) -> MicSessions {
        MicSessions { available: true, endpoints: 1, sessions, ..Default::default() }
    }

    /// セッションの一覧から判定した会議。
    fn detected(sessions: Vec<MicSession>, windows: Vec<WindowInfo>, own_exe: Option<&str>) -> Option<Detection> {
        let observation = Observation { mic_users: mic_users(&available(sessions)), windows };
        detect(&observation, own_exe)
    }

    const STORE_SLACK: &str = r"C:\Program Files\WindowsApps\91750D7E.Slack_4.47.69.0_x64__8she8kybcnzg4\app\Slack.exe";
    const STORE_SLACK_PACKAGE: &str = "91750D7E.Slack_8she8kybcnzg4";
    const MSIX_SLACK: &str = r"C:\Program Files\WindowsApps\com.tinyspeck.slackdesktop_4.47.69.0_x64__8yrtsj140pw4g\app\slack.exe";
    const MSIX_SLACK_PACKAGE: &str = "com.tinyspeck.slackdesktop_8yrtsj140pw4g";
    const TEAMS_DIR: &str = r"C:\Program Files\WindowsApps\MSTeams_25000.1.2.3_x64__8wekyb3d8bbwe";
    const TEAMS_PACKAGE: &str = "MSTeams_8wekyb3d8bbwe";

    /// Active のセッションのアプリを使用中とする。パッケージのアプリは、パッケージファミリー名でも判定できる。
    // [MTG-020][MTG-001]
    #[test]
    fn active_sessions_are_mic_users() {
        let sessions = available(vec![
            session(STORE_SLACK, Some(STORE_SLACK_PACKAGE), SessionState::Active),
            session(r"C:\Program Files\Google\Chrome\Application\chrome.exe", None, SessionState::Inactive),
            session(r"C:\Program Files\Google\Chrome\Application\chrome.exe", None, SessionState::Active),
            session(r"C:\Users\山田\AppData\Roaming\Zoom\bin\Zoom.exe", None, SessionState::Expired),
        ]);
        let users = mic_users(&sessions);
        assert_eq!(
            users,
            vec![
                MicUser { id: STORE_SLACK_PACKAGE.into(), in_use: true },
                MicUser { id: r"C:\Program Files\Google\Chrome\Application\chrome.exe".into(), in_use: true },
                MicUser { id: r"C:\Users\山田\AppData\Roaming\Zoom\bin\Zoom.exe".into(), in_use: false },
            ],
            "同じアプリのセッションは 1 つにまとめ、どれかが Active なら使用中"
        );
        let observation = Observation { mic_users: users, windows: vec![] };
        assert_eq!(detect(&observation, None).map(|d| d.app), Some(MeetingApp::SlackHuddle));
    }

    /// パッケージ名が会議アプリに当たらないパッケージのアプリは、実行ファイルの名前で判定する。
    /// パッケージ名が空なら、パッケージのアプリではないとする。
    // [MTG-020][MTG-001][MTG-003]
    #[test]
    fn session_id_prefers_a_matching_package_then_the_exe() {
        let teams = session(&format!(r"{TEAMS_DIR}\ms-teams.exe"), Some(TEAMS_PACKAGE), SessionState::Active);
        assert_eq!(teams.id(), TEAMS_PACKAGE);
        let packaged_zoom = session(r"C:\Program Files\WindowsApps\Zoom.Workplace_6.0.0.0_x64__abc\Zoom.exe", Some("Zoom.Workplace_abc"), SessionState::Active);
        assert_eq!(desktop_app(&packaged_zoom.id()), Some(MeetingApp::Zoom));
        let recorder = session(r"C:\Program Files\WindowsApps\Microsoft.WindowsSoundRecorder_1.0.0.0_x64__8wekyb3d8bbwe\SoundRec.exe", Some("Microsoft.WindowsSoundRecorder_8wekyb3d8bbwe"), SessionState::Active);
        assert_eq!(desktop_app(&recorder.id()), None, "会議アプリ以外は当たらない");
        assert_eq!(session("", Some("Contoso.App_x1"), SessionState::Active).id(), "Contoso.App_x1");
        assert_eq!(session("", None, SessionState::Active).id(), UNKNOWN);
        assert_eq!(session("", Some(""), SessionState::Active).id(), UNKNOWN, "空のパッケージ名は使わない");
        assert_eq!(session(r"C:\x\rec.exe", Some(""), SessionState::Active).id(), r"C:\x\rec.exe");
    }

    /// 会議アプリのパッケージに入っている更新・起動のプロセスと、ほかのアプリも使う部品は、パッケージファミリー名が
    /// 会議アプリに当たっても会議とみなさない。同じパッケージの会議アプリの本体は、会議とみなす。
    // [MTG-003][MTG-020][MTG-001]
    #[test]
    fn helper_processes_in_a_meeting_app_package_are_not_meetings() {
        let teams = |exe: &str| session(&format!(r"{TEAMS_DIR}\{exe}"), Some(TEAMS_PACKAGE), SessionState::Active);
        let store_slack = |exe: &str| {
            session(&format!(r"C:\Program Files\WindowsApps\91750D7E.Slack_4.47.69.0_x64__8she8kybcnzg4\app\{exe}"), Some(STORE_SLACK_PACKAGE), SessionState::Active)
        };
        let msix_slack = |exe: &str| {
            session(&format!(r"C:\Program Files\WindowsApps\com.tinyspeck.slackdesktop_4.47.69.0_x64__8yrtsj140pw4g\app\{exe}"), Some(MSIX_SLACK_PACKAGE), SessionState::Active)
        };
        let helpers = [
            teams("ms-teamsupdate.exe"),
            teams("msteams_autostarter.exe"),
            teams("msedgewebview2.exe"),
            teams("MSEdgeWebView2.EXE"),
            store_slack("Update.exe"),
            store_slack("msedgewebview2.exe"),
            msix_slack("update.exe"),
            session(r"C:\Program Files (x86)\Microsoft\EdgeWebView\Application\131.0.2903.112\msedgewebview2.exe", Some(TEAMS_PACKAGE), SessionState::Active),
            session(r"C:\Program Files\WindowsApps\Zoom.Workplace_6.0.0.0_x64__abc\zcscpthost.exe", Some("Zoom.Workplace_abc"), SessionState::Active),
            session(r"C:\Users\me\AppData\Local\slack\Update.exe", None, SessionState::Active),
        ];
        for helper in helpers {
            assert_eq!(desktop_app(&helper.id()), None, "{helper:?}");
            assert_eq!(detected(vec![helper.clone()], vec![], None), None, "{helper:?}");
        }
        for (main, app) in [
            (teams("ms-teams.exe"), MeetingApp::Teams),
            (teams("ms-teams_modulehost.exe"), MeetingApp::Teams),
            (store_slack("Slack.exe"), MeetingApp::SlackHuddle),
            (msix_slack("slack.exe"), MeetingApp::SlackHuddle),
            (session(r"C:\Users\me\AppData\Local\slack\app-4.47.69\slack.exe", None, SessionState::Active), MeetingApp::SlackHuddle),
        ] {
            assert_eq!(detected(vec![main.clone()], vec![], None), Some(Detection::desktop(app)), "{main:?}");
        }
        // 補助のプロセスと本体が同時にマイクを使っていれば、本体で会議とみなす。補助が Active でも、本体が Inactive なら会議とみなさない
        assert_eq!(
            detected(vec![teams("msedgewebview2.exe"), teams("ms-teams.exe")], vec![], None),
            Some(Detection::desktop(MeetingApp::Teams))
        );
        let mut idle = teams("ms-teams.exe");
        idle.state = SessionState::Inactive;
        assert_eq!(detected(vec![teams("ms-teamsupdate.exe"), idle], vec![], None), None);
    }

    /// 通常版・MSIX 版・Microsoft Store 版の Slack の録音のセッションは、どれもハドルとみなす。
    // [MTG-001][MTG-020]
    #[test]
    fn every_slack_edition_is_a_huddle() {
        for slack in [
            session(r"C:\Users\me\AppData\Local\slack\app-4.47.69\slack.exe", None, SessionState::Active),
            session(MSIX_SLACK, Some(MSIX_SLACK_PACKAGE), SessionState::Active),
            session(STORE_SLACK, Some(STORE_SLACK_PACKAGE), SessionState::Active),
            // 実行ファイルのパスを読めなくても、パッケージファミリー名で判定する
            session("", Some(MSIX_SLACK_PACKAGE), SessionState::Active),
        ] {
            assert_eq!(detected(vec![slack.clone()], vec![], None), Some(Detection::desktop(MeetingApp::SlackHuddle)), "{slack:?}");
        }
    }

    /// Kiroscribe 自身の録音のセッションは、会議の判定に使わない。
    // [MTG-005][MTG-020]
    #[test]
    fn own_session_is_excluded() {
        let own = r"C:\Users\me\AppData\Local\Kiroscribe\kiroscribe.exe";
        let sessions = vec![
            session(own, None, SessionState::Active),
            session(r"C:\Program Files\Google\Chrome\Application\chrome.exe", None, SessionState::Active),
        ];
        let gmail = vec![WindowInfo { exe: "chrome.exe".into(), title: "Gmail - Google Chrome".into() }];
        assert_eq!(detected(sessions.clone(), gmail, Some(own)), None);
        let with_slack = vec![sessions[0].clone(), session(STORE_SLACK, Some(STORE_SLACK_PACKAGE), SessionState::Active)];
        assert_eq!(detected(with_slack, vec![], Some(own)), Some(Detection::desktop(MeetingApp::SlackHuddle)));
    }

    /// パッケージのブラウザ（MSIX 版など）も、実行ファイルの名前でブラウザとみなし、タブの題で判定する。
    // [MTG-002][MTG-020]
    #[test]
    fn packaged_browsers_are_judged_by_tab_title() {
        let arc = session(r"C:\Program Files\WindowsApps\TheBrowserCompany.Arc_1.0.0.0_x64__ttt1ap7aakyb4\Arc.exe", Some("TheBrowserCompany.Arc_ttt1ap7aakyb4"), SessionState::Active);
        assert_eq!(arc.exe(), "arc.exe");
        let meet = vec![WindowInfo { exe: "Arc.exe".into(), title: "Meet - abc-defg-hij".into() }];
        assert_eq!(detected(vec![arc.clone()], meet, None), Some(Detection::in_browser(MeetingApp::GoogleMeet, "arc.exe")));
        let mail = vec![WindowInfo { exe: "arc.exe".into(), title: "受信トレイ".into() }];
        assert_eq!(detected(vec![arc.clone()], mail, None), None);
        let report = report(&available(vec![arc]), false);
        assert!(report.sessions[0].browser);
    }

    fn ok(sessions: Vec<MicSession>) -> Result<EndpointSessions, String> {
        Ok(EndpointSessions { sessions, failed: vec![] })
    }

    /// デバイスごとの読み取りの結果のまとめ方。デバイスがないときは、空の一覧を取れたとする。
    /// すべてのデバイス、またはすべてのセッションの情報を読めなかったときは、取れなかったとする（ConsentStore を使う）。
    /// 一部だけ読めなかったときは、読めたものを使い、読めなかった数と最初の理由を残す。
    // [MTG-020][LOG-005]
    #[test]
    fn summarize_tells_failures_from_an_empty_list() {
        let slack = session(STORE_SLACK, Some(STORE_SLACK_PACKAGE), SessionState::Active);

        let none = summarize(vec![]);
        assert!(none.available);
        assert_eq!((none.endpoints, none.status()), (0, ReadStatus::NoDevices));

        let quiet = summarize(vec![ok(vec![]), ok(vec![])]);
        assert!(quiet.available && quiet.sessions.is_empty() && quiet.error.is_none());
        assert_eq!((quiet.endpoints, quiet.status()), (2, ReadStatus::Ok), "どのアプリも録音していないのは、正常な空の一覧");

        let all_devices = summarize(vec![Err("0x88890004".into()), Err("0x80070005".into())]);
        assert!(!all_devices.available);
        assert_eq!((all_devices.failed_endpoints, all_devices.error.as_deref()), (2, Some("0x88890004")));
        assert_eq!(all_devices.status(), ReadStatus::Failed);

        let all_sessions = summarize(vec![Ok(EndpointSessions { sessions: vec![], failed: vec!["pid=0".into(), "0x8007001F".into()] })]);
        assert!(!all_sessions.available, "セッションがあるのに、どれも読めなかった");
        assert_eq!((all_sessions.failed_sessions, all_sessions.error.as_deref()), (2, Some("pid=0")));

        let partial = summarize(vec![
            Err("0x80070005".into()),
            Ok(EndpointSessions { sessions: vec![slack.clone()], failed: vec!["0x8007001F".into()] }),
        ]);
        assert!(partial.available);
        assert_eq!(partial.status(), ReadStatus::Partial);
        assert_eq!((partial.endpoints, partial.failed_endpoints, partial.failed_sessions), (2, 1, 1));
        assert_eq!(partial.error.as_deref(), Some("0x80070005"));
        assert_eq!(partial.sessions, vec![slack], "読めたセッションは残す");

        assert_eq!(hresult(0x8889_0004_u32 as i32), "0x88890004");
        assert_eq!(package_name(&[0x41, 0x5F, 0x62, 0, 0x7A]), Some("A_b".into()));
        assert_eq!(package_name(&[0, 0x41]), None, "空のパッケージ名はパッケージのアプリでない");
        assert_eq!(package_name(&[]), None);
    }

    /// ログに書くのは、実行ファイルの名前・パッケージファミリー名・状態・会議アプリとして当たるか・ブラウザかだけ。
    /// フルパス（利用者の名前を含む）とプロセス ID は書かない。会議アプリ・ブラウザ・Active のセッションを先に並べる。
    // [LOG-005]
    #[test]
    fn report_lists_sessions_without_full_paths() {
        let sessions = available(vec![
            session(r"C:\Users\山田 太郎\AppData\Local\slack\app-4.47.69\slack.exe", None, SessionState::Inactive),
            session(STORE_SLACK, Some(STORE_SLACK_PACKAGE), SessionState::Active),
            session(r"C:\Program Files\Google\Chrome\Application\chrome.exe", None, SessionState::Active),
            session(r"C:\Windows\System32\rec.exe", None, SessionState::Active),
        ]);
        let report = report(&sessions, false);
        assert!(report.available);
        assert_eq!((report.endpoints, report.total, report.status), (1, 4, ReadStatus::Ok));
        let rows: Vec<(&str, Option<&str>, SessionState, Option<MeetingApp>, bool)> = report
            .sessions
            .iter()
            .map(|s| (s.exe.as_str(), s.package.as_deref(), s.state, s.app, s.browser))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("slack.exe", Some(STORE_SLACK_PACKAGE), SessionState::Active, Some(MeetingApp::SlackHuddle), false),
                ("chrome.exe", None, SessionState::Active, None, true),
                ("rec.exe", None, SessionState::Active, None, false),
                ("slack.exe", None, SessionState::Inactive, Some(MeetingApp::SlackHuddle), false),
            ]
        );
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("山田") && !json.contains("WindowsApps") && !json.contains("pid"), "{json}");
        assert!(!json.contains("error") && !json.contains("failed"), "取れたときは理由と失敗の数を書かない: {json}");
        assert!(json.contains(r#""state":"active""#) && json.contains(r#""status":"ok""#), "{json}");
        assert!(json.contains(r#"{"exe":"rec.exe","state":"active"}"#), "ないものは書かない: {json}");

        let failed = super::report(&MicSessions { error: Some("0x80070005".into()), ..Default::default() }, false);
        assert!(!failed.available);
        assert_eq!((failed.status, failed.error.as_deref()), (ReadStatus::Failed, Some("0x80070005")));

        let partial = summarize(vec![Err("0x80070005".into()), ok(vec![])]);
        let json = serde_json::to_value(super::report(&partial, false)).unwrap();
        assert_eq!(json["status"], "partial");
        assert_eq!((&json["failedEndpoints"], &json["error"]), (&serde_json::json!(1), &serde_json::json!("0x80070005")));
    }

    /// ふだんのログには、判定に関わらないアプリ（会議アプリでもブラウザでもない、Inactive・Expired のもの）を書かない。
    /// 書く件数には上限があり、Active のセッションを優先する。確認用の道具（`all`）では、すべて書く。
    // [LOG-005]
    #[test]
    fn normal_report_omits_unrelated_idle_sessions() {
        let mut list: Vec<MicSession> = (0..20)
            .map(|i| session(&format!(r"C:\Apps\tool{i}.exe"), None, if i % 2 == 0 { SessionState::Inactive } else { SessionState::Expired }))
            .collect();
        list.extend((0..12).map(|i| session(&format!(r"C:\Apps\rec{i:02}.exe"), None, SessionState::Active)));
        list.push(session(r"C:\Zoom\bin\Zoom.exe", None, SessionState::Expired));
        list.push(session(STORE_SLACK, Some(STORE_SLACK_PACKAGE), SessionState::Active));
        let sessions = available(list);

        let normal = report(&sessions, false);
        assert_eq!(normal.total, 34);
        assert_eq!(normal.sessions.len(), MAX_LOGGED_SESSIONS);
        assert_eq!(normal.sessions[0].app, Some(MeetingApp::SlackHuddle));
        assert!(normal.sessions.iter().all(|s| s.state == SessionState::Active), "{:?}", normal.sessions);
        assert!(!normal.sessions.iter().any(|s| s.exe.starts_with("tool")));

        assert_eq!(report(&sessions, true).sessions.len(), 34);
    }

    /// Windows 以外では、オーディオのセッションは取れない。
    // [MTG-021]
    #[cfg(not(windows))]
    #[test]
    fn sessions_are_unavailable_outside_windows() {
        assert_eq!(read(), MicSessions::default());
        assert_eq!(read_playback(), MicSessions::default());
    }

    /// オーディオのセッションから読むのは、プロセスと状態だけ。音声と音量を読む部品は使わない。
    // [SEC-013]
    #[test]
    fn reads_no_audio_or_levels() {
        let source = include_str!("mic_sessions.rs").replace("\r\n", "\n");
        let code = &source[..source.find("\n#[cfg(test)]\nmod tests").expect("テストの手前まで")];
        for forbidden in [
            // 音声のデータを読む・書く部品
            "IAudioClient",
            "IAudioCaptureClient",
            "IAudioRenderClient",
            "ActivateAudioInterfaceAsync",
            "GetBuffer",
            // 音量・音の大きさを読む部品
            "IAudioMeterInformation",
            "GetPeakValue",
            "ISimpleAudioVolume",
            "IChannelAudioVolume",
            "IAudioStreamVolume",
            "IAudioEndpointVolume",
            "GetMasterVolume",
            // 音声を扱うほかの部品と、外部への送信
            "cpal",
            "crate::audio::",
            "reqwest",
            "std::net",
        ] {
            assert!(!code.contains(forbidden), "オーディオのセッションの読み取りが {forbidden} を使っている");
        }
    }
}
