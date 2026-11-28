//! 会議の検出で、1 回ごとに読む PC の様子（[`Facts`]）: 対象のアプリの窓、窓の中の部品、録音・再生のオーディオのセッション。
//!
//! 読むのは、判定表（[`crate::meeting_rules`]）の対象のアプリ（会議アプリとブラウザ）のものだけ。
//! - 窓: トップレベルの窓の題・クラス名・見えているか
//! - 部品: UI Automation で、判定表が使う種類（Button・TabItem・Text）の部品の名前と種類。調べる窓は [`scan_targets`]
//! - オーディオのセッション: [`crate::audio_sessions`]
//!
//! 画面の画像や音声そのものは読まない（SEC-013）。Windows 以外では、何も読まない。

use serde::{Deserialize, Serialize};

use crate::audio_sessions::AudioSessions;
use crate::meeting_rules::Rules;
use crate::meetings::Browsers;

/// トップレベルの窓 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowFact {
    /// 窓の識別子（HWND の値）
    pub hwnd: u64,
    /// 実行ファイルの名前（小文字）
    pub exe: String,
    pub class: String,
    pub title: String,
    /// 見えているか（最小化した窓も見えているとする）
    pub visible: bool,
}

/// 部品の種類（UI Automation の ControlType のうち、判定表が使うもの）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ControlType {
    Button,
    CheckBox,
    MenuItem,
    TabItem,
    Text,
}

impl ControlType {
    pub fn name(self) -> &'static str {
        match self {
            ControlType::Button => "Button",
            ControlType::CheckBox => "CheckBox",
            ControlType::MenuItem => "MenuItem",
            ControlType::TabItem => "TabItem",
            ControlType::Text => "Text",
        }
    }
}

/// 窓の中の部品 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlFact {
    pub kind: ControlType,
    pub name: String,
}

/// 窓 1 つの中を調べた結果。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScannedWindow {
    pub hwnd: u64,
    /// 判定表の部品の印に当たりうる部品（[`Rules::keeps_control`]）
    pub controls: Vec<ControlFact>,
    /// 調べた部品の数
    pub seen: usize,
    /// 調べられなかった理由（HRESULT・応答のない窓など）
    pub error: Option<String>,
}

/// 窓の中の部品を調べた 1 回分。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ControlScan {
    pub windows: Vec<ScannedWindow>,
    /// かかった時間（ミリ秒）
    pub elapsed_ms: u64,
    /// UI Automation そのものを使えなかった理由
    pub error: Option<String>,
}

/// 1 回分の PC の様子。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Facts {
    /// 対象のアプリの窓
    pub windows: Vec<WindowFact>,
    /// 録音のオーディオのセッション
    pub capture: AudioSessions,
    /// 再生のオーディオのセッション
    pub render: AudioSessions,
    /// 窓の中の部品（この回に調べなかったときは `None`）
    pub scan: Option<ControlScan>,
}

impl Facts {
    /// この回に調べた窓の中。
    pub fn scanned_window(&self, hwnd: u64) -> Option<&ScannedWindow> {
        self.scan.as_ref()?.windows.iter().find(|window| window.hwnd == hwnd && window.error.is_none())
    }

    /// この回に、その窓の中を調べたか。UI Automation そのものを使えないときは、調べたものとして扱う
    /// （組み合わせでの開始を、いつまでも止めないため）。
    pub fn scan_covers(&self, hwnd: u64) -> bool {
        match &self.scan {
            Some(scan) if scan.error.is_some() => true,
            Some(_) => self.scanned_window(hwnd).is_some(),
            None => false,
        }
    }
}

/// 窓の中の部品を調べる窓: デスクトップの会議アプリ（部品の印のあるもの）の見えている窓と、対象のブラウザの
/// 見えている窓のうち題が判定表のサービスの題に当たるものと、続いているブラウザの会議の窓（`meeting_windows`）。
pub fn scan_targets(rules: &Rules, browsers: &Browsers, windows: &[WindowFact], meeting_windows: &[u64]) -> Vec<u64> {
    windows
        .iter()
        .filter(|window| window.visible)
        .filter(|window| {
            if browsers.contains(&window.exe) {
                rules.browser_title_matches(&window.title) || meeting_windows.contains(&window.hwnd)
            } else {
                rules.scans_desktop(&window.exe)
            }
        })
        .map(|window| window.hwnd)
        .collect()
}

