//! 会議の検出で、1 回ごとに読む PC の様子（[`Facts`]）: 対象のアプリの窓、窓の中の部品、録音・再生のオーディオのセッション。
//!
//! 読むのは、判定表（[`crate::meeting_rules`]）の対象のアプリ（会議アプリとブラウザ）のものだけ。
//! - 窓: トップレベルの窓の題・クラス名・見えているか
//! - 部品: UI Automation で、判定表が使う種類（Button・TabItem・Text）の部品の名前と種類。調べる窓は [`scan_targets`]
//! - サイト: 調べるブラウザの窓の、ブラウザ自身のナビゲーション用ツールバーの中のアドレスバーと、最上位の本文の URL から、
//!   ブラウザが表示を確定したページのホスト名（[`selected_page`]）。値は所属を確かめた部品のものだけを読み、パスとクエリは
//!   読んだその場で捨てる。ブラウザの窓の部品は、表示中のページの本文の中のものと、タブの一覧の TabItem だけを読む
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

/// ブラウザの種類（アドレスバーとページの本文の境界の見分け方が違う）。窓のクラス名で決める。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// Chromium 系（Chrome・Edge・Brave など。Electron のアプリも同じクラス名）
    Chromium,
    /// Firefox 系（Firefox・Zen など）
    Firefox,
}

/// Firefox 系のナビゲーション用ツールバー（`<toolbar id="nav-bar">`）の AutomationId。
const FIREFOX_NAVIGATION_TOOLBAR: &str = "nav-bar";

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
    /// ページの作成者もクラス名や識別子を付けられるので、ブラウザ自身のナビゲーション用ツールバーの中か
    /// （[`Engine::in_navigation_toolbar`]）も確かめる。
    pub fn is_address_bar(self, part: &UiPart) -> bool {
        match self {
            Engine::Chromium => part.kind == PartKind::Edit && part.class_name == "OmniboxViewViews",
            Engine::Firefox => matches!(part.kind, PartKind::Edit | PartKind::ComboBox) && part.automation_id == "urlbar-input",
        }
    }

    /// 部品の祖先（近い順。窓そのものは含めない）に、ページの本文との境界があるか（その部品はページの中）。
    /// - Chromium 系: ページの最上位の本文（RootWebArea）は、ページの作成者が役割を変えても Document になる
    /// - Firefox 系: 本文の役割はページの作成者が変えられる（role="application" の本文は Pane になる）。どのページも
    ///   ブラウザの枠（`<browser>`。役割 INTERNAL_FRAME）の下にあり、その枠は Pane になるので、Pane も境界とみなす
    pub fn in_page(self, ancestors: &[UiPart]) -> bool {
        ancestors.iter().any(|part| match self {
            Engine::Chromium => part.kind == PartKind::Document,
            Engine::Firefox => matches!(part.kind, PartKind::Document | PartKind::Pane),
        })
    }

    /// ブラウザ自身のナビゲーション用ツールバーか。Firefox 系は AutomationId が nav-bar の ToolBar。Chromium 系は ToolBar
    /// （ページの部品は必ず本文の Document の下にあるので、ページの外の ToolBar はブラウザ自身のもの）。
    fn is_navigation_toolbar(self, part: &UiPart) -> bool {
        part.kind == PartKind::ToolBar
            && match self {
                Engine::Chromium => true,
                Engine::Firefox => part.automation_id == FIREFOX_NAVIGATION_TOOLBAR,
            }
    }

    /// アドレスバーの祖先から、ブラウザ自身のナビゲーション用ツールバーの中か: ページとの境界がなく、いちばん近い ToolBar が
    /// ナビゲーション用ツールバーのとき。ツールバーを確かめられない窓では、アドレスバーとして読まない（会議を始めない）。
    pub fn in_navigation_toolbar(self, ancestors: &[UiPart]) -> bool {
        !self.in_page(ancestors)
            && ancestors.iter().find(|part| part.kind == PartKind::ToolBar).is_some_and(|toolbar| self.is_navigation_toolbar(toolbar))
    }

    /// 部品の祖先から、ブラウザ自身のタブの一覧（Tab）の中の部品か（ページの中のタブは含めない）。
    pub fn in_tab_strip(self, ancestors: &[UiPart]) -> bool {
        !self.in_page(ancestors) && ancestors.iter().any(|part| part.kind == PartKind::Tab)
    }
}

