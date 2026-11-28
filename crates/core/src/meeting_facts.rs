//! 会議の検出で、1 回ごとに読む PC の様子（[`Facts`]）: 対象のアプリの窓、窓の中の部品、録音・再生のオーディオのセッション。
//!
//! 読むのは、判定表（[`crate::meeting_rules`]）の対象のアプリ（会議アプリとブラウザ）のものだけ。
//! - 窓: トップレベルの窓の題・クラス名・見えているか
//! - 部品: UI Automation で、判定表が使う種類（Button・TabItem・Text）の部品の名前と種類。調べる窓は [`scan_targets`]
//! - サイト: 調べるブラウザの窓の、ブラウザ自身のツールバーの中のアドレスバーと、最上位の本文の URL から、ブラウザが
//!   表示を確定したページのホスト名（[`confirmed_site`]）。パスとクエリは読んだその場で捨てる
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
    /// ブラウザの窓なら、表示中のページのホスト名（小文字。アドレスバーから読む）。読めなかった・ブラウザでないときは `None`
    pub site: Option<String>,
}

impl WindowFact {
    /// 表示中のページのホスト名を添える。
    pub fn at_site(mut self, site: &str) -> Self {
        self.site = Some(site.to_string());
        self
    }
}

/// アドレスバーやページの URL（`https://meet.google.com/abc?x=1` や、スキームを省いた `meet.google.com/abc`）からホスト名を取り出す。
/// 先頭が `http://`・`https://` のときと、スキームを省いた表示（先頭に `https://` を補う）だけを、URL の構文解析
/// （[`url::Url`]）で読み、外側の URL のホストだけを返す（パスやクエリの中の別の URL は見ない）。
/// ほかのスキーム（`chrome://` など）・ホスト名の形でないもの（検索語の入力中など）は `None`。
pub fn host_of(address: &str) -> Option<String> {
    let text = address.trim();
    if text.is_empty() || text.contains(char::is_whitespace) {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    let with_scheme = if lower.starts_with("https://") || lower.starts_with("http://") {
        text.to_string()
    } else {
        // スキームを省いた表示。先頭の区切りまでに、ポートでない `:` があるもの（`chrome://`・`about:` など）は、ほかのスキーム
        let head = text.split(['/', '?', '#', '\\']).next().unwrap_or_default();
        if text.starts_with('/') || head.contains(':') && !is_host_and_port(head) {
            return None;
        }
        format!("https://{text}")
    };
    let url = url::Url::parse(&with_scheme).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = match url.host()? {
        url::Host::Domain(domain) => domain.trim_end_matches('.').to_lowercase(),
        url::Host::Ipv4(address) => address.to_string(),
        url::Host::Ipv6(address) => address.to_string(),
    };
    let valid = !host.is_empty() && (host.contains('.') || host.contains(':') || host == "localhost");
    valid.then_some(host)
}

/// スキームを省いた表示の先頭が「ホスト名:ポート」（`localhost:3000`・`[::1]:8080`）か。
fn is_host_and_port(head: &str) -> bool {
    let authority = head.rsplit('@').next().unwrap_or_default();
    if authority.starts_with('[') {
        return true;
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => !host.is_empty() && !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()),
        None => true,
    }
}

/// ブラウザの種類（アドレスバーの見分け方が違う）。窓のクラス名で決める。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// Chromium 系（Chrome・Edge・Brave など。Electron のアプリも同じクラス名）
    Chromium,
    /// Firefox 系（Firefox・Zen など）
    Firefox,
}

impl Engine {
    /// 窓のクラス名からブラウザの種類を決める。知らないクラス名は `None`（アドレスバーを読まない）。
    pub fn of_window(class: &str) -> Option<Engine> {
        match class {
            "Chrome_WidgetWin_1" => Some(Engine::Chromium),
            "MozillaWindowClass" => Some(Engine::Firefox),
            _ => None,
        }
    }

    /// アドレスバーの部品か。Chromium 系はクラス名が OmniboxViewViews の Edit、Firefox 系は AutomationId が urlbar-input の
    /// Edit か ComboBox（Firefox 150 の入力欄は role="combobox" で、UI Automation には ComboBox として出る）。
    /// ページの作成者もクラス名や識別子を付けられるので、ブラウザ自身のツールバーの中か（[`in_browser_toolbar`]）も確かめる。
    pub fn is_address_bar(self, part: &UiPart) -> bool {
        match self {
            Engine::Chromium => part.kind == PartKind::Edit && part.class_name == "OmniboxViewViews",
            Engine::Firefox => matches!(part.kind, PartKind::Edit | PartKind::ComboBox) && part.automation_id == "urlbar-input",
        }
    }
}