/// PC の様子を読む（Windows では、UI Automation の部品を持ち続ける。作ったスレッドで使うこと）。
pub struct Observer {
    #[cfg(windows)]
    inner: win::Observer,
}

impl Default for Observer {
    fn default() -> Self {
        Self::new()
    }
}

impl Observer {
    pub fn new() -> Self {
        Observer {
            #[cfg(windows)]
            inner: win::Observer::new(),
        }
    }

    /// 1 回分を読む。`scan` のときだけ、窓の中の部品も調べる。Windows 以外では、何も読まない。
    pub fn observe(&mut self, rules: &Rules, browsers: &Browsers, meeting_windows: &[u64], scan: bool) -> Facts {
        #[cfg(windows)]
        {
            self.inner.observe(rules, browsers, meeting_windows, scan)
        }
        #[cfg(not(windows))]
        {
            let _ = (rules, browsers, meeting_windows, scan);
            Facts::default()
        }
    }
}

#[cfg(windows)]
mod win {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use windows::core::Interface;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
    use windows::Win32::System::Variant::{VARIANT, VT_I4};
    use windows::Win32::UI::Accessibility::{
        AccessibleObjectFromWindow, CUIAutomation, IAccessible, IUIAutomation, IUIAutomation2, IUIAutomationCacheRequest,
        IUIAutomationCondition, TreeScope_Descendants, UIA_ButtonControlTypeId, UIA_CheckBoxControlTypeId,
        UIA_ControlTypePropertyId, UIA_MenuItemControlTypeId, UIA_NamePropertyId, UIA_TabItemControlTypeId,
        UIA_TextControlTypeId, UIA_CONTROLTYPE_ID,
    };

    use super::{scan_targets, ControlFact, ControlScan, ControlType, Facts, ScannedWindow, WindowFact};
    use crate::audio_sessions::{self, hresult, Flow};
    use crate::meeting_rules::Rules;
    use crate::meetings::{exe_name, Browsers};

    /// Chromium の部品を使う窓のクラス名と、その中の Web の部分の子の窓のクラス名（MTG-030）。
    const CHROMIUM_WINDOW: &str = "Chrome_WidgetWin_1";
    const CHROMIUM_RENDER_WIDGET: &str = "Chrome_RenderWidgetHostHWND";
    /// 同じ窓に MSAA で問い合わせ直すまでの間隔。
    const NUDGE_INTERVAL: Duration = Duration::from_secs(60);
    /// 1 つの窓の中で問い合わせる子の窓の数の上限。
    const NUDGE_CHILDREN: usize = 4;

    fn code(err: &windows::core::Error) -> String {
        hresult(err.code().0)
    }

    fn control_type_id(kind: ControlType) -> UIA_CONTROLTYPE_ID {
        match kind {
            ControlType::Button => UIA_ButtonControlTypeId,
            ControlType::CheckBox => UIA_CheckBoxControlTypeId,
            ControlType::MenuItem => UIA_MenuItemControlTypeId,
            ControlType::TabItem => UIA_TabItemControlTypeId,
            ControlType::Text => UIA_TextControlTypeId,
        }
    }