/// アドレスバー・ページの本文・タブの一覧・メッセージの一覧を見分けるのに使う部品の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartKind {
    Edit,
    ComboBox,
    /// ページの本文（ページの作成者が中身を決める）
    Document,
    ToolBar,
    /// Firefox 系では、ページを入れるブラウザの枠と、role="application" の本文
    Pane,
    /// タブの一覧
    Tab,
    /// 一覧の項目（チャットのメッセージなど）
    ListItem,
    Other,
}

/// 部品の種類と、見分けに使う値。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiPart {
    pub kind: PartKind,
    pub class_name: String,
    pub automation_id: String,
}

impl UiPart {
    /// 種類だけの部品（クラス名と識別子が空）。
    pub fn of(kind: PartKind) -> Self {
        UiPart { kind, class_name: String::new(), automation_id: String::new() }
    }
}

/// 本文（Document）の祖先から、その本文が窓の最上位の本文（埋め込まれた本文でない）か。
pub fn is_top_document(ancestors: &[UiPart]) -> bool {
    !ancestors.iter().any(|part| part.kind == PartKind::Document)
}

/// 部品の祖先（本文まで）から、一覧の項目（チャットのメッセージなど）の中の部品か。祖先をたどれないときは `None`。
pub fn in_list_item(ancestors: Option<&[UiPart]>) -> bool {
    ancestors.is_none_or(|ancestors| ancestors.iter().any(|part| part.kind == PartKind::ListItem))
}

/// 表示中のページ: ブラウザ自身のアドレスバーが 1 つだけで、そのホスト名と同じホスト名の最上位の本文（ブラウザが表示を
/// 確定したページ）が 1 つだけのとき、そのホスト名と、その本文の位置。アドレスバーが入力先かどうかを問わず、本文の URL と
/// 合わないアドレスバーの値（入力途中や、別の窓に移る前に編集した値）は使わない。本文の URL を読めないとき・合う本文が
/// 2 つ以上あるとき（どれが表示中か分からない）は `None`。
pub fn selected_page(bars: &[String], documents: &[Option<String>]) -> Option<(String, usize)> {
    let [bar] = bars else { return None };
    let host = host_of(bar)?;
    let mut matching = documents
        .iter()
        .enumerate()
        .filter(|(_, url)| url.as_deref().and_then(host_of).as_deref() == Some(host.as_str()))
        .map(|(at, _)| at);
    let at = matching.next()?;
    matching.next().is_none().then_some((host, at))
}

