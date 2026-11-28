//! 会議中を示す手がかりの、Windows での読み取り。
//!
//! - オーディオのセッション: 録音・再生の端点ごとに、セッションの状態と音の大きさ（IAudioMeterInformation のピーク）
//! - 窓: 会議アプリとブラウザのトップレベルの窓（プロセス・クラス名・見えているか・大きさ・最小化か・名前）
//! - 部品: UI Automation で、窓の中のボタン類の名前と種類
//! - カメラ: ConsentStore の webcam（アプリごとの使用開始・終了の時刻）
//!
//! 音声そのものは読まない。音の大きさは数値だけ。

use std::collections::HashMap;
use std::time::Instant;

use kiroscribe_core::meetings::exe_name;
use kiroscribe_core::mic_sessions::{hresult, package_name, SessionState};
use kiroscribe_core::mic_store::MicStore;
use windows::core::{Interface, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::HWND;
use windows::Win32::Media::Audio::Endpoints::IAudioMeterInformation;
use windows::Win32::Media::Audio::{
    eCapture, eCommunications, eConsole, eRender, AudioSessionStateActive, AudioSessionStateExpired, IAudioSessionControl,
    IAudioSessionControl2, IAudioSessionManager2, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    STGM_READ,
};
use windows::Win32::System::Variant::{VARIANT, VT_I4};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomation2, IUIAutomationCacheRequest, IUIAutomationCondition, TreeScope_Descendants,
    UIA_ButtonControlTypeId, UIA_CheckBoxControlTypeId, UIA_ControlTypePropertyId, UIA_MenuItemControlTypeId,
    UIA_NamePropertyId, UIA_SplitButtonControlTypeId, UIA_CONTROLTYPE_ID,
};

use crate::signals::{
    control_category, is_target_exe, AudioSession, AudioSnapshot, Control, Endpoint, Flow, UiaScan, UiaWindow,
    WindowSnapshot,
};

/// このスレッドで COM を使えるようにし、終わったら戻す（初期化できたときだけ）。
pub struct Com(bool);