    fn control_type(id: UIA_CONTROLTYPE_ID) -> Option<ControlType> {
        [ControlType::Button, ControlType::CheckBox, ControlType::MenuItem, ControlType::TabItem, ControlType::Text]
            .into_iter()
            .find(|kind| control_type_id(*kind) == id)
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

    /// UI Automation で部品を調べる道具一式。
    struct Uia {
        automation: IUIAutomation,
        cache: IUIAutomationCacheRequest,
        condition: Option<(Vec<ControlType>, IUIAutomationCondition)>,
    }

    impl Uia {
        fn new() -> Result<Self, String> {
            unsafe {
                let automation: IUIAutomation =
                    CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).map_err(|err| code(&err))?;
                // 応答しないアプリで止まり続けないよう、待つ時間を区切る
                if let Ok(automation2) = automation.cast::<IUIAutomation2>() {
                    let _ = automation2.SetConnectionTimeout(2_000);
                    let _ = automation2.SetTransactionTimeout(5_000);
                }
                let cache = automation.CreateCacheRequest().map_err(|err| code(&err))?;
                cache.AddProperty(UIA_NamePropertyId).map_err(|err| code(&err))?;
                cache.AddProperty(UIA_ControlTypePropertyId).map_err(|err| code(&err))?;
                Ok(Uia { automation, cache, condition: None })
            }
        }

        /// 読む部品の種類の条件（種類が変わったときだけ作り直す）。
        fn condition(&mut self, types: &[ControlType]) -> Result<IUIAutomationCondition, String> {
            if let Some((known, condition)) = &self.condition {
                if known == types {
                    return Ok(condition.clone());
                }
            }
            let mut condition: Option<IUIAutomationCondition> = None;
            for kind in types {
                let one = unsafe {
                    self.automation
                        .CreatePropertyCondition(UIA_ControlTypePropertyId, &variant_i32(control_type_id(*kind).0))
                        .map_err(|err| code(&err))?
                };
                condition = Some(match condition {
                    None => one,
                    Some(before) => unsafe { self.automation.CreateOrCondition(&before, &one).map_err(|err| code(&err))? },
                });
            }
            let condition = condition.ok_or("判定表に部品の印がない")?;
            self.condition = Some((types.to_vec(), condition.clone()));
            Ok(condition)
        }

        fn scan_window(&self, hwnd: u64, condition: &IUIAutomationCondition, rules: &Rules) -> ScannedWindow {
            let mut result = ScannedWindow { hwnd, ..Default::default() };
            let found = unsafe {
                self.automation
                    .ElementFromHandle(HWND(hwnd as usize as *mut _))
                    .and_then(|element| element.FindAllBuildCache(TreeScope_Descendants, condition, &self.cache))
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
                let Ok(element) = (unsafe { found.GetElement(at) }) else { continue };
                result.seen += 1;
                let Some(kind) = unsafe { element.CachedControlType() }.ok().and_then(control_type) else { continue };
                let name = unsafe { element.CachedName() }.map(|name| name.to_string()).unwrap_or_default();
                if rules.keeps_control(kind, &name) {
                    result.controls.push(ControlFact { kind, name });
                }
            }
            result
        }
    }

    pub struct Observer {
        com: bool,
        uia: Option<Result<Uia, String>>,
        nudged: HashMap<u64, Instant>,
        own_pid: u32,
    }

    impl Observer {
        pub fn new() -> Self {
            // UI Automation の部品を持ち続けるので、このスレッドの COM は、この道具を捨てるまで初期化したままにする
            let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
            Observer { com, uia: None, nudged: HashMap::new(), own_pid: std::process::id() }
        }

        pub fn observe(&mut self, rules: &Rules, browsers: &Browsers, meeting_windows: &[u64], scan: bool) -> Facts {
            let windows = list_windows(self.own_pid, &|exe| rules.is_target(exe, browsers));
            let own = self.own_pid;
            let without_own = |mut sessions: audio_sessions::AudioSessions| {
                sessions.sessions.retain(|session| session.pid != own);
                sessions
            };
            let capture = without_own(audio_sessions::read(Flow::Capture, &|_| false));
            let render = without_own(audio_sessions::read(Flow::Render, &|exe| rules.wants_peak(exe, browsers)));
            let scan = scan.then(|| {
                let targets = scan_targets(rules, browsers, &windows, meeting_windows);
                self.scan(rules, &windows, &targets)
            });
            Facts { windows, capture, render, scan }
        }

