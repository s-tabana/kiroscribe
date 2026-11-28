//! Windows のオーディオのセッションから、録音・再生しているアプリを読む。
//!
//! Windows は、録音・再生のデバイスごとに、アプリのセッションを持っている。
//! セッションの状態が Active なら、そのプロセスはいま録音（再生）している。
//! 会議の検出は、これを判定表の録音・再生の印（[`crate::meeting_rules`]）に使う。
//!
//! 読むのは、セッションのプロセス ID と状態と、再生のセッションの音の大きさの数値（ピーク）だけ。音声そのものは読まない
//! （SEC-013）。ログには、実行ファイルの名前・パッケージファミリー名・状態・音が鳴っているかだけを書き、フルパスは書かない。

use serde::{Deserialize, Serialize};

use crate::meetings::exe_name;

/// セッションの状態（`AudioSessionState`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SessionState {
    /// いま録音（再生）している
    Active,
    /// ストリームはあるが、止まっている
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

/// 録音か再生か。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Flow {
    /// 録音（マイク）
    Capture,
    /// 再生（スピーカー）
    Render,
}

impl Flow {
    pub fn label(self) -> &'static str {
        match self {
            Flow::Capture => "録音",
            Flow::Render => "再生",
        }
    }
}

/// オーディオのセッション 1 件。
#[derive(Debug, Clone, PartialEq)]
pub struct AudioSession {
    pub pid: u32,
    /// 実行ファイルのフルパス（読めなかったときは空）
    pub exe_path: String,
    /// パッケージのアプリ（Microsoft Store 版・MSIX 版）なら、パッケージファミリー名
    pub package: Option<String>,
    pub state: SessionState,
    /// 音の大きさ（IAudioMeterInformation のピーク。0.0〜1.0）。読んでいなければ `None`
    pub peak: Option<f32>,
}

/// 実行ファイルのパスもパッケージ名も読めなかったプロセスの名前。
pub const UNKNOWN: &str = "unknown";

/// パッケージファミリー名（小文字）の先頭と、そのアプリの実行ファイルの名前。実行ファイルのパスを読めないときに使う。
const PACKAGE_PREFIXES: &[(&str, &str)] = &[
    // Microsoft Store 版の Slack（https://slack.com/help/articles/360048367814 ）
    ("91750d7e.slack_", "slack.exe"),
    // MSIX 版の Slack（https://slack.com/help/articles/212475728 ）
    ("com.tinyspeck.slackdesktop_", "slack.exe"),
    ("msteams_", "ms-teams.exe"),
    ("microsoftteams_", "ms-teams.exe"),
];

/// パッケージファミリー名から、そのアプリの実行ファイルの名前を決める（判定表の対象のアプリだけ）。
pub fn packaged_exe(package: &str) -> Option<&'static str> {
    let lower = package.to_lowercase();
    PACKAGE_PREFIXES.iter().find_map(|(prefix, exe)| {
        let rest = lower.strip_prefix(prefix)?;
        (!rest.is_empty()).then_some(*exe)
    })
}

impl AudioSession {
    /// ログに書く実行ファイルの名前（小文字。フルパスは書かない）。読めなければ [`UNKNOWN`]。
    pub fn exe(&self) -> String {
        if self.exe_path.is_empty() {
            UNKNOWN.to_string()
        } else {
            exe_name(&self.exe_path)
        }
    }

    /// 判定表の `apps` と照らす名前: 実行ファイルの名前。読めなければ、パッケージファミリー名から決めた名前
    /// （Slack・Teams だけ）。どちらでもなければ [`UNKNOWN`]。
    pub fn app_exe(&self) -> String {
        if !self.exe_path.is_empty() {
            return exe_name(&self.exe_path);
        }
        self.package
            .as_deref()
            .and_then(packaged_exe)
            .map(str::to_string)
            .unwrap_or_else(|| UNKNOWN.to_string())
    }

    /// 再生の音が鳴っているか（状態が Active で、音の大きさが `floor` 以上）。
    pub fn audible(&self, floor: f32) -> bool {
        self.state == SessionState::Active && self.peak.is_some_and(|peak| peak >= floor)
    }
}