impl Com {
    pub fn init() -> Self {
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

fn code(err: &windows::core::Error) -> String {
    hresult(err.code().0)
}

/// COM が確保した文字列を String にして解放する。
unsafe fn take_string(text: PWSTR) -> String {
    let value = text.to_string().unwrap_or_default();
    CoTaskMemFree(Some(text.0 as *const _));
    value
}

/// ピークを何回か読んで、いちばん大きい値にする（1 回だけだと、話している途中の一瞬の無音を拾うため）。
const PEAK_SAMPLES: usize = 10;
const PEAK_SAMPLE_GAP: std::time::Duration = std::time::Duration::from_millis(30);

/// 録音か再生の、すべての有効な端点のセッションと、その音の大きさ。
pub fn read_audio(flow: Flow, names: bool) -> AudioSnapshot {
    let _com = Com::init();
    let mut snapshot =
        AudioSnapshot { flow, endpoints: Vec::new(), sessions: Vec::new(), error: None, failures: 0, first_failure: None };
    let mut meters: Vec<(usize, IAudioMeterInformation)> = Vec::new();
    if let Err(err) = unsafe { list(flow, names, &mut snapshot, &mut meters) } {
        snapshot.error = Some(code(&err));
        return snapshot;
    }
    // 読めたメーターを、少しずつ間を空けて何回か読む
    let mut peaks: Vec<Option<f32>> = vec![None; snapshot.sessions.len()];
    for round in 0..PEAK_SAMPLES {
        if round > 0 {
            std::thread::sleep(PEAK_SAMPLE_GAP);
        }
        for (at, meter) in &meters {
            if let Ok(value) = unsafe { meter.GetPeakValue() } {
                let slot = &mut peaks[*at];
                *slot = Some(slot.map_or(value, |v: f32| v.max(value)));
            }
        }
    }
    for (session, peak) in snapshot.sessions.iter_mut().zip(peaks) {
        session.peak = peak;
    }
    snapshot
}

fn fail(snapshot: &mut AudioSnapshot, reason: String) {
    snapshot.failures += 1;
    snapshot.first_failure.get_or_insert(reason);
}

unsafe fn device_id(device: &IMMDevice) -> Option<String> {
    device.GetId().ok().map(|id| take_string(id))
}

unsafe fn device_name(device: &IMMDevice) -> Option<String> {
    let store = device.OpenPropertyStore(STGM_READ).ok()?;
    let mut value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
    let name = PropVariantToStringAlloc(&value).ok().map(|text| take_string(text));
    let _ = PropVariantClear(&mut value);
    name
}

unsafe fn list(
    flow: Flow,
    names: bool,
    snapshot: &mut AudioSnapshot,
    meters: &mut Vec<(usize, IAudioMeterInformation)>,
) -> windows::core::Result<()> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let data_flow = match flow {
        Flow::Capture => eCapture,
        Flow::Render => eRender,
    };
    let default_of = |role| enumerator.GetDefaultAudioEndpoint(data_flow, role).ok().and_then(|d| device_id(&d));
    let console = default_of(eConsole);
    let communications = default_of(eCommunications);
    let devices = enumerator.EnumAudioEndpoints(data_flow, DEVICE_STATE_ACTIVE)?;
    let mut identities = HashMap::new();
    for at in 0..devices.GetCount()? {
        let index = at as usize + 1;
        let device = match devices.Item(at) {
            Ok(device) => device,
            Err(err) => {
                fail(snapshot, code(&err));
                continue;
            }
        };
        let id = device_id(&device);
        snapshot.endpoints.push(Endpoint {
            index,
            default_console: id.is_some() && id == console,
            default_communications: id.is_some() && id == communications,
            name: if names { device_name(&device) } else { None },
        });
        let list = match device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None).and_then(|m| m.GetSessionEnumerator())
        {
            Ok(list) => list,
            Err(err) => {
                fail(snapshot, code(&err));
                continue;
            }
        };
        for session_at in 0..list.GetCount().unwrap_or(0) {
            match list.GetSession(session_at).map_err(|err| code(&err)).and_then(|c| session(&c, index, &mut identities)) {
                Ok(Some((session, meter))) => {
                    if let Some(meter) = meter {
                        meters.push((snapshot.sessions.len(), meter));
                    }
                    snapshot.sessions.push(session);
                }
                Ok(None) => {}
                Err(reason) => fail(snapshot, reason),
            }
        }
    }
    Ok(())
}

/// セッション 1 つと、その音の大きさのメーター。システムの音のセッションは `None`。
unsafe fn session(
    control: &IAudioSessionControl,
    endpoint: usize,
    identities: &mut HashMap<u32, (String, Option<String>)>,
) -> Result<Option<(AudioSession, Option<IAudioMeterInformation>)>, String> {
    let control2 = control.cast::<IAudioSessionControl2>().map_err(|err| code(&err))?;
    // S_OK ならシステムの音のセッション、S_FALSE ならそうでない
    let system = control2.IsSystemSoundsSession();
    if system.is_err() {
        return Err(hresult(system.0));
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
    let (exe, package) = identities.entry(pid).or_insert_with(|| process_identity(pid)).clone();
    let meter = control.cast::<IAudioMeterInformation>().ok();
    Ok(Some((AudioSession { endpoint, pid, exe, package, state, peak: None }, meter)))
}

/// プロセスの実行ファイルの名前（小文字。読めなければ unknown）と、パッケージのアプリならパッケージファミリー名。
fn process_identity(pid: u32) -> (String, Option<String>) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Storage::Packaging::Appx::GetPackageFamilyName;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return (kiroscribe_core::mic_sessions::UNKNOWN.into(), None);
        }
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let exe = if QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut size) != 0 {
            exe_name(&String::from_utf16_lossy(&buf[..size as usize]))
        } else {
            kiroscribe_core::mic_sessions::UNKNOWN.into()
        };
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        let package = (GetPackageFamilyName(handle, &mut len, name.as_mut_ptr()) == 0)
            .then(|| package_name(&name[..(len as usize).min(name.len())]))
            .flatten();
        CloseHandle(handle);
        (exe, package)
    }
}