        fn scan(&mut self, rules: &Rules, windows: &[WindowFact], targets: &[u64]) -> ControlScan {
            let started = Instant::now();
            let uia = self.uia.get_or_insert_with(Uia::new);
            let uia = match uia {
                Ok(uia) => uia,
                Err(error) => return ControlScan { error: Some(error.clone()), ..Default::default() },
            };
            let condition = match uia.condition(&rules.control_types()) {
                Ok(condition) => condition,
                Err(error) => return ControlScan { error: Some(error), ..Default::default() },
            };
            let mut scanned = Vec::new();
            for hwnd in targets {
                let Some(window) = windows.iter().find(|window| window.hwnd == *hwnd) else { continue };
                if unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsHungAppWindow(*hwnd as usize as _) } != 0 {
                    scanned.push(ScannedWindow { hwnd: *hwnd, error: Some("応答のない窓".into()), ..Default::default() });
                    continue;
                }
                if window.class == CHROMIUM_WINDOW {
                    let due = self.nudged.get(hwnd).is_none_or(|at| at.elapsed() >= NUDGE_INTERVAL);
                    if due {
                        unsafe { nudge_chromium(*hwnd) };
                        self.nudged.insert(*hwnd, Instant::now());
                    }
                }
                scanned.push(uia.scan_window(*hwnd, &condition, rules));
            }
            // なくなった窓の記録は捨てる
            self.nudged.retain(|hwnd, _| windows.iter().any(|window| window.hwnd == *hwnd));
            ControlScan { windows: scanned, elapsed_ms: started.elapsed().as_millis() as u64, error: None }
        }
    }

    impl Drop for Observer {
        fn drop(&mut self) {
            // UI Automation の部品は、COM を戻す前に解放する
            self.uia = None;
            if self.com {
                unsafe { CoUninitialize() };
            }
        }
    }

    /// Chromium の部品を使う窓（Discord・Slack・ブラウザ）の中の Web の部分は、支援技術からの問い合わせがあるまで、
    /// 部品の木を作らないことがある。その子の窓（Chrome_RenderWidgetHostHWND）に MSAA で問い合わせて名前を読み、
    /// 部品を読めるようにする（MTG-030）。
    unsafe fn nudge_chromium(hwnd: u64) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{EnumChildWindows, GetClassNameW, IsHungAppWindow, OBJID_CLIENT};
        unsafe extern "system" fn collect(
            child: windows_sys::Win32::Foundation::HWND,
            lparam: windows_sys::Win32::Foundation::LPARAM,
        ) -> windows_sys::core::BOOL {
            let list = &mut *(lparam as *mut Vec<usize>);
            let mut class = [0u16; 64];
            let len = GetClassNameW(child, class.as_mut_ptr(), class.len() as i32);
            if len > 0 && String::from_utf16_lossy(&class[..len as usize]) == CHROMIUM_RENDER_WIDGET {
                list.push(child as usize);
            }
            (list.len() < NUDGE_CHILDREN) as i32
        }
        let mut children: Vec<usize> = Vec::new();
        EnumChildWindows(hwnd as usize as _, Some(collect), &mut children as *mut _ as isize);
        for child in children {
            if IsHungAppWindow(child as _) != 0 {
                continue;
            }
            let mut object: *mut core::ffi::c_void = std::ptr::null_mut();
            let asked = AccessibleObjectFromWindow(HWND(child as *mut _), OBJID_CLIENT as u32, &IAccessible::IID, &mut object);
            if asked.is_ok() && !object.is_null() {
                let accessible = IAccessible::from_raw(object);
                // 名前を読んだことで、Chromium は支援技術が使っているとみなす
                let _ = accessible.get_accName(&variant_i32(0));
            }
        }
    }

    /// 対象のアプリのトップレベルの窓。見えない窓は、題のあるものだけ。自分自身（Kiroscribe）の窓は除く。
    fn list_windows(own_pid: u32, target: &dyn Fn(&str) -> bool) -> Vec<WindowFact> {
        use windows_sys::Win32::UI::WindowsAndMessaging::EnumWindows;
        unsafe extern "system" fn collect(
            hwnd: windows_sys::Win32::Foundation::HWND,
            lparam: windows_sys::Win32::Foundation::LPARAM,
        ) -> windows_sys::core::BOOL {
            let list = &mut *(lparam as *mut Vec<usize>);
            list.push(hwnd as usize);
            1
        }
        let mut handles: Vec<usize> = Vec::new();
        unsafe {
            EnumWindows(Some(collect), &mut handles as *mut _ as isize);
        }
        let mut names: HashMap<u32, String> = HashMap::new();
        handles.into_iter().filter_map(|hwnd| unsafe { window(hwnd, own_pid, target, &mut names) }).collect()
    }

    fn utf16(buf: &[u16], len: i32) -> String {
        if len <= 0 {
            String::new()
        } else {
            String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())])
        }
    }

    unsafe fn window(hwnd: usize, own_pid: u32, target: &dyn Fn(&str) -> bool, names: &mut HashMap<u32, String>) -> Option<WindowFact> {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetClassNameW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
        };
        let handle = hwnd as windows_sys::Win32::Foundation::HWND;
        let mut pid = 0u32;
        GetWindowThreadProcessId(handle, &mut pid);
        if pid == own_pid || pid == 0 {
            return None;
        }
        let exe = names.entry(pid).or_insert_with(|| process_exe(pid)).clone();
        if exe.is_empty() || !target(&exe) {
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
        Some(WindowFact { hwnd: hwnd as u64, exe, class: utf16(&class, class_len), title, visible })
    }

    /// プロセスの実行ファイルの名前（小文字）。読めなければ空。
    fn process_exe(pid: u32) -> String {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return String::new();
            }
            let mut buf = [0u16; 1024];
            let mut size = buf.len() as u32;
            let name = if QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut size) != 0 {
                exe_name(&String::from_utf16_lossy(&buf[..size as usize]))
            } else {
                String::new()
            };
            CloseHandle(handle);
            name
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// 実際の窓・オーディオのセッション・窓の中の部品を読んでも、パニックしない。
        // [MTG-020][MTG-029]
        #[test]
        fn observing_the_real_pc_does_not_fail() {
            let rules = Rules::builtin();
            let browsers = Browsers::builtin();
            let mut observer = Observer::new();
            let facts = observer.observe(rules, &browsers, &[], true);
            assert!(facts.capture.available, "{:?}", facts.capture.error);
            assert!(facts.scan.is_some());
            let _ = crate::meeting_rules::evaluate(rules, &facts, &browsers);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(hwnd: u64, exe: &str, title: &str, visible: bool) -> WindowFact {
        WindowFact { hwnd, exe: exe.into(), class: "Chrome_WidgetWin_1".into(), title: title.into(), visible }
    }

    /// 窓の中を調べるのは、会議アプリの見えている窓と、題がサービスに当たるブラウザの窓と、続いている会議の窓だけ。
    // [MTG-029]
    #[test]
    fn only_meeting_app_windows_and_meeting_tabs_are_scanned() {
        let rules = Rules::builtin();
        let browsers = Browsers::builtin();
        let windows = [
            window(1, "slack.exe", "#general - Acme - Slack", true),
            window(2, "slack.exe", "", false),
            window(3, "chrome.exe", "Meet - abc-defg-hij - Google Chrome", true),
            window(4, "chrome.exe", "受信トレイ - Gmail - Google Chrome", true),
            window(5, "zen.exe", "Meet – abc-defg-hij — Zen Browser", true),
            window(6, "msedge.exe", "企画書 - Word - Microsoft Edge", true),
            window(7, "zoom.exe", "Zoom ミーティング", true),
            window(8, "thunderbird.exe", "受信トレイ - Mozilla Thunderbird", true),
        ];
        assert_eq!(scan_targets(rules, &browsers, &windows, &[]), vec![1, 3, 5, 7]);
        assert_eq!(scan_targets(rules, &browsers, &windows, &[6]), vec![1, 3, 5, 6, 7], "背面の会議のタブのある窓");
    }
}
