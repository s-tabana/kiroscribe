//! 会議中を示す手がかりの、OS によらない部分: 読んだ値の形、分類、表示する行、前回からの差分。
//!
//! 読み取り（Windows だけ）は `win.rs`。ここは Linux でもテストできる。
//! 表示するのは、実行ファイルの名前・状態・数値・分類だけ。窓の名前・部品の名前・端点の名前は、
//! `--titles` のときだけ出す（チャンネル名・参加者名・機器の持ち主の名前が含まれうるため）。

// Windows 以外では読み取りがないので、表示の部品の多くはテストからしか使わない
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::HashMap;

use kiroscribe_core::meetings::{desktop_app, web_meeting};
use kiroscribe_core::mic_sessions::SessionState;
use kiroscribe_core::mic_store::MicStore;

/// 窓と部品を調べる対象のブラウザ（実行ファイル名、小文字）。製品の一覧（`meetings::BROWSERS`）より広く、
/// マイクを使っているかによらず窓を出す。
pub const BROWSERS: &[&str] = &[
    // Chromium 系
    "chrome.exe",
    "msedge.exe",
    "brave.exe",
    "vivaldi.exe",
    "opera.exe",
    "arc.exe",
    "chromium.exe",
    "thorium.exe",
    // Firefox 系
    "firefox.exe",
    "zen.exe",
    "floorp.exe",
    "librewolf.exe",
    "waterfox.exe",
];

/// 窓と部品を調べる対象か: デスクトップの会議アプリ（Slack・Teams・Zoom・Discord。製品と同じ判定）かブラウザ。
pub fn is_target_exe(exe: &str) -> bool {
    let exe = exe.to_lowercase();
    desktop_app(&exe).is_some() || BROWSERS.contains(&exe.as_str())
}

/// どのプロセスにもある、入力方式（IME）の見えない窓のクラス名（会議の手がかりにならないので出さない）。
const IME_CLASSES: &[&str] = &["IME", "MSCTFIME UI"];

/// 出す窓か: 入力方式の見えない窓は出さない。
pub fn is_listed_window(class: &str, visible: bool) -> bool {
    visible || !IME_CLASSES.contains(&class)
}

/// 録音か再生か。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Capture,
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

/// 有効な端点（録音・再生のデバイス）1 台。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Endpoint {
    /// 列挙の順番（1 から）
    pub index: usize,
    /// 既定のデバイス（コンソール）か
    pub default_console: bool,
    /// 既定の通信デバイスか
    pub default_communications: bool,
    /// 端点の名前（`--titles` のときだけ読む）
    pub name: Option<String>,
}

/// オーディオのセッション 1 件。
#[derive(Debug, Clone, PartialEq)]
pub struct AudioSession {
    /// 端点の列挙の順番（1 から）
    pub endpoint: usize,
    pub pid: u32,
    /// 実行ファイルの名前（小文字。読めなければ unknown）
    pub exe: String,
    pub package: Option<String>,
    pub state: SessionState,
    /// 音の大きさ（IAudioMeterInformation のピーク。0.0〜1.0）。読めなければ `None`
    pub peak: Option<f32>,
    /// プロセスを開けなかった・実行ファイルのパスを読めなかった理由
    pub identity_error: Option<String>,
    /// 実行ファイルの名前を、プロセスを開かずにプロセスの一覧から読んだか
    pub exe_from_list: bool,
    /// セッションの識別子（フルパスは実行ファイルの名前に縮めたもの。[`sanitize_session_id`]）
    pub session_id: Option<String>,
    /// ブラウザの子プロセスなら、その種類と親
    pub child: Option<ChildProcess>,
}

/// ブラウザの子プロセス（Chromium の `--type=utility` の音声サービスなど）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildProcess {
    /// 子プロセスの種類（[`child_kind`]。読めなければ `None`）
    pub kind: Option<String>,
    pub parent_pid: u32,
    pub parent_exe: String,
}

/// 情報を読めなかったセッション 1 つ（捨てずに出す）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionProblem {
    pub endpoint: usize,
    /// 端点の中での順番（1 から）
    pub index: usize,
    pub reason: String,
    pub pid: Option<u32>,
    pub session_id: Option<String>,
}