/// オーディオのセッションの一覧を読んだ結果（録音か再生の、すべての有効なデバイス）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioSessions {
    /// 一覧を取れたか（[`summarize`] の決まりで決める）
    pub available: bool,
    /// 有効なデバイスの数
    pub endpoints: usize,
    /// セッションを読めなかったデバイスの数
    pub failed_endpoints: usize,
    /// 情報（状態・プロセス ID など）を読めなかったセッションの数
    pub failed_sessions: usize,
    pub sessions: Vec<AudioSession>,
    /// 取れなかった理由か、一部を読めなかったときの最初の理由（HRESULT。ログに書く）
    pub error: Option<String>,
}

/// 一覧の読み取りの結果の区分（ログに書く）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ReadStatus {
    /// すべてのデバイスとセッションを読めた
    Ok,
    /// 有効なデバイスがない（一覧は取れていて、空）
    NoDevices,
    /// 一部のデバイスかセッションを読めなかった（読めたものを使う）
    Partial,
    /// 一覧を取れなかった（録音・再生の印は立たない）
    Failed,
}

impl AudioSessions {
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

/// デバイス 1 台から読んだセッション。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EndpointSessions {
    pub sessions: Vec<AudioSession>,
    /// 情報を読めなかったセッションごとの理由（HRESULT）
    pub failed: Vec<String>,
}

/// デバイスごとの読み取りの結果をまとめる（`Err` はセッションを読めなかったデバイスと、その理由）。
/// - デバイスがない: 一覧は取れた（空）とする
/// - すべてのデバイスを読めなかった、またはセッションがあるのに、どのセッションの情報も読めなかった: 取れなかったとする
/// - 一部だけ読めなかった: 読めたものを使い、読めなかった数と最初の理由を残す
pub fn summarize(endpoints: Vec<Result<EndpointSessions, String>>) -> AudioSessions {
    let mut result = AudioSessions { endpoints: endpoints.len(), ..Default::default() };
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

/// ログに書く、セッション 1 件。ないもの（パッケージ・音の有無）は書かない（値の数の上限の中で、多くのセッションを残すため）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionReport {
    /// 実行ファイルの名前（小文字）
    pub exe: String,
    pub state: SessionState,
    /// パッケージファミリー名
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// 再生の音が鳴っているか（再生のセッションで、音の大きさを読んだものだけ）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audible: Option<bool>,
}

/// ログに書く、オーディオのセッションの様子。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionsReport {
    pub status: ReadStatus,
    pub endpoints: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub failed_endpoints: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub failed_sessions: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// セッションの数（一覧に書かなかったものも含む）
    pub total: usize,
    pub sessions: Vec<SessionReport>,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// ふだんのログの一覧に書く、セッションの数の上限。
pub const MAX_LOGGED_SESSIONS: usize = 8;

/// セッションの一覧をログ用に整える。Active を先に並べる。`target` が true を返すアプリ（判定表の対象のアプリ）の
/// セッションだけを、[`MAX_LOGGED_SESSIONS`] 件まで書く（`all` なら、すべてのセッションを書く。確認用の道具）。
pub fn report(sessions: &AudioSessions, target: &dyn Fn(&str) -> bool, audible_peak: f32, all: bool) -> SessionsReport {
    let mut list: Vec<SessionReport> = sessions
        .sessions
        .iter()
        .filter(|session| all || target(&session.app_exe()))
        .map(|session| SessionReport {
            exe: session.exe(),
            package: session.package.clone().filter(|package| !package.is_empty()),
            state: session.state,
            audible: session.peak.map(|_| session.audible(audible_peak)),
        })
        .collect();
    list.sort_by(|a, b| (a.state, &a.exe, &a.package).cmp(&(b.state, &b.exe, &b.package)));
    if !all {
        list.dedup();
        list.truncate(MAX_LOGGED_SESSIONS);
    }
    SessionsReport {
        status: sessions.status(),
        endpoints: sessions.endpoints,
        failed_endpoints: sessions.failed_endpoints,
        failed_sessions: sessions.failed_sessions,
        error: sessions.error.clone(),
        total: sessions.sessions.len(),
        sessions: list,
    }
}