/// 会議アプリとブラウザの、トップレベルの窓。見えない窓は、名前のあるものだけ（名前のない裏の窓は数が多いため）。
pub fn list_windows() -> Vec<WindowSnapshot> {
    use windows_sys::Win32::UI::WindowsAndMessaging::EnumWindows;
    let mut handles: Vec<usize> = Vec::new();
    unsafe extern "system" fn collect(
        hwnd: windows_sys::Win32::Foundation::HWND,
        lparam: windows_sys::Win32::Foundation::LPARAM,
    ) -> windows_sys::core::BOOL {
        let list = &mut *(lparam as *mut Vec<usize>);
        list.push(hwnd as usize);
        1
    }
    unsafe {
        EnumWindows(Some(collect), &mut handles as *mut _ as isize);
    }
    let mut names: HashMap<u32, String> = HashMap::new();
    handles
        .into_iter()
        .filter_map(|hwnd| unsafe { window(hwnd, &mut names) })
        .collect()
}

fn utf16(buf: &[u16], len: i32) -> String {
    if len <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())])
    }
}

unsafe fn window(hwnd: usize, names: &mut HashMap<u32, String>) -> Option<WindowSnapshot> {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
        IsWindowVisible, IsZoomed,
    };
    let handle = hwnd as windows_sys::Win32::Foundation::HWND;
    let mut pid = 0u32;
    GetWindowThreadProcessId(handle, &mut pid);
    let exe = names.entry(pid).or_insert_with(|| process_identity(pid).0).clone();
    if !is_target_exe(&exe) {
        return None;
    }
    let visible = IsWindowVisible(handle) != 0;
    let length = GetWindowTextLengthW(handle);
    let mut text = vec![0u16; length.max(0) as usize + 1];
    let copied = GetWindowTextW(handle, text.as_mut_ptr(), text.len() as i32);
    let title = utf16(&text, copied);
    if !visible && title.is_empty() {
        return None;
    }
    let mut class = [0u16; 256];
    let class_len = GetClassNameW(handle, class.as_mut_ptr(), class.len() as i32);
    let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    GetWindowRect(handle, &mut rect);
    Some(WindowSnapshot {
        hwnd,
        exe,
        class: utf16(&class, class_len),
        title,
        visible,
        minimized: IsIconic(handle) != 0,
        maximized: IsZoomed(handle) != 0,
        width: rect.right - rect.left,
        height: rect.bottom - rect.top,
    })
}

/// 調べる部品の種類。
const CONTROL_TYPES: &[(UIA_CONTROLTYPE_ID, &str)] = &[
    (UIA_ButtonControlTypeId, "Button"),
    (UIA_CheckBoxControlTypeId, "CheckBox"),
    (UIA_SplitButtonControlTypeId, "SplitButton"),
    (UIA_MenuItemControlTypeId, "MenuItem"),
];

fn kind_name(id: UIA_CONTROLTYPE_ID) -> &'static str {
    CONTROL_TYPES.iter().find(|(known, _)| *known == id).map(|(_, name)| *name).unwrap_or("Other")
}

/// UI Automation で部品を調べる道具一式（作るのは 1 回だけ）。
pub struct Uia {
    automation: IUIAutomation,
    condition: IUIAutomationCondition,
    cache: IUIAutomationCacheRequest,
}

/// 整数の VARIANT（VT_I4）。
fn variant_i32(value: i32) -> VARIANT {
    let mut variant = VARIANT::default();
    unsafe {
        let inner = &mut *variant.Anonymous.Anonymous;
        inner.vt = VT_I4;
        inner.Anonymous.lVal = value;
    }
    variant
}