/// 録音か再生の、端点ごとのセッションの一覧。
#[derive(Debug, Clone, PartialEq)]
pub struct AudioSnapshot {
    pub flow: Flow,
    pub endpoints: Vec<Endpoint>,
    pub sessions: Vec<AudioSession>,
    /// 一覧そのものを取れなかった理由
    pub error: Option<String>,
    /// 一部を読めなかったときの件数と最初の理由
    pub failures: usize,
    pub first_failure: Option<String>,
    /// 情報を読めなかったセッション
    pub problems: Vec<SessionProblem>,
    /// 既定のデバイス（コンソール・通信）を読めなかった理由（有効なデバイスがないときなど）
    pub default_errors: Vec<String>,
}

/// トップレベルの窓 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSnapshot {
    pub hwnd: usize,
    pub exe: String,
    pub class: String,
    pub title: String,
    pub visible: bool,
    pub minimized: bool,
    pub maximized: bool,
    pub width: i32,
    pub height: i32,
}

/// UI Automation で見つけた部品 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Control {
    pub name: String,
    /// 部品の種類（Button・CheckBox など）
    pub kind: &'static str,
}

/// 窓 1 つの中の部品を調べた結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiaWindow {
    pub hwnd: usize,
    pub exe: String,
    /// 調べた部品（ボタン・チェックボックスなど）の数
    pub scanned: usize,
    /// 参加中にだけ出そうな部品（[`control_category`] に当たったもの）
    pub matched: Vec<Control>,
    pub error: Option<String>,
}

/// UI Automation の 1 回分の走査。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UiaScan {
    pub windows: Vec<UiaWindow>,
    /// 走査にかかった時間（ミリ秒）
    pub elapsed_ms: u128,
    /// UI Automation そのものを使えなかった理由
    pub error: Option<String>,
}

/// 子プロセスのコマンドラインから、種類だけを取り出す（パスなどほかの部分は返さない）。
/// - Chromium 系: `--type=utility --utility-sub-type=audio.mojom.AudioService` なら `utility（audio.mojom.AudioService）`
/// - Firefox 系: `-contentproc ... tab` なら `contentproc（tab）`
pub fn child_kind(command_line: &str) -> Option<String> {
    let safe = |value: &str| {
        let value = value.trim_matches('"');
        (!value.is_empty() && value.len() <= 80 && value.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)))
            .then(|| value.to_string())
    };
    let tokens: Vec<&str> = command_line.split_whitespace().collect();
    let flag = |name: &str| tokens.iter().find_map(|t| t.trim_matches('"').strip_prefix(name)).and_then(safe);
    if let Some(kind) = flag("--type=") {
        return Some(match flag("--utility-sub-type=") {
            Some(sub) => format!("{kind}（{sub}）"),
            None => kind,
        });
    }
    if tokens.iter().any(|t| t.trim_matches('"') == "-contentproc") {
        let last = tokens.last().and_then(|t| safe(t)).filter(|t| t.chars().all(|c| c.is_ascii_alphabetic()));
        return Some(match last {
            Some(kind) => format!("contentproc（{kind}）"),
            None => "contentproc".into(),
        });
    }
    None
}

/// セッションの識別子に含まれるフルパスを、実行ファイルの名前（小文字）に縮める。
/// 識別子は `{デバイス}|\Device\HarddiskVolume3\...\chrome.exe%b{...}` のような形で、パスに利用者の名前が入りうる。
pub fn sanitize_session_id(id: &str) -> String {
    id.split('|')
        .map(|part| {
            let (head, tail) = match part.find("%b") {
                Some(at) => part.split_at(at),
                None => (part, ""),
            };
            let head = if head.contains(['\\', '/']) { kiroscribe_core::meetings::exe_name(head) } else { head.to_string() };
            format!("{head}{tail}")
        })
        .collect::<Vec<_>>()
        .join("|")
}

/// 音の大きさの区分（ピークの数値の変化で行を出しすぎないよう、区分が変わったときだけ出し直す）。
pub fn peak_level(peak: f32) -> &'static str {
    if peak < 0.001 {
        "無音"
    } else if peak < 0.02 {
        "小"
    } else if peak < 0.1 {
        "中"
    } else {
        "大"
    }
}