/// いまのオーディオのセッションの一覧を読む。`peak_for` が true を返すアプリ（実行ファイルの名前）の、
/// 再生の Active のセッションだけ、音の大きさも読む（約 0.3 秒かかる）。Windows 以外では、取れない（`available` が false）。
pub fn read(flow: Flow, peak_for: &dyn Fn(&str) -> bool) -> AudioSessions {
    #[cfg(windows)]
    {
        windows_sessions::read(flow, peak_for)
    }
    #[cfg(not(windows))]
    {
        let _ = (flow, peak_for);
        AudioSessions::default()
    }
}

#[cfg(windows)]
mod windows_sessions {
    use windows::core::Interface;
    use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
    use windows::Win32::Media::Audio::{
        eCapture, eRender, AudioSessionStateActive, AudioSessionStateExpired, IAudioSessionControl2,
        IAudioSessionEnumerator, IAudioSessionManager2, IMMDeviceCollection, IMMDeviceEnumerator, MMDeviceEnumerator,
        DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED};

    use super::{AudioSession, AudioSessions, EndpointSessions, Flow, SessionState};

    /// 音の大きさを何回か読んで、いちばん大きい値にする（1 回だけだと、話している途中の一瞬の無音を拾うため）。
    const PEAK_SAMPLES: usize = 10;
    const PEAK_SAMPLE_GAP: std::time::Duration = std::time::Duration::from_millis(30);

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

    pub fn read(flow: Flow, peak_for: &dyn Fn(&str) -> bool) -> AudioSessions {
        let _com = Com::init();
        let mut meters = Vec::new();
        let mut result = match unsafe { list_endpoints(flow, peak_for, &mut meters) } {
            Ok(endpoints) => super::summarize(endpoints),
            Err(err) => {
                return AudioSessions { available: false, error: Some(code(&err)), ..Default::default() };
            }
        };
        if !meters.is_empty() {
            let mut peaks: Vec<Option<f32>> = vec![None; meters.len()];
            for round in 0..PEAK_SAMPLES {
                if round > 0 {
                    std::thread::sleep(PEAK_SAMPLE_GAP);
                }
                for (slot, (_, meter)) in peaks.iter_mut().zip(&meters) {
                    if let Ok(value) = unsafe { meter.GetPeakValue() } {
                        *slot = Some(slot.map_or(value, |v: f32| v.max(value)));
                    }
                }
            }
            for ((at, _), peak) in meters.iter().zip(peaks) {
                if let Some(session) = result.sessions.get_mut(*at) {
                    session.peak = Some(peak.unwrap_or(0.0));
                }
            }
        }
        result
    }

    fn code(err: &windows::core::Error) -> String {
        super::hresult(err.code().0)
    }

    /// 音の大きさを読むセッション（まとめた一覧の中の位置）と、そのメーター。
    type Meters = Vec<(usize, IAudioMeterInformation)>;

    /// すべての有効なデバイスの、セッションを読む。デバイス 1 台・セッション 1 つが読めなくても、ほかは読む
    /// （読めなかったことは、デバイスとセッションごとに残す）。デバイスの一覧を取れないときだけ `Err`。
    unsafe fn list_endpoints(
        flow: Flow,
        peak_for: &dyn Fn(&str) -> bool,
        meters: &mut Meters,
    ) -> windows::core::Result<Vec<Result<EndpointSessions, String>>> {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let data_flow = match flow {
            Flow::Capture => eCapture,
            Flow::Render => eRender,
        };
        let devices = enumerator.EnumAudioEndpoints(data_flow, DEVICE_STATE_ACTIVE)?;
        let count = devices.GetCount()?;
        let peaks = |session: &AudioSession| flow == Flow::Render && session.state == SessionState::Active && peak_for(&session.app_exe());
        // まとめた一覧（summarize）では、読めたデバイスのセッションが、この順に並ぶ
        let mut listed = 0;
        Ok((0..count).map(|index| endpoint_sessions(&devices, index, &peaks, meters, &mut listed)).collect())
    }