/// アドレスバーを見分けるのに使う部品の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartKind {
    Edit,
    ComboBox,
    /// ページの本文（ページの作成者が中身を決める）
    Document,
    ToolBar,
    Other,
}

/// アドレスバーを見分けるのに使う、部品の値。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiPart {
    pub kind: PartKind,
    pub class_name: String,
    pub automation_id: String,
}

/// 部品の祖先の種類（近い順。窓そのものは含めない）から、その部品がブラウザ自身のツールバーの中か。
/// ページの本文（Document）の中の部品は、ツールバーに似せて作れるので、ツールバーの中とみなさない。
pub fn in_browser_toolbar(ancestors: &[PartKind]) -> bool {
    !ancestors.contains(&PartKind::Document) && ancestors.contains(&PartKind::ToolBar)
}

/// 本文（Document）の祖先の種類から、その本文が窓の最上位の本文（埋め込まれた本文でない）か。
pub fn is_top_document(ancestors: &[PartKind]) -> bool {
    !ancestors.contains(&PartKind::Document)
}

/// ブラウザ自身のツールバーの中で見つけたアドレスバー 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressBar {
    /// 表示中の値
    pub value: String,
    /// キーボードの入力先か（編集中でありうる）
    pub focused: bool,
}

/// 表示中のページのホスト名を、ブラウザが確定したものだけ返す。
/// - アドレスバーが 1 つだけ見つかったときだけ（見つからない・2 つ以上あるときは `None`）
/// - 最上位の本文の URL（ブラウザが表示を確定したページ）を読めたときは、アドレスバーのホスト名がそのどれかと同じときだけ
///   （入力途中の値は、まだ確定していないので使わない）
/// - 本文の URL を読めないときは、アドレスバーが入力先でないときだけ
pub fn confirmed_site(bars: &[AddressBar], documents: &[String]) -> Option<String> {
    let [bar] = bars else { return None };
    let host = host_of(&bar.value)?;
    let committed: Vec<String> = documents.iter().filter_map(|url| host_of(url)).collect();
    if committed.is_empty() {
        return (!bar.focused).then_some(host);
    }
    committed.contains(&host).then_some(host)
}