/// 窓の名前の分類（既定ではこの分類だけを出す）。
pub fn title_kind(title: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        return "名前なし".into();
    }
    if let Some(app) = web_meeting(title) {
        return format!("会議らしい名前（製品の判定では {}）", app.label());
    }
    let lower = title.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let english = ["huddle", "meeting", "call", "voice", "webinar"].iter().any(|w| words.contains(w));
    let japanese = ["ハドル", "会議", "ミーティング", "通話", "ボイスチャンネル"].iter().any(|w| lower.contains(w));
    if english || japanese {
        "会議らしい名前".into()
    } else {
        "その他".into()
    }
}

/// 参加中にだけ出そうな部品の区分。部品の名前（小文字にして調べる）から決める。
pub fn control_category(name: &str) -> Option<&'static str> {
    let name = name.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| name.contains(w));
    if has(&["leave", "hang up", "hangup", "disconnect", "end call", "退出", "退室", "切断", "通話を終了", "会議を終了", "通話から退出"]) {
        Some("退出")
    } else if has(&["share screen", "screen share", "share your screen", "present now", "stop sharing", "画面を共有", "画面共有", "共有を停止", "発表"]) {
        Some("画面共有")
    } else if has(&["mute", "microphone", "deafen", "ミュート", "マイク"]) {
        Some("ミュート")
    } else if has(&["camera", "video", "カメラ", "ビデオ"]) {
        Some("カメラ")
    } else {
        None
    }
}

/// ミュートの部品の名前が、「ミュートを解除する」側（いまミュートしていそう）か。
pub fn looks_unmute(name: &str) -> bool {
    let name = name.to_lowercase();
    ["unmute", "turn on microphone", "ミュート解除", "ミュートを解除", "マイクをオン"].iter().any(|w| name.contains(w))
}

pub const CATEGORIES: &[&str] = &["退出", "ミュート", "カメラ", "画面共有"];

/// 表示する行 1 つ。`key` が同じ行どうしを前回と比べ、`compare` が変わったときだけ `text` を出す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub key: String,
    /// 消えたときに出す名前
    pub label: String,
    pub text: String,
    pub compare: String,
}

impl Line {
    pub fn new(key: String, label: String, text: String) -> Self {
        Line { compare: text.clone(), key, label, text }
    }

    fn compared_by(mut self, compare: String) -> Self {
        self.compare = compare;
        self
    }
}

/// 前回の行を覚え、変化があった行だけを返す。
#[derive(Debug, Default)]
pub struct Differ {
    previous: Vec<(String, String, String)>,
    seen: bool,
}

impl Differ {
    /// 前回からの変化（新しい行・変わった行・消えた行）。最初の 1 回は、すべての行。
    pub fn changes(&mut self, lines: &[Line]) -> Vec<String> {
        let before: HashMap<&str, &str> =
            self.previous.iter().map(|(key, _, compare)| (key.as_str(), compare.as_str())).collect();
        let mut out = Vec::new();
        for line in lines {
            if !self.seen || before.get(line.key.as_str()) != Some(&line.compare.as_str()) {
                out.push(line.text.clone());
            }
        }
        for (key, label, _) in &self.previous {
            if !lines.iter().any(|line| &line.key == key) {
                out.push(format!("  {label}: 消失"));
            }
        }
        self.previous = lines.iter().map(|l| (l.key.clone(), l.label.clone(), l.compare.clone())).collect();
        self.seen = true;
        out
    }
}