    unsafe fn endpoint_sessions(
        devices: &IMMDeviceCollection,
        index: u32,
        peaks: &dyn Fn(&AudioSession) -> bool,
        meters: &mut Meters,
        listed: &mut usize,
    ) -> Result<EndpointSessions, String> {
        let device = devices.Item(index).map_err(|err| code(&err))?;
        let manager = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None).map_err(|err| code(&err))?;
        let list = manager.GetSessionEnumerator().map_err(|err| code(&err))?;
        let total = list.GetCount().map_err(|err| code(&err))?;
        let mut read = EndpointSessions::default();
        for at in 0..total {
            match session(&list, at) {
                Ok(Some((session, control))) => {
                    if peaks(&session) {
                        if let Ok(meter) = control.cast::<IAudioMeterInformation>() {
                            meters.push((*listed, meter));
                        }
                    }
                    read.sessions.push(session);
                    *listed += 1;
                }
                // システムの音のセッション（プロセスに結び付かない）
                Ok(None) => {}
                Err(err) => read.failed.push(err),
            }
        }
        Ok(read)
    }

    /// セッション 1 つ。システムの音のセッションは `None`。
    unsafe fn session(list: &IAudioSessionEnumerator, at: i32) -> Result<Option<(AudioSession, IAudioSessionControl2)>, String> {
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
        Ok(Some((AudioSession { pid, exe_path, package, state, peak: None }, control2)))
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
            // 実際のオーディオのセッションを読んでもパニックしない。デバイスがない PC でも、一覧は取れる
            let capture = super::read(super::Flow::Capture, &|_| true);
            assert!(capture.available, "{:?}", capture.error);
            let render = super::read(super::Flow::Render, &|_| true);
            assert!(render.available, "{:?}", render.error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(exe_path: &str, package: Option<&str>, state: SessionState, peak: Option<f32>) -> AudioSession {
        AudioSession { pid: 100, exe_path: exe_path.into(), package: package.map(str::to_string), state, peak }
    }

    const STORE_SLACK_PACKAGE: &str = "91750D7E.Slack_8she8kybcnzg4";

    /// 判定表と照らす名前は、実行ファイルの名前。読めないときは、Slack・Teams のパッケージファミリー名から決める。
    /// 似ているだけのパッケージ名は、当てない。
    // [MTG-020][MTG-001][MTG-003]
    #[test]
    fn app_exe_is_the_exe_name_or_a_known_package() {
        let store_slack = session(
            r"C:\Program Files\WindowsApps\91750D7E.Slack_4.47.69.0_x64__8she8kybcnzg4\app\Slack.exe",
            Some(STORE_SLACK_PACKAGE),
            SessionState::Active,
            None,
        );
        assert_eq!(store_slack.app_exe(), "slack.exe");
        assert_eq!(session("", Some(STORE_SLACK_PACKAGE), SessionState::Active, None).app_exe(), "slack.exe");
        assert_eq!(session("", Some("com.tinyspeck.slackdesktop_8yrtsj140pw4g"), SessionState::Active, None).app_exe(), "slack.exe");
        assert_eq!(session("", Some("MSTeams_8wekyb3d8bbwe"), SessionState::Active, None).app_exe(), "ms-teams.exe");
        assert_eq!(session("", Some("MSTeams_"), SessionState::Active, None).app_exe(), UNKNOWN, "名前の続きがない");
        assert_eq!(session("", Some("MSTeamsHelper_8wekyb3d8bbwe"), SessionState::Active, None).app_exe(), UNKNOWN);
        assert_eq!(session("", None, SessionState::Active, None).app_exe(), UNKNOWN);
        // Teams のパッケージに入っている補助のプロセスは、実行ファイルの名前のまま（判定表の対象にならない）
        let webview = session(
            r"C:\Program Files\WindowsApps\MSTeams_25000.1.2.3_x64__8wekyb3d8bbwe\msedgewebview2.exe",
            Some("MSTeams_8wekyb3d8bbwe"),
            SessionState::Active,
            None,
        );
        assert_eq!(webview.app_exe(), "msedgewebview2.exe");
        assert_eq!(webview.exe(), "msedgewebview2.exe");
    }

    /// 再生の音が鳴っているのは、Active で、音の大きさが下限以上のときだけ。
    // [MTG-020]
    #[test]
    fn audible_needs_an_active_session_above_the_floor() {
        assert!(session("a.exe", None, SessionState::Active, Some(0.05)).audible(0.01));
        assert!(session("a.exe", None, SessionState::Active, Some(0.01)).audible(0.01));
        assert!(!session("a.exe", None, SessionState::Active, Some(0.001)).audible(0.01), "ほぼ無音");
        assert!(!session("a.exe", None, SessionState::Inactive, Some(0.5)).audible(0.01));
        assert!(!session("a.exe", None, SessionState::Active, None).audible(0.01), "読んでいない");
    }

    fn ok(sessions: Vec<AudioSession>) -> Result<EndpointSessions, String> {
        Ok(EndpointSessions { sessions, failed: vec![] })
    }

    /// 一覧を取れた・取れなかった・一部だけ読めなかったを分け、読めなかった数と最初の理由を残す。
    // [MTG-020][LOG-005]
    #[test]
    fn summarize_tells_failures_from_an_empty_list() {
        let chrome = session(r"C:\x\chrome.exe", None, SessionState::Active, None);
        let empty = summarize(vec![]);
        assert!(empty.available && empty.status() == ReadStatus::NoDevices);
        let all_failed = summarize(vec![Err("0x88890004".into())]);
        assert!(!all_failed.available);
        assert_eq!((all_failed.status(), all_failed.error.as_deref()), (ReadStatus::Failed, Some("0x88890004")));
        let no_info = summarize(vec![Ok(EndpointSessions { sessions: vec![], failed: vec!["0x8007001F".into()] })]);
        assert_eq!(no_info.status(), ReadStatus::Failed);
        let partial = summarize(vec![Err("0x80070005".into()), ok(vec![chrome.clone()])]);
        assert_eq!((partial.status(), partial.failed_endpoints, partial.sessions.len()), (ReadStatus::Partial, 1, 1));
        let fine = summarize(vec![ok(vec![chrome])]);
        assert_eq!(fine.status(), ReadStatus::Ok);
    }

    /// ログには、対象のアプリのセッションだけを、実行ファイルの名前・パッケージファミリー名・状態・音の有無で書く。
    /// フルパス（利用者の名前を含む）は書かない。
    // [LOG-005]
    #[test]
    fn report_lists_target_sessions_without_full_paths() {
        let sessions = summarize(vec![ok(vec![
            session(r"C:\Users\山田\AppData\Local\slack\slack.exe", None, SessionState::Inactive, None),
            session(r"C:\Program Files\Google\Chrome\Application\chrome.exe", None, SessionState::Active, Some(0.2)),
            session(r"C:\Program Files\Mozilla Thunderbird\thunderbird.exe", None, SessionState::Active, Some(0.5)),
            session("", Some("MSTeams_8wekyb3d8bbwe"), SessionState::Active, None),
        ])]);
        let target = |exe: &str| ["slack.exe", "chrome.exe", "ms-teams.exe"].contains(&exe);
        let report = report(&sessions, &target, 0.01, false);
        assert_eq!(report.total, 4);
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(
            json["sessions"],
            serde_json::json!([
                { "exe": "chrome.exe", "state": "active", "audible": true },
                { "exe": "unknown", "state": "active", "package": "MSTeams_8wekyb3d8bbwe" },
                { "exe": "slack.exe", "state": "inactive" },
            ])
        );
        assert!(!json.to_string().contains("山田"));
        assert_eq!(super::report(&sessions, &target, 0.01, true).sessions.len(), 4, "確認用の道具には、すべて書く");
    }

    // [MTG-021]
    #[cfg(not(windows))]
    #[test]
    fn sessions_are_unavailable_outside_windows() {
        assert!(!read(Flow::Capture, &|_| true).available);
        assert!(!read(Flow::Render, &|_| true).available);
    }

    /// 音声そのものを読む部品は使わない。音の大きさの数値（IAudioMeterInformation のピーク）だけを読む。
    // [SEC-013]
    #[test]
    fn reads_no_audio_itself() {
        let source = include_str!("audio_sessions.rs").replace("\r\n", "\n");
        let code = &source[..source.find("\n#[cfg(test)]\nmod tests").expect("テストの手前まで")];
        for forbidden in ["IAudioCaptureClient", "IAudioClient", "IAudioRenderClient", "crate::audio::", "cpal"] {
            assert!(!code.contains(forbidden), "オーディオのセッションの読み取りが {forbidden} を使っている");
        }
    }
}