/// 窓 1 つの中を調べた結果のうち、調べ切れなかった理由（組み合わせの開始に要る調べた回に数えない。MTG-007）。
/// - `missed`・`first_error`: 読めなかった部品の数と、最初の理由
/// - `requires_document`: ブラウザや Chromium の部品を使うアプリの窓（ページの本文の中に参加ボタンなどがある）
/// - `documents`: 見つかった本文（Document）の数。0 なら、ブラウザの枠だけを読み、本文を読めていない
/// - `prepare_error`: 本文を読めるようにする準備（Chromium への MSAA の問い合わせ）の失敗
pub fn scan_shortfall(
    missed: usize,
    first_error: Option<&str>,
    requires_document: bool,
    documents: usize,
    prepare_error: Option<&str>,
) -> Option<String> {
    let mut reasons = Vec::new();
    if let Some(error) = prepare_error {
        reasons.push(format!("本文を読む準備ができない（{error}）"));
    }
    if requires_document && documents == 0 {
        reasons.push("本文を読めない".to_string());
    }
    if missed > 0 {
        reasons.push(format!("{missed} 個の部品を読めない（{}）", first_error.unwrap_or_default()));
    }
    (!reasons.is_empty()).then(|| reasons.join("・"))
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
    /// 調べ切れなかったときの理由（[`scan_shortfall`]: 部品の数・部品・種類・名前の取得の失敗、本文を読めない、
    /// 本文を読む準備の失敗）。読めた部品の印は立つが、打ち消しの印を読み損ねたかもしれないので、組み合わせの開始に要る
    /// 調べた回には数えない（MTG-007）
    pub incomplete: Option<String>,
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

    /// この回に、その窓の中を調べ切れたか。UI Automation そのものを使えないとき、その窓を調べられなかったとき、
    /// 一部の部品を読めなかったときは、調べたものとして扱わない（待機画面の参加ボタンを読めないまま、組み合わせで始めない。MTG-007）。
    pub fn scan_covers(&self, hwnd: u64) -> bool {
        self.scan.as_ref().is_some_and(|scan| scan.error.is_none())
            && self.scanned_window(hwnd).is_some_and(|scanned| scanned.incomplete.is_none())
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
    use windows::core::BSTR;
    use windows::Win32::UI::Accessibility::{
        AccessibleObjectFromWindow, CUIAutomation, IAccessible, IUIAutomation, IUIAutomation2, IUIAutomationCacheRequest,
        IUIAutomationCondition, IUIAutomationElement, IUIAutomationTreeWalker, TreeScope_Descendants,
        UIA_AutomationIdPropertyId, UIA_ButtonControlTypeId, UIA_CheckBoxControlTypeId, UIA_ClassNamePropertyId,
        UIA_ComboBoxControlTypeId, UIA_ControlTypePropertyId, UIA_DocumentControlTypeId, UIA_EditControlTypeId,
        UIA_HasKeyboardFocusPropertyId, UIA_MenuItemControlTypeId, UIA_NamePropertyId, UIA_NativeWindowHandlePropertyId,
        UIA_TabItemControlTypeId, UIA_TextControlTypeId, UIA_ToolBarControlTypeId, UIA_ValueValuePropertyId,
        UIA_CONTROLTYPE_ID,
    };

    use super::{
        confirmed_site, in_browser_toolbar, is_top_document, scan_shortfall, scan_targets, AddressBar, ControlFact, ControlScan,
        ControlType, Engine, Facts, PartKind, ScannedWindow, UiPart, WindowFact,
    };
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
    /// UI Automation を使えなかったあと、作り直すまでの間隔。
    const RETRY_INTERVAL: Duration = Duration::from_secs(30);
    /// ブラウザのアドレスバーの部品: Chromium 系はクラス名、Firefox 系は AutomationId で見分ける（[`Engine::is_address_bar`]）。
    const CHROMIUM_ADDRESS_BAR: &str = "OmniboxViewViews";
    const FIREFOX_ADDRESS_BAR: &str = "urlbar-input";
    /// 部品から窓までの祖先をたどる段数の上限（超えたら、ツールバーの中か・最上位の本文かを確かめられないものとして扱う）。
    const MAX_ANCESTORS: usize = 32;

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
        /// 本文（Document）の部品の条件（窓の中を調べるときに、本文を読めたかを確かめる）
        document: IUIAutomationCondition,
        /// アドレスバーと本文を読むための、取り置く値と、ブラウザの種類ごとの条件（アドレスバーか本文）
        site_cache: IUIAutomationCacheRequest,
        chromium_site: IUIAutomationCondition,
        firefox_site: IUIAutomationCondition,
        /// 祖先をたどる道具と、そのときに取り置く値（種類と窓の識別子）
        walker: IUIAutomationTreeWalker,
        parent_cache: IUIAutomationCacheRequest,
    }

    fn part_kind(id: UIA_CONTROLTYPE_ID) -> PartKind {
        match id {
            id if id == UIA_EditControlTypeId => PartKind::Edit,
            id if id == UIA_ComboBoxControlTypeId => PartKind::ComboBox,
            id if id == UIA_DocumentControlTypeId => PartKind::Document,
            id if id == UIA_ToolBarControlTypeId => PartKind::ToolBar,
            _ => PartKind::Other,
        }
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
                let type_is = |id: UIA_CONTROLTYPE_ID| {
                    automation.CreatePropertyCondition(UIA_ControlTypePropertyId, &variant_i32(id.0)).map_err(|err| code(&err))
                };
                let document = type_is(UIA_DocumentControlTypeId)?;
                let site_cache = automation.CreateCacheRequest().map_err(|err| code(&err))?;
                for property in [
                    UIA_ValueValuePropertyId,
                    UIA_ControlTypePropertyId,
                    UIA_ClassNamePropertyId,
                    UIA_AutomationIdPropertyId,
                    UIA_HasKeyboardFocusPropertyId,
                ] {
                    site_cache.AddProperty(property).map_err(|err| code(&err))?;
                }
                let chromium = automation
                    .CreatePropertyCondition(UIA_ClassNamePropertyId, &VARIANT::from(CHROMIUM_ADDRESS_BAR))
                    .map_err(|err| code(&err))?;
                let chromium = automation.CreateAndCondition(&type_is(UIA_EditControlTypeId)?, &chromium).map_err(|err| code(&err))?;
                let chromium_site = automation.CreateOrCondition(&chromium, &document).map_err(|err| code(&err))?;
                let firefox = automation
                    .CreatePropertyCondition(UIA_AutomationIdPropertyId, &VARIANT::from(FIREFOX_ADDRESS_BAR))
                    .map_err(|err| code(&err))?;
                let firefox_types =
                    automation.CreateOrCondition(&type_is(UIA_EditControlTypeId)?, &type_is(UIA_ComboBoxControlTypeId)?).map_err(|err| code(&err))?;
                let firefox = automation.CreateAndCondition(&firefox_types, &firefox).map_err(|err| code(&err))?;
                let firefox_site = automation.CreateOrCondition(&firefox, &document).map_err(|err| code(&err))?;
                let walker = automation.ControlViewWalker().map_err(|err| code(&err))?;
                let parent_cache = automation.CreateCacheRequest().map_err(|err| code(&err))?;
                parent_cache.AddProperty(UIA_ControlTypePropertyId).map_err(|err| code(&err))?;
                parent_cache.AddProperty(UIA_NativeWindowHandlePropertyId).map_err(|err| code(&err))?;
                Ok(Uia {
                    automation,
                    cache,
                    condition: None,
                    document,
                    site_cache,
                    chromium_site,
                    firefox_site,
                    walker,
                    parent_cache,
                })
            }
        }

        /// 部品から窓（`hwnd`）までの祖先の種類（近い順。窓そのものは含めない）。窓までたどれないときは `None`。
        /// 本文（Document）の祖先が見つかったら、そこで止める（その部品は本文の中なので、それより上は要らない）。
        fn ancestors(&self, element: &IUIAutomationElement, hwnd: u64) -> Option<Vec<PartKind>> {
            let mut kinds = Vec::new();
            let mut current = element.clone();
            for _ in 0..MAX_ANCESTORS {
                let parent = unsafe { self.walker.GetParentElementBuildCache(&current, &self.parent_cache) }.ok()?;
                let handle = unsafe { parent.CachedNativeWindowHandle() }.ok()?;
                if handle.0 as usize as u64 == hwnd {
                    return Some(kinds);
                }
                let kind = part_kind(unsafe { parent.CachedControlType() }.ok()?);
                kinds.push(kind);
                if kind == PartKind::Document {
                    return Some(kinds);
                }
                current = parent;
            }
            None
        }

        /// ブラウザの窓で、ブラウザが表示を確定したページのホスト名（[`confirmed_site`]）。ブラウザ自身のツールバーの中の
        /// アドレスバーがない（アプリとして開いた窓など）・読めないときは `None`。ページの本文の中の部品は、アドレスバーとして読まない。
        /// 値（URL）は、ホスト名を取り出したらその場で捨てる。
        fn site(&self, hwnd: u64, engine: Engine) -> Option<String> {
            let condition = match engine {
                Engine::Chromium => &self.chromium_site,
                Engine::Firefox => &self.firefox_site,
            };
            let found = unsafe {
                let element = self.automation.ElementFromHandle(HWND(hwnd as usize as *mut _)).ok()?;
                element.FindAllBuildCache(TreeScope_Descendants, condition, &self.site_cache).ok()?
            };
            let mut bars: Vec<AddressBar> = Vec::new();
            let mut documents: Vec<String> = Vec::new();
            for at in 0..unsafe { found.Length() }.ok()? {
                let element = unsafe { found.GetElement(at) }.ok()?;
                let part = unsafe {
                    UiPart {
                        kind: part_kind(element.CachedControlType().ok()?),
                        class_name: element.CachedClassName().map(|name| name.to_string()).unwrap_or_default(),
                        automation_id: element.CachedAutomationId().map(|id| id.to_string()).unwrap_or_default(),
                    }
                };
                let value = || -> Option<String> {
                    let value = unsafe { element.GetCachedPropertyValue(UIA_ValueValuePropertyId) }.ok()?;
                    BSTR::try_from(&value).ok().map(|text| text.to_string())
                };
                if part.kind == PartKind::Document {
                    // 埋め込まれた本文（iframe など）や、たどれない本文は数えない
                    if self.ancestors(&element, hwnd).is_some_and(|ancestors| is_top_document(&ancestors)) {
                        documents.extend(value());
                    }
                } else if engine.is_address_bar(&part) {
                    if self.ancestors(&element, hwnd).is_some_and(|ancestors| in_browser_toolbar(&ancestors)) {
                        let focused = unsafe { element.CachedHasKeyboardFocus() }.map(|focus| focus.as_bool()).unwrap_or(true);
                        bars.push(AddressBar { value: value().unwrap_or_default(), focused });
                    }
                }
            }
            confirmed_site(&bars, &documents)
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
            // 本文（Document）も読み、本文を読めたかを確かめる（読めた本文は、部品の印には数えない）
            let condition = unsafe { self.automation.CreateOrCondition(&condition, &self.document).map_err(|err| code(&err))? };
            self.condition = Some((types.to_vec(), condition.clone()));
            Ok(condition)
        }

        /// 窓 1 つの中を調べる。`requires_document` の窓（ブラウザ・Chromium の部品を使うアプリ）は、本文を 1 つも読めなければ、
        /// 調べ切れなかった回にする。`prepare_error` は、本文を読めるようにする準備の失敗。
        fn scan_window(
            &self,
            hwnd: u64,
            condition: &IUIAutomationCondition,
            rules: &Rules,
            requires_document: bool,
            prepare_error: Option<&str>,
        ) -> ScannedWindow {
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
            let count = match unsafe { found.Length() } {
                Ok(count) => count,
                Err(err) => {
                    result.error = Some(code(&err));
                    return result;
                }
            };
            // 読めなかった部品の数と、最初の理由（読めた部品の印は立てるが、調べ切れなかった回として扱う）
            let mut missed = 0usize;
            let mut first: Option<String> = None;
            let mut documents = 0usize;
            let mut miss = |err: windows::core::Error| {
                missed += 1;
                first.get_or_insert_with(|| code(&err));
            };
            for at in 0..count {
                let element = match unsafe { found.GetElement(at) } {
                    Ok(element) => element,
                    Err(err) => {
                        miss(err);
                        continue;
                    }
                };
                result.seen += 1;
                let kind = match unsafe { element.CachedControlType() } {
                    Ok(id) if id == UIA_DocumentControlTypeId => {
                        documents += 1;
                        continue;
                    }
                    Ok(id) => control_type(id),
                    Err(err) => {
                        miss(err);
                        continue;
                    }
                };
                let Some(kind) = kind else { continue };
                let name = match unsafe { element.CachedName() } {
                    Ok(name) => name.to_string(),
                    Err(err) => {
                        miss(err);
                        continue;
                    }
                };
                if rules.keeps_control(kind, &name) {
                    result.controls.push(ControlFact { kind, name });
                }
            }
            result.incomplete = scan_shortfall(missed, first.as_deref(), requires_document, documents, prepare_error);
            result
        }
    }

    pub struct Observer {
        com: bool,
        uia: Option<Uia>,
        /// UI Automation を最後に使えなかった時刻と理由（[`RETRY_INTERVAL`] たつまでは作り直さない）
        failed: Option<(Instant, String)>,
        nudged: HashMap<u64, Instant>,
        /// ブラウザの窓ごとに、前にサイトを読んだときの題と、読んだサイト
        sites: HashMap<u64, (String, Option<String>)>,
        own_pid: u32,
    }

    impl Observer {
        pub fn new() -> Self {
            // UI Automation の部品を持ち続けるので、このスレッドの COM は、この道具を捨てるまで初期化したままにする
            let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
            Observer {
                com,
                uia: None,
                failed: None,
                nudged: HashMap::new(),
                sites: HashMap::new(),
                own_pid: std::process::id(),
            }
        }

        pub fn observe(&mut self, rules: &Rules, browsers: &Browsers, meeting_windows: &[u64], scan: bool) -> Facts {
            let mut windows = list_windows(self.own_pid, &|exe| rules.is_target(exe, browsers));
            let own = self.own_pid;
            let without_own = |mut sessions: audio_sessions::AudioSessions| {
                sessions.sessions.retain(|session| session.pid != own);
                sessions
            };
            let capture = without_own(audio_sessions::read(Flow::Capture, &|_| false));
            let render = without_own(audio_sessions::read(Flow::Render, &|exe| rules.wants_peak(exe, browsers)));
            let mut read_sites: Vec<u64> = Vec::new();
            let scan = scan.then(|| {
                let targets = scan_targets(rules, browsers, &windows, meeting_windows);
                self.scan(rules, browsers, &mut windows, &targets, &mut read_sites)
            });
            // この回にサイトを読まなかったブラウザの窓は、題が変わっていなければ、前に読んだサイトのまま
            for window in windows.iter_mut().filter(|window| browsers.contains(&window.exe) && !read_sites.contains(&window.hwnd)) {
                if let Some((title, site)) = self.sites.get(&window.hwnd) {
                    if *title == window.title {
                        window.site = site.clone();
                    }
                }
            }
            self.sites.retain(|hwnd, _| windows.iter().any(|window| window.hwnd == *hwnd));
            Facts { windows, capture, render, scan }
        }

        /// UI Automation の道具。作れなかったときは、間隔を置いて作り直す（一時的な失敗から、起動し直さずに戻る）。
        fn ensure_uia(&mut self) -> Result<(), String> {
            if self.uia.is_some() {
                return Ok(());
            }
            if let Some((at, error)) = &self.failed {
                if at.elapsed() < RETRY_INTERVAL {
                    return Err(error.clone());
                }
            }
            match Uia::new() {
                Ok(uia) => {
                    self.uia = Some(uia);
                    self.failed = None;
                    Ok(())
                }
                Err(error) => {
                    self.failed = Some((Instant::now(), error.clone()));
                    Err(error)
                }
            }
        }

        fn scan(
            &mut self,
            rules: &Rules,
            browsers: &Browsers,
            windows: &mut [WindowFact],
            targets: &[u64],
            read_sites: &mut Vec<u64>,
        ) -> ControlScan {
            let started = Instant::now();
            if let Err(error) = self.ensure_uia() {
                return ControlScan { error: Some(error), ..Default::default() };
            }
            let Some(uia) = self.uia.as_mut() else { return ControlScan::default() };
            let condition = match uia.condition(&rules.control_types()) {
                Ok(condition) => condition,
                Err(error) => return ControlScan { error: Some(error), ..Default::default() },
            };
            let uia = &*uia;
            let mut scanned = Vec::new();
            for hwnd in targets {
                let Some(window) = windows.iter_mut().find(|window| window.hwnd == *hwnd) else { continue };
                if unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsHungAppWindow(*hwnd as usize as _) } != 0 {
                    scanned.push(ScannedWindow { hwnd: *hwnd, error: Some("応答のない窓".into()), ..Default::default() });
                    continue;
                }
                // 本文を読めるようにする準備に失敗したときは、次の回にも問い合わせ直し、この回は調べ切れなかった回にする
                let mut prepare_error = None;
                if window.class == CHROMIUM_WINDOW {
                    let due = self.nudged.get(hwnd).is_none_or(|at| at.elapsed() >= NUDGE_INTERVAL);
                    if due {
                        match unsafe { nudge_chromium(*hwnd) } {
                            Ok(()) => {
                                self.nudged.insert(*hwnd, Instant::now());
                            }
                            Err(error) => {
                                self.nudged.remove(hwnd);
                                prepare_error = Some(error);
                            }
                        }
                    }
                }
                let engine = Engine::of_window(&window.class);
                if browsers.contains(&window.exe) {
                    window.site = engine.and_then(|engine| uia.site(*hwnd, engine));
                    self.sites.insert(*hwnd, (window.title.clone(), window.site.clone()));
                    read_sites.push(*hwnd);
                }
                scanned.push(uia.scan_window(*hwnd, &condition, rules, engine.is_some(), prepare_error.as_deref()));
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
    /// 部品を読めるようにする（MTG-030）。子の窓がない・どれも応答しない・どれにも問い合わせられないときは、その理由。
    unsafe fn nudge_chromium(hwnd: u64) -> Result<(), String> {
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
        if children.is_empty() {
            return Err("Web の部分の子の窓がない".into());
        }
        let mut last = "応答のない子の窓".to_string();
        for child in children {
            if IsHungAppWindow(child as _) != 0 {
                continue;
            }
            let mut object: *mut core::ffi::c_void = std::ptr::null_mut();
            let asked = AccessibleObjectFromWindow(HWND(child as *mut _), OBJID_CLIENT as u32, &IAccessible::IID, &mut object);
            if let Err(err) = asked {
                last = code(&err);
                continue;
            }
            if object.is_null() {
                last = "MSAA の部品がない".into();
                continue;
            }
            let accessible = IAccessible::from_raw(object);
            // 名前を読んだことで、Chromium は支援技術が使っているとみなす
            match accessible.get_accName(&variant_i32(0)) {
                Ok(_) => return Ok(()),
                Err(err) => last = code(&err),
            }
        }
        Err(last)
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
        Some(WindowFact { hwnd: hwnd as u64, exe, class: utf16(&class, class_len), title, visible, site: None })
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
        WindowFact { hwnd, exe: exe.into(), class: "Chrome_WidgetWin_1".into(), title: title.into(), visible, site: None }
    }

    /// アドレスバーの値からは、ホスト名だけを取り出す（パス・クエリ・ポート・利用者名は捨てる）。検索語の入力中は取り出さない。
    // [MTG-031]
    #[test]
    fn only_the_host_is_taken_from_the_address_bar() {
        assert_eq!(host_of("https://meet.google.com/abc-defg-hij?authuser=1").as_deref(), Some("meet.google.com"));
        assert_eq!(host_of("meet.google.com/abc-defg-hij").as_deref(), Some("meet.google.com"), "Chromium はスキームを省く");
        assert_eq!(host_of("http://127.0.0.1:8080/").as_deref(), Some("127.0.0.1"));
        assert_eq!(host_of("localhost:3000/x").as_deref(), Some("localhost"));
        assert_eq!(host_of("https://user@App.Slack.com/client").as_deref(), Some("app.slack.com"));
        assert_eq!(host_of("http://[::1]:3000/").as_deref(), Some("::1"));
        assert_eq!(host_of("meet google"), None);
        assert_eq!(host_of(""), None);
        assert_eq!(host_of("会議"), None);
        assert_eq!(host_of("chrome://settings"), None);
        assert_eq!(host_of("about:blank"), None);
    }

    /// パス・クエリ・フラグメント・利用者名の中にある別の URL は、表示中のページのホスト名としない（外側の URL のホストだけ）。
    // [MTG-031]
    #[test]
    fn urls_inside_the_path_or_query_are_not_the_host() {
        for (address, host) in [
            ("evil.example/redirect?to=https://meet.google.com/abc", "evil.example"),
            ("evil.example/https://meet.google.com/abc", "evil.example"),
            ("evil.example/#https://meet.google.com", "evil.example"),
            ("https://evil.example/?u=https://meet.google.com", "evil.example"),
            ("https://meet.google.com@evil.example/", "evil.example"),
            ("evil.example\\@meet.google.com/", "evil.example"),
            ("HTTPS://EVIL.EXAMPLE/http://meet.google.com", "evil.example"),
        ] {
            assert_eq!(host_of(address).as_deref(), Some(host), "{address}");
        }
        assert_eq!(host_of("ftp://meet.google.com/"), None, "HTTP(S) のほかのスキーム");
        assert_eq!(host_of("javascript:https://meet.google.com"), None);
    }

    fn part(kind: PartKind, class_name: &str, automation_id: &str) -> UiPart {
        UiPart { kind, class_name: class_name.into(), automation_id: automation_id.into() }
    }

    /// アドレスバーは、ブラウザの種類ごとの見分け方で、ブラウザ自身のツールバーの中の部品だけ。Firefox 150 の入力欄は ComboBox。
    /// ページの本文の中に同じクラス名・識別子の入力欄を置いても、アドレスバーとしない。知らないクラス名の窓では読まない。
    // [MTG-031]
    #[test]
    fn the_address_bar_is_the_browsers_own_part() {
        assert_eq!(Engine::of_window("Chrome_WidgetWin_1"), Some(Engine::Chromium));
        assert_eq!(Engine::of_window("MozillaWindowClass"), Some(Engine::Firefox));
        assert_eq!(Engine::of_window("ApplicationFrameWindow"), None);
        assert!(Engine::Chromium.is_address_bar(&part(PartKind::Edit, "OmniboxViewViews", "")));
        assert!(!Engine::Chromium.is_address_bar(&part(PartKind::Edit, "", "urlbar-input")), "Chromium では識別子で見分けない");
        assert!(Engine::Firefox.is_address_bar(&part(PartKind::ComboBox, "", "urlbar-input")), "Firefox 150");
        assert!(Engine::Firefox.is_address_bar(&part(PartKind::Edit, "", "urlbar-input")));
        assert!(!Engine::Firefox.is_address_bar(&part(PartKind::Edit, "OmniboxViewViews", "")));
        assert!(!Engine::Firefox.is_address_bar(&part(PartKind::Other, "", "urlbar-input")));

        use PartKind::{Document, Other, ToolBar};
        assert!(in_browser_toolbar(&[Other, ToolBar, Other]), "ブラウザのツールバーの中");
        assert!(!in_browser_toolbar(&[Other, Other]), "ツールバーの外");
        assert!(!in_browser_toolbar(&[ToolBar, Document, Other]), "ページの本文の中のツールバー");
        assert!(!in_browser_toolbar(&[Other, Document, ToolBar]), "ページの本文の中");
        assert!(is_top_document(&[Other, Other]));
        assert!(!is_top_document(&[Other, Document, Other]), "埋め込まれた本文");
    }

    fn bar(value: &str, focused: bool) -> AddressBar {
        AddressBar { value: value.into(), focused }
    }

    /// 表示中のページのホスト名は、ブラウザが確定したものだけ。入力途中の会議の URL では、表示中のページを会議のサイトとしない。
    /// アドレスバーがない窓（アプリとして開いた窓）・2 つ以上ある窓では読まない。
    // [MTG-031]
    #[test]
    fn only_a_committed_site_is_used() {
        let page = |url: &str| vec![url.to_string()];
        // 本文の URL を読めるとき: アドレスバーのホストが、確定した本文のどれかと同じときだけ
        assert_eq!(confirmed_site(&[bar("meet.google.com/abc", false)], &page("https://meet.google.com/abc")).as_deref(), Some("meet.google.com"));
        assert_eq!(confirmed_site(&[bar("meet.google.com/abc", true)], &page("https://meet.google.com/abc")).as_deref(), Some("meet.google.com"), "入力先でも、確定した本文と同じ");
        assert_eq!(confirmed_site(&[bar("meet.google.com/abc", true)], &page("https://news.example/")), None, "入力途中の会議の URL");
        assert_eq!(confirmed_site(&[bar("meet.google.com/abc", false)], &page("https://news.example/")), None, "入力先を離れた入力途中の値");
        // 本文の URL を読めないとき: アドレスバーが入力先でないときだけ
        assert_eq!(confirmed_site(&[bar("meet.google.com/abc", false)], &[]).as_deref(), Some("meet.google.com"));
        assert_eq!(confirmed_site(&[bar("meet.google.com/abc", true)], &[]), None, "編集中かもしれない");
        assert_eq!(confirmed_site(&[bar("meet.google.com/abc", false)], &page("about:blank")).as_deref(), Some("meet.google.com"), "ホスト名のない本文は数えない");
        // アドレスバーがない・2 つ以上ある
        assert_eq!(confirmed_site(&[], &page("https://meet.google.com/abc")), None, "アプリとして開いた窓");
        assert_eq!(confirmed_site(&[bar("meet.google.com", false), bar("meet.google.com", false)], &[]), None);
        assert_eq!(confirmed_site(&[bar("meet google", false)], &[]), None, "検索語");
    }

    /// 窓 1 つの中を調べ切れなかった回: 部品を読めない、ブラウザや Chromium の部品を使うアプリの窓で本文を 1 つも読めない
    /// （ブラウザの枠だけ）、本文を読めるようにする準備に失敗した。部品の印が 0 件でも、本文を読めていれば調べ切れた回。
    // [MTG-007]
    #[test]
    fn a_scan_needs_the_page_body() {
        assert_eq!(scan_shortfall(0, None, true, 1, None), None, "印に当たる部品が 0 件でも、本文を読めた");
        assert_eq!(scan_shortfall(0, None, false, 0, None), None, "本文のないデスクトップのアプリ（Zoom）");
        assert_eq!(scan_shortfall(0, None, true, 0, None).as_deref(), Some("本文を読めない"), "ブラウザの枠だけ");
        assert_eq!(
            scan_shortfall(0, None, true, 2, Some("Web の部分の子の窓がない")).as_deref(),
            Some("本文を読む準備ができない（Web の部分の子の窓がない）")
        );
        assert_eq!(scan_shortfall(3, Some("0x80040201"), false, 0, None).as_deref(), Some("3 個の部品を読めない（0x80040201）"));
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