impl Uia {
    /// 呼ぶスレッドで COM を初期化しておくこと。
    pub fn new() -> Result<Self, String> {
        unsafe {
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).map_err(|err| code(&err))?;
            // 応答しないアプリで止まり続けないよう、待つ時間を区切る
            if let Ok(automation2) = automation.cast::<IUIAutomation2>() {
                let _ = automation2.SetConnectionTimeout(2_000);
                let _ = automation2.SetTransactionTimeout(5_000);
            }
            let mut condition: Option<IUIAutomationCondition> = None;
            for (id, _) in CONTROL_TYPES {
                let one = automation
                    .CreatePropertyCondition(UIA_ControlTypePropertyId, &variant_i32(id.0))
                    .map_err(|err| code(&err))?;
                condition = Some(match condition {
                    None => one,
                    Some(before) => automation.CreateOrCondition(&before, &one).map_err(|err| code(&err))?,
                });
            }
            let cache = automation.CreateCacheRequest().map_err(|err| code(&err))?;
            cache.AddProperty(UIA_NamePropertyId).map_err(|err| code(&err))?;
            cache.AddProperty(UIA_ControlTypePropertyId).map_err(|err| code(&err))?;
            Ok(Uia { automation, condition: condition.expect("部品の種類がない"), cache })
        }
    }

    /// 窓 1 つの中の部品を調べる。
    fn scan_window(&self, window: &WindowSnapshot) -> UiaWindow {
        let mut result =
            UiaWindow { hwnd: window.hwnd, exe: window.exe.clone(), scanned: 0, matched: Vec::new(), error: None };
        let found = unsafe {
            self.automation
                .ElementFromHandle(HWND(window.hwnd as *mut _))
                .and_then(|element| element.FindAllBuildCache(TreeScope_Descendants, &self.condition, &self.cache))
        };
        let found = match found {
            Ok(found) => found,
            Err(err) => {
                result.error = Some(code(&err));
                return result;
            }
        };
        let count = unsafe { found.Length() }.unwrap_or(0);
        for at in 0..count {
            let Ok(element) = (unsafe { found.GetElement(at) }) else {
                continue;
            };
            result.scanned += 1;
            let name = unsafe { element.CachedName() }.map(|name| name.to_string()).unwrap_or_default();
            if control_category(&name).is_some() {
                let kind = unsafe { element.CachedControlType() }.map(kind_name).unwrap_or("Other");
                result.matched.push(Control { name, kind });
            }
        }
        result
    }

    /// 会議アプリとブラウザの、見えている窓（最小化を含む）の中を調べる。
    pub fn scan(&self) -> UiaScan {
        let started = Instant::now();
        let windows: Vec<UiaWindow> =
            list_windows().iter().filter(|w| w.visible).map(|window| self.scan_window(window)).collect();
        UiaScan { windows, elapsed_ms: started.elapsed().as_millis(), error: None }
    }
}

/// UI Automation の走査を 1 回する（呼ぶスレッドで COM を初期化しておくこと）。
pub fn scan_once(uia: &Result<Uia, String>) -> UiaScan {
    match uia {
        Ok(uia) => uia.scan(),
        Err(error) => UiaScan { error: Some(error.clone()), ..Default::default() },
    }
}

pub const WEBCAM: &str = r"Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\webcam";

/// カメラの使用状況の記録（ConsentStore の webcam）。
pub fn read_camera() -> MicStore {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    kiroscribe_core::meetings::windows_observer::read_mic_store(&RegKey::predef(HKEY_CURRENT_USER), WEBCAM)
}

/// Windows の版（記録の見出しに書く）。
pub fn windows_version() -> String {
    use winreg::enums::HKEY_LOCAL_MACHINE;
    use winreg::RegKey;
    let Ok(key) = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion") else {
        return "不明".into();
    };
    let text = |name: &str| key.get_value::<String, _>(name).unwrap_or_default();
    let ubr: u32 = key.get_value("UBR").unwrap_or(0);
    format!("{} {} ビルド {}.{ubr}", text("ProductName"), text("DisplayVersion"), text("CurrentBuild"))
}