/// 端点とセッションの行。セッションは、端点・プロセス・実行ファイルの名前ごとに 1 行。
pub fn audio_lines(snapshot: &AudioSnapshot) -> Vec<Line> {
    let flow = snapshot.flow.label();
    if let Some(error) = &snapshot.error {
        let text = format!("  {flow}の端点: 取れない（{error}）");
        return vec![Line::new(format!("{flow}/一覧"), format!("{flow}の端点"), text)];
    }
    let failures = match &snapshot.first_failure {
        Some(first) => format!("、読めなかったもの {} 件（最初の理由 {first}）", snapshot.failures),
        None => String::new(),
    };
    let mut lines = vec![Line::new(
        format!("{flow}/一覧"),
        format!("{flow}の端点"),
        format!("  {flow}の端点: {} 台、セッション {} 件{failures}", snapshot.endpoints.len(), snapshot.sessions.len()),
    )];
    let default_of = |pick: fn(&Endpoint) -> bool| {
        snapshot.endpoints.iter().find(|e| pick(e)).map(|e| format!("端点 {}", e.index)).unwrap_or_else(|| "なし".into())
    };
    let errors = if snapshot.default_errors.is_empty() {
        String::new()
    } else {
        format!("（読めない: {}）", snapshot.default_errors.join("・"))
    };
    lines.push(Line::new(
        format!("{flow}/既定"),
        format!("{flow}の既定のデバイス"),
        format!(
            "  {flow}の既定のデバイス: {}、通信用の既定: {}{errors}",
            default_of(|e| e.default_console),
            default_of(|e| e.default_communications)
        ),
    ));
    for endpoint in &snapshot.endpoints {
        let mut marks = Vec::new();
        if endpoint.default_console {
            marks.push("既定");
        }
        if endpoint.default_communications {
            marks.push("既定の通信");
        }
        let marks = if marks.is_empty() { String::new() } else { format!("（{}）", marks.join("・")) };
        let name = endpoint.name.as_deref().map(|name| format!(" {name}")).unwrap_or_default();
        lines.push(Line::new(
            format!("{flow}/端点/{}", endpoint.index),
            format!("{flow}の端点 {}", endpoint.index),
            format!("  {flow}の端点 {}{marks}{name}", endpoint.index),
        ));
    }
    let mut count: HashMap<(usize, u32, &str), usize> = HashMap::new();
    for session in &snapshot.sessions {
        let ordinal = count.entry((session.endpoint, session.pid, session.exe.as_str())).or_insert(0);
        *ordinal += 1;
        let label = format!("{flow} 端点{} {} pid={}{}", session.endpoint, session.exe, session.pid, suffix(*ordinal));
        let package = session.package.as_deref().map(|p| format!(" [{p}]")).unwrap_or_default();
        let (peak, level) = match session.peak {
            Some(peak) => (format!("ピーク {peak:.3}（{}）", peak_level(peak)), peak_level(peak)),
            None => ("ピーク 読めない".to_string(), "読めない"),
        };
        let browser = if BROWSERS.contains(&session.exe.as_str()) { "（ブラウザ）" } else { "" };
        let mut extra = String::new();
        if session.exe_from_list {
            extra.push_str(" 名前はプロセスの一覧から");
        }
        if let Some(child) = &session.child {
            let kind = child.kind.as_deref().unwrap_or("種類は読めない");
            extra.push_str(&format!(" 子プロセス={kind} 親={} pid={}", child.parent_exe, child.parent_pid));
        }
        if let Some(error) = &session.identity_error {
            extra.push_str(&format!(" プロセスを読めない理由={error}"));
        }
        if session.identity_error.is_some() || session.exe == kiroscribe_core::mic_sessions::UNKNOWN {
            if let Some(id) = &session.session_id {
                extra.push_str(&format!(" 識別子={id}"));
            }
        }
        let text = format!("  {label}{browser}{package}{extra} {} {peak}", session.state.name());
        lines.push(
            Line::new(format!("{flow}/セッション/{}/{}/{}/{ordinal}", session.endpoint, session.pid, session.exe), label, text)
                .compared_by(format!("{} {level}{extra}", session.state.name())),
        );
    }
    for problem in &snapshot.problems {
        let label = format!("{flow} 端点{} の {} 番目のセッション", problem.endpoint, problem.index);
        let pid = problem.pid.map(|pid| format!(" pid={pid}")).unwrap_or_default();
        let id = problem.session_id.as_deref().map(|id| format!(" 識別子={id}")).unwrap_or_default();
        lines.push(Line::new(
            format!("{flow}/読めない/{}/{}", problem.endpoint, problem.index),
            label.clone(),
            format!("  {label}: 読めない（{}）{pid}{id}", problem.reason),
        ));
    }
    lines
}

fn suffix(ordinal: usize) -> String {
    if ordinal > 1 {
        format!(" #{ordinal}")
    } else {
        String::new()
    }
}