/// 窓 1 つの中を調べた結果のうち、調べ切れなかった理由（組み合わせの開始に要る調べた回に数えない。MTG-007）。
/// - `missed`・`first_error`: 読めなかった部品の数と、最初の理由
/// - `requires_document`: ブラウザや Chromium の部品を使うアプリの窓（ページの本文の中に参加ボタンなどがある）
/// - `documents`: 見つかった本文（Document）の数。0 なら、ブラウザの枠だけを読み、本文を読めていない
/// - `prepare_error`: 本文を読めるようにする準備（Chromium への MSAA の問い合わせ）の失敗
/// - `page_error`: ブラウザの窓で、表示中のページを確かめられなかった理由（[`selected_page`]・調べている間のページの変化）
pub fn scan_shortfall(
    missed: usize,
    first_error: Option<&str>,
    requires_document: bool,
    documents: usize,
    prepare_error: Option<&str>,
    page_error: Option<&str>,
) -> Option<String> {
    let mut reasons = Vec::new();
    if let Some(error) = prepare_error {
        reasons.push(format!("本文を読む準備ができない（{error}）"));
    }
    if let Some(error) = page_error {
        reasons.push(error.to_string());
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
    /// 一覧の項目（チャットのメッセージなど）の中の部品か（[`in_list_item`]）。祖先をたどれなかったときも `true`。
    /// 一覧の中を数えない印（[`Rules::cares_about_lists`]）に当たる部品だけ確かめ、ほかは `false`
    pub in_list_item: bool,
}

impl ControlFact {
    /// 一覧の項目の外の部品。
    pub fn new(kind: ControlType, name: impl Into<String>) -> Self {
        ControlFact { kind, name: name.into(), in_list_item: false }
    }

    /// 一覧の項目（チャットのメッセージなど）の中の部品。
    pub fn in_message(kind: ControlType, name: impl Into<String>) -> Self {
        ControlFact { in_list_item: true, ..ControlFact::new(kind, name) }
    }
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
        IUIAutomationCondition, IUIAutomationElement, IUIAutomationElementArray, IUIAutomationTreeWalker, TreeScope_Descendants,
        UIA_AutomationIdPropertyId, UIA_ButtonControlTypeId, UIA_CheckBoxControlTypeId, UIA_ClassNamePropertyId,
        UIA_ComboBoxControlTypeId, UIA_ControlTypePropertyId, UIA_DocumentControlTypeId, UIA_EditControlTypeId,
        UIA_ListItemControlTypeId, UIA_MenuItemControlTypeId, UIA_NamePropertyId, UIA_NativeWindowHandlePropertyId,
        UIA_PaneControlTypeId, UIA_TabControlTypeId, UIA_TabItemControlTypeId, UIA_TextControlTypeId, UIA_ToolBarControlTypeId,
        UIA_ValueValuePropertyId, UIA_CONTROLTYPE_ID,
    };

    use super::{
        in_list_item, is_top_document, scan_shortfall, scan_targets, selected_page, ControlFact, ControlScan, ControlType, Engine,
        Facts, PartKind, ScannedWindow, UiPart, WindowFact,
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
    /// 部品から窓（か本文）までの祖先をたどる段数の上限（超えたら、どこの部品かを確かめられないものとして扱う）。
    const MAX_ANCESTORS: usize = 64;

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

    /// 部品の今の値（ValuePattern の値）。所属を確かめたアドレスバーと最上位の本文だけに使う（SEC-013）。
    fn current_value(element: &IUIAutomationElement) -> Option<String> {
        let value = unsafe { element.GetCurrentPropertyValue(UIA_ValueValuePropertyId) }.ok()?;
        BSTR::try_from(&value).ok().map(|text| text.to_string())
    }

    /// UI Automation で部品を調べる道具一式。
    struct Uia {
        automation: IUIAutomation,
        cache: IUIAutomationCacheRequest,
        condition: Option<(Vec<ControlType>, IUIAutomationCondition)>,
        /// 本文（Document）の部品の条件（窓の中を調べるときに、本文を読めたかを確かめる）
        document: IUIAutomationCondition,
        /// タブ（TabItem）の部品の条件（ブラウザのタブの一覧を読む）
        tab_item: IUIAutomationCondition,
        /// アドレスバーと本文を見分けるための、取り置く値（種類・クラス名・識別子。値は取り置かない）と、
        /// ブラウザの種類ごとの条件（アドレスバーか本文）
        site_cache: IUIAutomationCacheRequest,
        chromium_site: IUIAutomationCondition,
        firefox_site: IUIAutomationCondition,
        /// 祖先をたどる道具と、そのときに取り置く値（種類・窓の識別子・クラス名・識別子）
        walker: IUIAutomationTreeWalker,
        parent_cache: IUIAutomationCacheRequest,
    }

    fn part_kind(id: UIA_CONTROLTYPE_ID) -> PartKind {
        match id {
            id if id == UIA_EditControlTypeId => PartKind::Edit,
            id if id == UIA_ComboBoxControlTypeId => PartKind::ComboBox,
            id if id == UIA_DocumentControlTypeId => PartKind::Document,
            id if id == UIA_ToolBarControlTypeId => PartKind::ToolBar,
            id if id == UIA_PaneControlTypeId => PartKind::Pane,
            id if id == UIA_TabControlTypeId => PartKind::Tab,
            id if id == UIA_ListItemControlTypeId => PartKind::ListItem,
            _ => PartKind::Other,
        }
    }

    /// 取り置いた値から、部品の種類と見分けに使う値を読む。
    fn cached_part(element: &IUIAutomationElement) -> Result<UiPart, windows::core::Error> {
        unsafe {
            Ok(UiPart {
                kind: part_kind(element.CachedControlType()?),
                class_name: element.CachedClassName().map(|name| name.to_string()).unwrap_or_default(),
                automation_id: element.CachedAutomationId().map(|id| id.to_string()).unwrap_or_default(),
            })
        }
    }

    /// 表示中のページ（[`selected_page`]）: ホスト名と、その最上位の本文。
    struct Page {
        site: String,
        document: IUIAutomationElement,
    }

    /// 窓 1 つの中を調べた途中の数: 読めなかった部品の数と最初の理由、読めた本文の数。
    #[derive(Default)]
    struct Tally {
        missed: usize,
        first: Option<String>,
        documents: usize,
    }

    impl Tally {
        fn miss(&mut self, err: &windows::core::Error) {
            self.missed += 1;
            self.first.get_or_insert_with(|| code(err));
        }
    }

    /// 部品を集める場所。
    #[derive(Clone, Copy)]
    enum Place {
        /// デスクトップの会議アプリの窓の中すべて
        Window,
        /// ブラウザの表示中のページの本文の中（TabItem は、ページの中のものなので除く）
        Page,
        /// ブラウザ自身のタブの一覧の中の TabItem だけ（[`Engine::in_tab_strip`]）
        TabStrip(Engine),
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
                let tab_item = type_is(UIA_TabItemControlTypeId)?;
                // 一次の列挙では、見分けに要る値だけを取り置く（ページの中の入力欄や埋め込まれた本文の値は読まない。SEC-013）
                let site_cache = automation.CreateCacheRequest().map_err(|err| code(&err))?;
                for property in [UIA_ControlTypePropertyId, UIA_ClassNamePropertyId, UIA_AutomationIdPropertyId] {
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
                for property in [
                    UIA_ControlTypePropertyId,
                    UIA_NativeWindowHandlePropertyId,
                    UIA_ClassNamePropertyId,
                    UIA_AutomationIdPropertyId,
                ] {
                    parent_cache.AddProperty(property).map_err(|err| code(&err))?;
                }
                Ok(Uia {
                    automation,
                    cache,
                    condition: None,
                    document,
                    tab_item,
                    site_cache,
                    chromium_site,
                    firefox_site,
                    walker,
                    parent_cache,
                })
            }
        }

        /// 部品から窓（`hwnd`）までの祖先（近い順。窓そのものは含めない）。窓までたどれないときは `None`。
        /// 本文（Document）の祖先が見つかったら、そこで止める（その部品は本文の中なので、それより上は要らない）。
        fn ancestors(&self, element: &IUIAutomationElement, hwnd: u64) -> Option<Vec<UiPart>> {
            let mut parts = Vec::new();
            let mut current = element.clone();
            for _ in 0..MAX_ANCESTORS {
                let parent = unsafe { self.walker.GetParentElementBuildCache(&current, &self.parent_cache) }.ok()?;
                let handle = unsafe { parent.CachedNativeWindowHandle() }.ok()?;
                if handle.0 as usize as u64 == hwnd {
                    return Some(parts);
                }
                let part = cached_part(&parent).ok()?;
                let document = part.kind == PartKind::Document;
                parts.push(part);
                if document {
                    return Some(parts);
                }
                current = parent;
            }
            None
        }

        /// ブラウザの窓の、表示中のページ（[`selected_page`]）。ブラウザ自身のナビゲーション用ツールバーの中のアドレスバーと、
        /// 最上位の本文を見分けてから、その値（URL）だけを読む。値は、ホスト名を取り出したらその場で捨てる。
        /// アドレスバーのない窓（アプリとして開いた窓など）・ツールバーを確かめられない窓・表示中のページを決められない窓は、理由を返す。
        fn page(&self, hwnd: u64, engine: Engine) -> Result<Page, String> {
            let condition = match engine {
                Engine::Chromium => &self.chromium_site,
                Engine::Firefox => &self.firefox_site,
            };
            let found = unsafe {
                let element = self.automation.ElementFromHandle(HWND(hwnd as usize as *mut _)).map_err(|err| code(&err))?;
                element.FindAllBuildCache(TreeScope_Descendants, condition, &self.site_cache).map_err(|err| code(&err))?
            };
            let mut bars: Vec<IUIAutomationElement> = Vec::new();
            let mut documents: Vec<IUIAutomationElement> = Vec::new();
            for at in 0..unsafe { found.Length() }.map_err(|err| code(&err))? {
                let element = unsafe { found.GetElement(at) }.map_err(|err| code(&err))?;
                let part = cached_part(&element).map_err(|err| code(&err))?;
                if part.kind == PartKind::Document {
                    // 埋め込まれた本文（iframe など）や、たどれない本文は数えない
                    if self.ancestors(&element, hwnd).is_some_and(|ancestors| is_top_document(&ancestors)) {
                        documents.push(element);
                    }
                } else if engine.is_address_bar(&part)
                    && self.ancestors(&element, hwnd).is_some_and(|ancestors| engine.in_navigation_toolbar(&ancestors))
                {
                    bars.push(element);
                }
            }
            let values: Vec<String> = bars.iter().map(|bar| current_value(bar).unwrap_or_default()).collect();
            let urls: Vec<Option<String>> = documents.iter().map(current_value).collect();
            let (site, at) = selected_page(&values, &urls).ok_or_else(|| "表示中のページを確かめられない".to_string())?;
            Ok(Page { site, document: documents.swap_remove(at) })
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

        /// 見つかった部品のうち、判定表の部品の印に当たりうるもの（[`Rules::keeps_control`]）を `result` に足す。
        fn gather(
            &self,
            found: &IUIAutomationElementArray,
            hwnd: u64,
            rules: &Rules,
            place: Place,
            result: &mut ScannedWindow,
            tally: &mut Tally,
        ) {
            let count = match unsafe { found.Length() } {
                Ok(count) => count,
                Err(err) => {
                    tally.miss(&err);
                    return;
                }
            };
            for at in 0..count {
                let element = match unsafe { found.GetElement(at) } {
                    Ok(element) => element,
                    Err(err) => {
                        tally.miss(&err);
                        continue;
                    }
                };
                result.seen += 1;
                let kind = match unsafe { element.CachedControlType() } {
                    Ok(id) if id == UIA_DocumentControlTypeId => {
                        tally.documents += 1;
                        continue;
                    }
                    Ok(id) => control_type(id),
                    Err(err) => {
                        tally.miss(&err);
                        continue;
                    }
                };
                let Some(kind) = kind else { continue };
                let wanted = match place {
                    Place::Window => true,
                    Place::Page => kind != ControlType::TabItem,
                    Place::TabStrip(_) => kind == ControlType::TabItem,
                };
                if !wanted {
                    continue;
                }
                let name = match unsafe { element.CachedName() } {
                    Ok(name) => name.to_string(),
                    Err(err) => {
                        tally.miss(&err);
                        continue;
                    }
                };
                if !rules.keeps_control(kind, &name) {
                    continue;
                }
                let mut fact = ControlFact::new(kind, name);
                match place {
                    Place::TabStrip(engine) => {
                        // ページの中や、ブックマークバー・サイドパネルの部品は、タブの一覧のタブとして数えない
                        if !self.ancestors(&element, hwnd).is_some_and(|ancestors| engine.in_tab_strip(&ancestors)) {
                            continue;
                        }
                    }
                    Place::Window | Place::Page => {
                        if rules.cares_about_lists(kind, &fact.name) {
                            fact.in_list_item = in_list_item(self.ancestors(&element, hwnd).as_deref());
                        }
                    }
                }
                result.controls.push(fact);
            }
        }

        /// デスクトップの会議アプリの窓 1 つの中を調べる。`requires_document` の窓（Chromium の部品を使うアプリ）は、本文を
        /// 1 つも読めなければ、調べ切れなかった回にする。`prepare_error` は、本文を読めるようにする準備の失敗。
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
            let mut tally = Tally::default();
            self.gather(&found, hwnd, rules, Place::Window, &mut result, &mut tally);
            result.incomplete =
                scan_shortfall(tally.missed, tally.first.as_deref(), requires_document, tally.documents, prepare_error, None);
            result
        }

        /// ブラウザの窓 1 つの中を調べ、表示中のページのホスト名も返す。部品は、ブラウザ自身のタブの一覧の TabItem と、
        /// 表示中のページの本文の中のものだけを読む（ブックマークバー・サイドパネル・別の本文の部品は読まない）。
        /// 調べている間に表示中のページが変わったときは、ページの部品とホスト名を捨て、調べ切れなかった回にする。
        fn scan_browser(
            &self,
            hwnd: u64,
            engine: Engine,
            condition: &IUIAutomationCondition,
            rules: &Rules,
            prepare_error: Option<&str>,
        ) -> (ScannedWindow, Option<String>) {
            let mut result = ScannedWindow { hwnd, ..Default::default() };
            let mut tally = Tally::default();
            if rules.control_types().contains(&ControlType::TabItem) {
                let found = unsafe {
                    self.automation
                        .ElementFromHandle(HWND(hwnd as usize as *mut _))
                        .and_then(|element| element.FindAllBuildCache(TreeScope_Descendants, &self.tab_item, &self.cache))
                };
                match found {
                    Ok(found) => self.gather(&found, hwnd, rules, Place::TabStrip(engine), &mut result, &mut tally),
                    Err(err) => {
                        result.error = Some(code(&err));
                        return (result, None);
                    }
                }
            }
            let mut site = None;
            let page_error = match self.page(hwnd, engine) {
                Ok(page) => {
                    tally.documents += 1;
                    match unsafe { page.document.FindAllBuildCache(TreeScope_Descendants, condition, &self.cache) } {
                        Ok(found) => self.gather(&found, hwnd, rules, Place::Page, &mut result, &mut tally),
                        Err(err) => tally.miss(&err),
                    }
                    // 調べている間にタブを切り替えた・ページを移ったときは、前のページのホスト名に、次のページの部品を結び付けない
                    let same = self.page(hwnd, engine).is_ok_and(|after| {
                        after.site == page.site
                            && unsafe { self.automation.CompareElements(&after.document, &page.document) }.is_ok_and(|same| same.as_bool())
                    });
                    if same {
                        site = Some(page.site);
                        None
                    } else {
                        result.controls.retain(|control| control.kind == ControlType::TabItem);
                        Some("調べている間に表示中のページが変わった".to_string())
                    }
                }
                Err(reason) => Some(reason),
            };
            result.incomplete =
                scan_shortfall(tally.missed, tally.first.as_deref(), true, tally.documents, prepare_error, page_error.as_deref());
            (result, site)
        }
    }

    pub struct Observer {
        com: bool,
        uia: Option<Uia>,
        /// UI Automation を最後に使えなかった時刻と理由（[`RETRY_INTERVAL`] たつまでは作り直さない）
        failed: Option<(Instant, String)>,
        nudged: HashMap<u64, Instant>,
        /// ブラウザの窓ごとに、前にサイトを読もうとしたときの題と、読んだサイト（読めなかったときは `None`）
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
            let mut targets: Vec<u64> = Vec::new();
            let scan = scan.then(|| {
                targets = scan_targets(rules, browsers, &windows, meeting_windows);
                self.scan(rules, browsers, &mut windows, &targets, &mut read_sites)
            });
            for window in windows.iter_mut().filter(|window| browsers.contains(&window.exe) && !read_sites.contains(&window.hwnd)) {
                if targets.contains(&window.hwnd) {
                    // 調べようとして調べられなかった窓（UI Automation を使えない回）は、前に読んだサイトを使わない
                    window.site = None;
                    self.sites.insert(window.hwnd, (window.title.clone(), None));
                } else if let Some((title, site)) = self.sites.get(&window.hwnd) {
                    // 調べなかった窓は、題が変わっていなければ、前に読んだサイトのまま
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

        /// 窓の中を調べる。調べようとしたブラウザの窓は `read_sites` に足し、そのサイトを、この回に読めたものだけにする
        /// （応答のない窓・調べ切れなかった窓では、前に読んだサイトを使わない）。
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
                let browser = browsers.contains(&window.exe);
                if browser {
                    window.site = None;
                    read_sites.push(*hwnd);
                }
                if unsafe { windows_sys::Win32::UI::WindowsAndMessaging::IsHungAppWindow(*hwnd as usize as _) } != 0 {
                    scanned.push(ScannedWindow { hwnd: *hwnd, error: Some("応答のない窓".into()), ..Default::default() });
                } else {
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
                    match (browser, engine) {
                        (true, Some(engine)) => {
                            let (result, site) = uia.scan_browser(*hwnd, engine, &condition, rules, prepare_error.as_deref());
                            window.site = site;
                            scanned.push(result);
                        }
                        _ => scanned.push(uia.scan_window(*hwnd, &condition, rules, engine.is_some(), prepare_error.as_deref())),
                    }
                }
                if browser {
                    self.sites.insert(*hwnd, (window.title.clone(), window.site.clone()));
                }
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

        use PartKind::{Document, Other, Pane, Tab, ToolBar};
        let kinds = |kinds: &[PartKind]| kinds.iter().copied().map(UiPart::of).collect::<Vec<_>>();
        let nav_bar = UiPart { automation_id: "nav-bar".into(), ..UiPart::of(ToolBar) };
        assert!(Engine::Chromium.in_navigation_toolbar(&kinds(&[Other, ToolBar, Other, Pane])), "Chromium のツールバーの中");
        assert!(!Engine::Chromium.in_navigation_toolbar(&kinds(&[Other, Other])), "ツールバーの外");
        assert!(!Engine::Chromium.in_navigation_toolbar(&kinds(&[ToolBar, Document])), "ページの本文の中のツールバー");
        assert!(Engine::Firefox.in_navigation_toolbar(&[UiPart::of(Other), nav_bar.clone(), UiPart::of(Other)]), "Firefox の nav-bar");
        assert!(!Engine::Firefox.in_navigation_toolbar(&kinds(&[Other, ToolBar, Other])), "識別子のないツールバー");
        assert!(!Engine::Firefox.in_navigation_toolbar(&[UiPart::of(ToolBar), nav_bar.clone()]), "いちばん近いツールバーが nav-bar でない");
        assert!(is_top_document(&kinds(&[Other, Other])));
        assert!(!is_top_document(&kinds(&[Other, Document])), "埋め込まれた本文");
        // タブの一覧の中のタブだけ
        assert!(Engine::Chromium.in_tab_strip(&kinds(&[Tab, Other, Pane])));
        assert!(!Engine::Chromium.in_tab_strip(&kinds(&[Other, Pane])), "ブックマークバー・サイドパネル");
        assert!(!Engine::Chromium.in_tab_strip(&kinds(&[Tab, Document])), "ページの中のタブ");
        assert!(!Engine::Firefox.in_tab_strip(&kinds(&[Tab, Pane])), "Firefox のページの中のタブ");
    }

    /// Firefox 150 では、本文に role="application" を付けたページの本文は Document でなく Pane になる。そのページの中に、
    /// ツールバー（AutomationId が nav-bar のものも）と urlbar-input の入力欄を置いても、ブラウザの枠（Pane）の下なので、
    /// アドレスバーとしない。
    // [MTG-031]
    #[test]
    fn a_firefox_page_without_a_document_is_still_a_page() {
        use PartKind::{Other, Pane, ToolBar};
        let nav_bar = UiPart { automation_id: "nav-bar".into(), ..UiPart::of(ToolBar) };
        // ページの中の偽のツールバー: ツールバー → 本文（Pane） → ブラウザの枠（Pane） → ブラウザの窓
        let fake = [UiPart::of(Other), nav_bar.clone(), UiPart::of(Other), UiPart::of(Pane), UiPart::of(Pane), UiPart::of(Other)];
        assert!(Engine::Firefox.in_page(&fake));
        assert!(!Engine::Firefox.in_navigation_toolbar(&fake));
        // Chromium 系のページの本文は必ず Document なので、窓の Pane はページの境界でない
        assert!(!Engine::Chromium.in_page(&[UiPart::of(Pane)]));
    }

    /// 一覧の項目（チャットのメッセージ）の中の部品か。祖先をたどれないときは、一覧の中かもしれないものとして扱う。
    // [MTG-032]
    #[test]
    fn buttons_in_a_message_are_in_a_list_item() {
        use PartKind::{Document, ListItem, Other};
        assert!(in_list_item(Some(&[UiPart::of(Other), UiPart::of(ListItem), UiPart::of(Document)])));
        assert!(!in_list_item(Some(&[UiPart::of(Other), UiPart::of(Document)])));
        assert!(in_list_item(None), "たどれない");
    }

    /// 表示中のページのホスト名は、アドレスバーのホスト名と同じホスト名の最上位の本文（ブラウザが表示を確定したページ）が
    /// 1 つだけあるときだけ。入力途中の値・入力先を離れた編集中の値・本文の URL を読めない窓では使わない。
    /// アドレスバーがない窓（アプリとして開いた窓）・2 つ以上ある窓でも使わない。
    // [MTG-031]
    #[test]
    fn only_a_committed_site_is_used() {
        let bar = |value: &str| vec![value.to_string()];
        let pages = |urls: &[&str]| urls.iter().map(|url| Some(url.to_string())).collect::<Vec<_>>();
        assert_eq!(selected_page(&bar("meet.google.com/abc"), &pages(&["https://meet.google.com/abc"])), Some(("meet.google.com".into(), 0)));
        assert_eq!(
            selected_page(&bar("meet.google.com/abc"), &pages(&["https://news.example/", "https://meet.google.com/abc"])),
            Some(("meet.google.com".into(), 1)),
            "サイドパネルの本文があっても、アドレスバーと同じページを選ぶ"
        );
        assert_eq!(selected_page(&bar("meet.google.com/abc"), &pages(&["https://news.example/"])), None, "入力途中・編集したまま離れた値");
        assert_eq!(selected_page(&bar("meet.google.com/abc"), &[]), None, "本文を読めない");
        assert_eq!(selected_page(&bar("meet.google.com/abc"), &[None]), None, "本文の URL を読めない");
        assert_eq!(selected_page(&bar("meet.google.com/abc"), &pages(&["about:blank"])), None, "ホスト名のない本文");
        assert_eq!(
            selected_page(&bar("meet.google.com/abc"), &pages(&["https://meet.google.com/a", "https://meet.google.com/b"])),
            None,
            "どちらが表示中か分からない"
        );
        assert_eq!(selected_page(&[], &pages(&["https://meet.google.com/abc"])), None, "アプリとして開いた窓");
        assert_eq!(selected_page(&[bar("meet.google.com")[0].clone(), "meet.google.com".into()], &pages(&["https://meet.google.com/"])), None);
        assert_eq!(selected_page(&bar("meet google"), &pages(&["https://meet.google.com/"])), None, "検索語");
    }

    /// 窓 1 つの中を調べ切れなかった回: 部品を読めない、ブラウザや Chromium の部品を使うアプリの窓で本文を 1 つも読めない
    /// （ブラウザの枠だけ）、本文を読めるようにする準備に失敗した。部品の印が 0 件でも、本文を読めていれば調べ切れた回。
    // [MTG-007]
    #[test]
    fn a_scan_needs_the_page_body() {
        assert_eq!(scan_shortfall(0, None, true, 1, None, None), None, "印に当たる部品が 0 件でも、本文を読めた");
        assert_eq!(scan_shortfall(0, None, false, 0, None, None), None, "本文のないデスクトップのアプリ（Zoom）");
        assert_eq!(scan_shortfall(0, None, true, 0, None, None).as_deref(), Some("本文を読めない"), "ブラウザの枠だけ");
        assert_eq!(
            scan_shortfall(0, None, true, 2, Some("Web の部分の子の窓がない"), None).as_deref(),
            Some("本文を読む準備ができない（Web の部分の子の窓がない）")
        );
        assert_eq!(scan_shortfall(3, Some("0x80040201"), false, 0, None, None).as_deref(), Some("3 個の部品を読めない（0x80040201）"));
        assert_eq!(
            scan_shortfall(0, None, true, 1, None, Some("調べている間に表示中のページが変わった")).as_deref(),
            Some("調べている間に表示中のページが変わった")
        );
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