/// 会議アプリとブラウザの窓の行。`titles` のときだけ実際の名前を出す。
pub fn window_lines(windows: &[WindowSnapshot], titles: bool) -> Vec<Line> {
    let mut lines = vec![Line::new(
        "窓/一覧".into(),
        "会議アプリ・ブラウザの窓".into(),
        format!("  会議アプリ・ブラウザの窓: {} 個（見えているもの {} 個）", windows.len(), windows.iter().filter(|w| w.visible).count()),
    )];
    for window in windows {
        let label = format!("窓 {} 0x{:x}", window.exe, window.hwnd);
        let shown = if window.minimized {
            "最小化"
        } else if !window.visible {
            "見えない"
        } else if window.maximized {
            "最大化"
        } else {
            "表示"
        };
        let name = if titles {
            format!("{}「{}」", title_kind(&window.title), window.title)
        } else {
            title_kind(&window.title)
        };
        let text = format!(
            "  {label} クラス={} {shown} {}x{} 名前: {name}",
            window.class, window.width, window.height
        );
        lines.push(Line::new(format!("窓/{:x}", window.hwnd), label, text));
    }
    lines
}

/// 走査にかかった時間の区分（区分が変わったときだけ出し直す）。
fn elapsed_level(ms: u128) -> &'static str {
    match ms {
        0..=199 => "0.2 秒未満",
        200..=999 => "1 秒未満",
        1000..=2999 => "3 秒未満",
        _ => "3 秒以上",
    }
}

/// 窓の中の部品の行。既定では区分ごとのあり・なしと部品の種類だけ、`titles` のときは部品の名前も出す。
pub fn uia_lines(scan: &UiaScan, titles: bool) -> Vec<Line> {
    if let Some(error) = &scan.error {
        return vec![Line::new("部品/一覧".into(), "窓の中の部品".into(), format!("  窓の中の部品: 調べられない（{error}）"))];
    }
    let mut lines = vec![Line::new(
        "部品/一覧".into(),
        "窓の中の部品".into(),
        format!("  窓の中の部品: 調べた窓 {} 個、{} ミリ秒", scan.windows.len(), scan.elapsed_ms),
    )
    .compared_by(format!("{} {}", scan.windows.len(), elapsed_level(scan.elapsed_ms)))];
    for window in &scan.windows {
        let label = format!("部品 {} 0x{:x}", window.exe, window.hwnd);
        if let Some(error) = &window.error {
            lines.push(Line::new(format!("部品/{:x}", window.hwnd), label.clone(), format!("  {label}: 調べられない（{error}）")));
            continue;
        }
        let mut found = Vec::new();
        for category in CATEGORIES {
            let controls: Vec<&Control> =
                window.matched.iter().filter(|c| control_category(&c.name) == Some(category)).collect();
            if controls.is_empty() {
                found.push(format!("{category}=なし"));
                continue;
            }
            let mut kinds: Vec<&str> = controls.iter().map(|c| c.kind).collect();
            kinds.sort();
            kinds.dedup();
            let unmute = *category == "ミュート" && controls.iter().any(|c| looks_unmute(&c.name));
            let unmute = if unmute { "・解除の名前" } else { "" };
            found.push(format!("{category}=あり（{}{unmute}）", kinds.join("・")));
        }
        let names = if titles && !window.matched.is_empty() {
            let names: Vec<String> = window.matched.iter().map(|c| format!("「{}」", c.name)).collect();
            format!(" 名前: {}", names.join(" "))
        } else {
            String::new()
        };
        let summary = found.join(" ");
        lines.push(
            Line::new(
                format!("部品/{:x}", window.hwnd),
                label.clone(),
                format!("  {label} {summary}（調べた部品 {} 個）{names}", window.scanned),
            )
            .compared_by(format!("{summary}{names}")),
        );
    }
    lines
}

/// カメラの使用（ConsentStore の webcam）の行。
pub fn camera_lines(store: Option<&MicStore>, format_time: impl Fn(u64) -> String) -> Vec<Line> {
    let text = match store {
        None => "  カメラ: 取れない（Windows 以外）".to_string(),
        Some(store) if !store.opened => "  カメラ（ConsentStore の webcam）: 開けない".to_string(),
        Some(store) => {
            let in_use: Vec<String> = store
                .entries
                .iter()
                .filter(|entry| entry.in_use())
                .map(|entry| kiroscribe_core::meetings::app_name(&entry.id))
                .collect();
            let latest_start = store.entries.iter().map(|e| e.last_start).max().unwrap_or(0);
            let latest_stop = store.entries.iter().filter_map(|e| e.last_stop).max().unwrap_or(0);
            format!(
                "  カメラ（ConsentStore の webcam）: 記録 {} 件、使用中 {}、最新の使用開始 {}、最新の使用終了 {}",
                store.entries.len(),
                if in_use.is_empty() { "なし".into() } else { in_use.join("・") },
                format_time(latest_start),
                format_time(latest_stop)
            )
        }
    };
    vec![Line::new("カメラ".into(), "カメラ".into(), text)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiroscribe_core::mic_store::MicEntry;

    fn session(endpoint: usize, pid: u32, exe: &str, state: SessionState, peak: Option<f32>) -> AudioSession {
        AudioSession {
            endpoint,
            pid,
            exe: exe.into(),
            package: None,
            state,
            peak,
            identity_error: None,
            exe_from_list: false,
            session_id: None,
            child: None,
        }
    }

    fn capture(sessions: Vec<AudioSession>) -> AudioSnapshot {
        AudioSnapshot {
            flow: Flow::Capture,
            endpoints: vec![Endpoint { index: 1, default_console: true, default_communications: true, name: None }],
            sessions,
            error: None,
            failures: 0,
            first_failure: None,
            problems: Vec::new(),
            default_errors: Vec::new(),
        }
    }

    #[test]
    fn only_changed_lines_are_printed_and_gone_sessions_are_reported() {
        let mut differ = Differ::default();
        let first = audio_lines(&capture(vec![
            session(1, 10, "slack.exe", SessionState::Active, Some(0.0)),
            session(1, 20, "chrome.exe", SessionState::Inactive, None),
        ]));
        assert_eq!(differ.changes(&first).len(), first.len(), "最初はすべての行");
        // ピークの数値が変わっても、区分が同じなら出さない
        let same = audio_lines(&capture(vec![
            session(1, 10, "slack.exe", SessionState::Active, Some(0.0004)),
            session(1, 20, "chrome.exe", SessionState::Inactive, None),
        ]));
        assert!(differ.changes(&same).is_empty());
        let changed = audio_lines(&capture(vec![session(1, 10, "slack.exe", SessionState::Inactive, Some(0.05))]));
        let out = differ.changes(&changed);
        assert!(out.iter().any(|l| l.contains("slack.exe pid=10") && l.contains("inactive") && l.contains("ピーク 0.050（中）")), "{out:?}");
        assert!(out.iter().any(|l| l.contains("chrome.exe pid=20") && l.contains("消失")), "{out:?}");
        assert!(out.iter().any(|l| l.contains("セッション 1 件")), "{out:?}");
    }

    #[test]
    fn sessions_of_one_process_are_numbered_and_endpoints_are_marked() {
        let lines = audio_lines(&capture(vec![
            session(1, 30, "zen.exe", SessionState::Active, Some(0.0)),
            session(1, 10, "chrome.exe", SessionState::Active, Some(0.2)),
            session(1, 10, "chrome.exe", SessionState::Inactive, Some(0.0)),
        ]));
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("録音の端点 1（既定・既定の通信）")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("pid=10（ブラウザ） active ピーク 0.200（大）")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("zen.exe pid=30（ブラウザ） active")), "製品の一覧にないブラウザも印を付ける: {texts:?}");
        assert!(texts.iter().any(|t| t.contains("pid=10 #2（ブラウザ） inactive")), "{texts:?}");
        let keys: std::collections::HashSet<&str> = lines.iter().map(|l| l.key.as_str()).collect();
        assert_eq!(keys.len(), lines.len(), "行の key は重ならない");
    }

    #[test]
    fn unidentified_sessions_and_browser_children_are_kept() {
        let mut child = session(1, 40, "chrome.exe", SessionState::Active, Some(0.0));
        child.child = Some(ChildProcess {
            kind: Some("utility（audio.mojom.AudioService）".into()),
            parent_pid: 30,
            parent_exe: "chrome.exe".into(),
        });
        let mut unknown = session(1, 50, "unknown", SessionState::Inactive, None);
        unknown.identity_error = Some("0x80070005".into());
        unknown.session_id = Some(sanitize_session_id(r"{0.0.1.00000000}.{abc}|\Device\HarddiskVolume3\Users\山田\app\Rec.exe%b{1}"));
        let mut listed = session(1, 60, "zen.exe", SessionState::Active, Some(0.0));
        listed.exe_from_list = true;
        let mut snapshot = capture(vec![child, unknown, listed]);
        snapshot.problems.push(SessionProblem { endpoint: 1, index: 4, reason: "0x88890004".into(), pid: Some(70), session_id: None });
        let texts: Vec<String> = audio_lines(&snapshot).into_iter().map(|l| l.text).collect();
        let all = texts.join("\n");
        assert!(all.contains("録音の既定のデバイス: 端点 1、通信用の既定: 端点 1"), "{all}");
        assert!(all.contains("chrome.exe pid=40（ブラウザ） 子プロセス=utility（audio.mojom.AudioService） 親=chrome.exe pid=30 active"), "{all}");
        assert!(all.contains("unknown pid=50 プロセスを読めない理由=0x80070005 識別子={0.0.1.00000000}.{abc}|rec.exe%b{1} inactive"), "{all}");
        assert!(!all.contains("山田"), "フルパスは出さない");
        assert!(all.contains("zen.exe pid=60（ブラウザ） 名前はプロセスの一覧から active"), "{all}");
        assert!(all.contains("録音 端点1 の 4 番目のセッション: 読めない（0x88890004） pid=70"), "{all}");
    }

    #[test]
    fn only_the_kind_of_a_child_process_is_read_from_its_command_line() {
        let chrome = r#""C:\Users\山田\AppData\Local\Google\Chrome\Application\chrome.exe" --type=utility --utility-sub-type=audio.mojom.AudioService --lang=ja --service-sandbox-type=audio --user-data-dir="C:\Users\山田\AppData""#;
        assert_eq!(child_kind(chrome).as_deref(), Some("utility（audio.mojom.AudioService）"));
        assert_eq!(child_kind("chrome.exe --type=renderer --enable-features=x").as_deref(), Some("renderer"));
        let firefox = r#""C:\Program Files\Zen Browser\zen.exe" -contentproc --channel=1 -isForBrowser -prefsLen 3 -parentBuildID 2 {guid} 1234 tab"#;
        assert_eq!(child_kind(firefox).as_deref(), Some("contentproc（tab）"));
        assert_eq!(child_kind(r#""C:\Program Files\Google\Chrome\Application\chrome.exe""#), None, "親のブラウザ");
        assert_eq!(child_kind(r#"x.exe --type=C:\Users\山田"#), None, "パスのような値は返さない");
    }

    #[test]
    fn window_titles_are_hidden_unless_requested() {
        let window = WindowSnapshot {
            hwnd: 0x1a2b,
            exe: "msedge.exe".into(),
            class: "Chrome_WidgetWin_1".into(),
            title: "Meet - abc-defg-hij - Microsoft Edge".into(),
            visible: true,
            minimized: false,
            maximized: false,
            width: 1200,
            height: 800,
        };
        let hidden = window_lines(std::slice::from_ref(&window), false);
        assert!(hidden[1].text.contains("会議らしい名前（製品の判定では Google Meet）"), "{}", hidden[1].text);
        assert!(!hidden[1].text.contains("abc-defg"), "既定では名前を出さない");
        assert!(hidden[1].text.contains("クラス=Chrome_WidgetWin_1 表示 1200x800"));
        let shown = window_lines(&[window], true);
        assert!(shown[1].text.contains("「Meet - abc-defg-hij - Microsoft Edge」"));
    }

    #[test]
    fn titles_are_classified_without_matching_parts_of_words() {
        assert_eq!(title_kind(""), "名前なし");
        assert_eq!(title_kind("Huddle with Sato - Slack"), "会議らしい名前（製品の判定では Slack ハドル）");
        assert_eq!(title_kind("ハドル: #general"), "会議らしい名前");
        assert_eq!(title_kind("Meeting with Sato | Contoso"), "会議らしい名前");
        assert_eq!(title_kind("Zoom Meeting"), "会議らしい名前（製品の判定では Zoom）");
        assert_eq!(title_kind("Recall notes - Google Chrome"), "その他", "call を語の一部として当てない");
        assert_eq!(title_kind("Discord"), "その他");
        assert_eq!(title_kind("#general | 開発 - Discord"), "会議らしい名前（製品の判定では Discord）");
    }

    #[test]
    fn controls_are_categorized_and_names_are_hidden_unless_requested() {
        assert_eq!(control_category("Leave call"), Some("退出"));
        assert_eq!(control_category("通話から退出"), Some("退出"));
        assert_eq!(control_category("Disconnect"), Some("退出"));
        assert_eq!(control_category("Turn off microphone (ctrl + d)"), Some("ミュート"));
        assert_eq!(control_category("Deafen"), Some("ミュート"));
        assert_eq!(control_category("Turn off camera"), Some("カメラ"));
        assert_eq!(control_category("Share screen"), Some("画面共有"));
        assert_eq!(control_category("Reload"), None);
        assert!(looks_unmute("Unmute") && looks_unmute("ミュート解除") && !looks_unmute("Mute"));

        let scan = UiaScan {
            windows: vec![UiaWindow {
                hwnd: 0x10,
                exe: "slack.exe".into(),
                scanned: 42,
                matched: vec![
                    Control { name: "Leave huddle".into(), kind: "Button" },
                    Control { name: "Unmute".into(), kind: "CheckBox" },
                ],
                error: None,
            }],
            elapsed_ms: 120,
            error: None,
        };
        let hidden = uia_lines(&scan, false);
        assert!(hidden[1].text.contains("退出=あり（Button）"), "{}", hidden[1].text);
        assert!(hidden[1].text.contains("ミュート=あり（CheckBox・解除の名前）"));
        assert!(hidden[1].text.contains("カメラ=なし") && hidden[1].text.contains("画面共有=なし"));
        assert!(!hidden[1].text.contains("Leave huddle"), "既定では部品の名前を出さない");
        assert!(uia_lines(&scan, true)[1].text.contains("「Leave huddle」"));
        // 調べた部品の数と時間が少し変わっただけでは出し直さない
        let mut differ = Differ::default();
        differ.changes(&hidden);
        let mut later = scan.clone();
        later.windows[0].scanned = 45;
        later.elapsed_ms = 150;
        assert!(differ.changes(&uia_lines(&later, false)).is_empty());
    }

    #[test]
    fn camera_in_use_comes_from_the_webcam_consent_store() {
        let store = MicStore {
            opened: true,
            entries: vec![
                MicEntry { id: r"C:\Users\山田\AppData\Local\Discord\app-1.0\Discord.exe".into(), last_start: 5, last_stop: Some(0) },
                MicEntry { id: "MSTeams_8wekyb3d8bbwe".into(), last_start: 3, last_stop: Some(4) },
            ],
        };
        let lines = camera_lines(Some(&store), |t| t.to_string());
        assert!(lines[0].text.contains("使用中 discord.exe"), "{}", lines[0].text);
        assert!(!lines[0].text.contains("山田"), "フルパスは出さない");
        assert!(camera_lines(None, |t| t.to_string())[0].text.contains("取れない"));
    }

    #[test]
    fn target_windows_are_meeting_apps_and_browsers() {
        for exe in ["Slack.exe", "ms-teams.exe", "Teams.exe", "Zoom.exe", "DiscordPTB.exe", "chrome.exe", "msedge.exe", "firefox.exe", "brave.exe", "vivaldi.exe", "zen.exe", "floorp.exe", "librewolf.exe", "waterfox.exe", "chromium.exe", "thorium.exe", "Opera.exe", "arc.exe"] {
            assert!(is_target_exe(exe), "{exe}");
        }
        assert!(!is_target_exe("notepad.exe"));
        assert!(!is_listed_window("IME", false) && !is_listed_window("MSCTFIME UI", false));
        assert!(is_listed_window("Chrome_WidgetWin_1", false) && is_listed_window("IME", true));
    }
}
