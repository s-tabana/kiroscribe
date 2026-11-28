//! 会議の検出の判定表: サービスごとの印（強い・中・弱い・打ち消し）と、その当てはめ。
//!
//! 判定表は製品に組み込む（[`BUILTIN`]、`meeting_rules.toml`）。形式と各サービスの表は docs/spec/mtg.md の「判定表」。
//! [`evaluate`] は、1 回分の PC の様子（[`Facts`]）に判定表を当てはめ、サービスごとに、文脈（窓 1 つか、
//! デスクトップの会議アプリ 1 つ）と、そこで立っている印を返す。開始・継続・終了は [`crate::meeting_judge`] が決める。

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::audio_sessions::{AudioSessions, Flow, SessionState};
use crate::meeting_facts::{ControlType, Facts, WindowFact};
use crate::meetings::{Browsers, MeetingApp};

/// 組み込みの判定表。
pub const BUILTIN: &str = include_str!("meeting_rules.toml");

/// 印の強さ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Strength {
    /// 1 つで開始する
    Strong,
    /// 中・弱と合わせて 2 つで開始する
    Medium,
    /// 中・弱と合わせて 2 つで開始する
    Weak,
    /// 打ち消し: その文脈では始めない。会議の文脈に出たら、すぐに終える
    Veto,
}

impl Strength {
    pub fn label(self) -> &'static str {
        match self {
            Strength::Strong => "強",
            Strength::Medium => "中",
            Strength::Weak => "弱",
            Strength::Veto => "打ち消し",
        }
    }
}

/// 印の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    /// トップレベルの窓の題とクラス名
    Window,
    /// 窓の中の部品（UI Automation）
    Control,
    /// オーディオのセッション（録音・再生）
    Session,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRules {
    version: u32,
    end_after_seconds: u32,
    combination_seconds: u32,
    combination_scans: u32,
    audible_peak: f32,
    #[serde(default)]
    words: HashMap<String, Vec<String>>,
    #[serde(rename = "service")]
    services: Vec<RawService>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawService {
    app: MeetingApp,
    #[serde(rename = "signal")]
    signals: Vec<RawSignal>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSignal {
    name: String,
    strength: Strength,
    kind: Kind,
    apps: Vec<String>,
    #[serde(default)]
    title: Vec<String>,
    #[serde(default)]
    names: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    types: Vec<ControlType>,
    #[serde(default)]
    class: Vec<String>,
    #[serde(default)]
    flow: Option<Flow>,
    #[serde(default)]
    audible: bool,
    #[serde(default)]
    after_start: bool,
}

/// 判定表（読み込んで、比べる形にしたもの）。
#[derive(Debug, Clone)]
pub struct Rules {
    pub version: u32,
    /// 強い印も組み合わせもない状態がこれだけ続いたら終了（ミリ秒）
    pub end_after_ms: i64,
    /// 組み合わせで始めるときに、続いている必要がある時間（ミリ秒）
    pub combination_ms: i64,
    /// 組み合わせで始めるときに、その間に窓の中の部品を調べた回数
    pub combination_scans: u32,
    /// 再生の音が鳴っているとみなす音の大きさ（ピーク）
    pub audible_peak: f32,
    pub services: Vec<Service>,
}

/// サービス 1 つの印の並び。
#[derive(Debug, Clone)]
pub struct Service {
    pub app: MeetingApp,
    pub signals: Vec<Signal>,
}

/// 印 1 つ。
#[derive(Debug, Clone)]
pub struct Signal {
    pub name: String,
    pub strength: Strength,
    pub kind: Kind,
    /// 開始のあとの継続の判定にだけ数える
    pub after_start: bool,
    apps: Apps,
    title: Patterns,
    names: Patterns,
    exclude: Patterns,
    types: Vec<ControlType>,
    class: Vec<String>,
    flow: Option<Flow>,
    audible: bool,
}

/// 印の対象のアプリ。
#[derive(Debug, Clone, Default)]
struct Apps {
    /// 対象のブラウザすべて（`browser`）
    browser: bool,
    /// 実行ファイルの名前（小文字）
    exes: Vec<String>,
}

impl Apps {
    fn matches(&self, exe: &str, browsers: &Browsers) -> bool {
        self.exes.iter().any(|known| known == exe) || (self.browser && browsers.contains(exe))
    }
}

/// 名前と題の比べ方（docs/spec/mtg.md の「名前と題の比べ方」）。どれかに当たれば当たり。
#[derive(Debug, Clone, Default)]
pub struct Patterns(Vec<Pattern>);

#[derive(Debug, Clone)]
enum Pattern {
    /// `^語`: その語で始まる（前の未読の印を除いて比べる）
    Prefix(String),
    /// `語$`: その語で終わる（最後の区切りより後ろを除いても比べる）
    Suffix(String),
    /// `語 + 語`: どの語も含む
    All(Vec<String>),
}

/// 比べる前に整える: 小文字にし、ダッシュを「-」にそろえ、幅のない空白（Edge が題の途中に入れる）を除く。
pub fn normalize(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{feff}'))
        .map(|c| if matches!(c, '—' | '–' | '―' | '‐' | '−') { '-' } else { c })
        .collect()
}

/// 先頭の未読の印（「(3) 」「(99+) 」「• 」）を除く。
fn without_unread(text: &str) -> &str {
    let mut rest = text.trim_start();
    loop {
        if let Some(after) = rest.strip_prefix('•') {
            rest = after.trim_start();
            continue;
        }
        if let Some(inner) = rest.strip_prefix('(') {
            if let Some(close) = inner.find(')') {
                let count = &inner[..close];
                if !count.is_empty() && count.chars().all(|c| c.is_ascii_digit() || c == '+') {
                    rest = inner[close + 1..].trim_start();
                    continue;
                }
            }
        }
        return rest;
    }
}

/// 最後の区切り（ダッシュをそろえたあとの「 - 」）より前。ブラウザの窓の題の末尾のブラウザの名前（と、Edge のプロファイルの名前）を除くため。
fn without_last_segment(text: &str) -> Option<&str> {
    text.rfind(" - ").map(|at| text[..at].trim_end())
}

impl Pattern {
    fn parse(raw: &str) -> Result<Self, String> {
        let text = normalize(raw);
        let pattern = if let Some(rest) = text.strip_prefix('^') {
            Pattern::Prefix(rest.to_string())
        } else if let Some(rest) = text.strip_suffix('$') {
            Pattern::Suffix(rest.to_string())
        } else {
            Pattern::All(text.split(" + ").map(str::to_string).collect())
        };
        let empty = match &pattern {
            Pattern::Prefix(word) | Pattern::Suffix(word) => word.trim().is_empty(),
            Pattern::All(words) => words.iter().any(|word| word.trim().is_empty()),
        };
        if empty {
            return Err(format!("空の語があります（{raw}）"));
        }
        Ok(pattern)
    }

    fn matches(&self, text: &str) -> bool {
        match self {
            Pattern::Prefix(word) => without_unread(text).starts_with(word.as_str()),
            Pattern::Suffix(word) => {
                let text = text.trim_end();
                let once = without_last_segment(text);
                let twice = once.and_then(without_last_segment);
                [Some(text), once, twice].into_iter().flatten().any(|candidate| candidate.ends_with(word.as_str()))
            }
            Pattern::All(words) => words.iter().all(|word| text.contains(word.as_str())),
        }
    }
}

impl Pattern {
    /// ログに書く、判定表の語の形（例: 「^meet - 」「再参加」「slack + ハドル」）。
    fn word(&self) -> String {
        match self {
            Pattern::Prefix(word) => format!("^{word}"),
            Pattern::Suffix(word) => format!("{word}$"),
            Pattern::All(words) => words.join(" + "),
        }
    }
}

impl Patterns {
    fn parse(raw: &[String], words: &HashMap<String, Vec<String>>) -> Result<Self, String> {
        let mut patterns = Vec::new();
        for entry in raw {
            match entry.strip_prefix('@') {
                Some(name) => {
                    let list = words.get(name).ok_or(format!("[words] に {name} がありません"))?;
                    for word in list {
                        if word.starts_with('@') {
                            return Err(format!("[words] の {name} の中で @ は使えません"));
                        }
                        patterns.push(Pattern::parse(word)?);
                    }
                }
                None => patterns.push(Pattern::parse(entry)?),
            }
        }
        Ok(Patterns(patterns))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 題か名前が、どれかに当たるか（比べる前に [`normalize`] で整える）。
    pub fn matches(&self, text: &str) -> bool {
        let text = normalize(text);
        self.0.iter().any(|pattern| pattern.matches(&text))
    }

    /// 題か名前が当たった語（比べる前に [`normalize`] で整える）。
    fn matched_word(&self, text: &str) -> Option<String> {
        let text = normalize(text);
        self.0.iter().find(|pattern| pattern.matches(&text)).map(Pattern::word)
    }
}

impl Rules {
    /// 判定表を読む。誤りがあれば、その場所と理由。
    pub fn parse(text: &str) -> Result<Self, String> {
        let raw: RawRules = toml::from_str(text).map_err(|err| err.to_string())?;
        if raw.audible_peak <= 0.0 || raw.audible_peak >= 1.0 {
            return Err("audible_peak は 0 より大きく 1 より小さい値にしてください".into());
        }
        let mut services = Vec::new();
        for service in raw.services {
            if services.iter().any(|known: &Service| known.app == service.app) {
                return Err(format!("{} が 2 回あります", service.app.label()));
            }
            let mut signals: Vec<Signal> = Vec::new();
            for signal in service.signals {
                let place = format!("{} の「{}」", service.app.label(), signal.name);
                if signals.iter().any(|known| known.name == signal.name) {
                    return Err(format!("{place}: 同じ名前の印が 2 つあります"));
                }
                let compiled = compile(signal, &raw.words).map_err(|reason| format!("{place}: {reason}"))?;
                signals.push(compiled);
            }
            services.push(Service { app: service.app, signals });
        }
        Ok(Rules {
            version: raw.version,
            end_after_ms: i64::from(raw.end_after_seconds) * 1000,
            combination_ms: i64::from(raw.combination_seconds) * 1000,
            combination_scans: raw.combination_scans,
            audible_peak: raw.audible_peak,
            services,
        })
    }

    /// 組み込みの判定表（読むのは 1 回だけ）。
    pub fn builtin() -> &'static Rules {
        static RULES: OnceLock<Rules> = OnceLock::new();
        RULES.get_or_init(|| Rules::parse(BUILTIN).expect("組み込みの判定表"))
    }

    pub fn service(&self, app: MeetingApp) -> Option<&Service> {
        self.services.iter().find(|service| service.app == app)
    }

    fn signals(&self) -> impl Iterator<Item = &Signal> {
        self.services.iter().flat_map(|service| service.signals.iter())
    }

    /// 対象のアプリか（判定表の `apps` に書いた実行ファイルか、対象のブラウザ）。
    pub fn is_target(&self, exe: &str, browsers: &Browsers) -> bool {
        browsers.contains(exe) || self.is_desktop_app(exe)
    }

    /// 判定表の `apps` に実行ファイルの名前を書いた、デスクトップの会議アプリか（小文字の名前で比べる）。
    pub fn is_desktop_app(&self, exe: &str) -> bool {
        self.signals().any(|signal| signal.apps.exes.iter().any(|known| known == exe))
    }

    /// 部品の印のある、デスクトップのアプリか（その窓の中を調べる）。
    pub fn scans_desktop(&self, exe: &str) -> bool {
        self.signals().any(|signal| signal.kind == Kind::Control && signal.apps.exes.iter().any(|known| known == exe))
    }

    /// ブラウザの窓の題が、判定表のどれかのサービスの題に当たるか（その窓の中を調べる）。
    pub fn browser_title_matches(&self, title: &str) -> bool {
        self.signals().any(|signal| {
            signal.apps.browser && signal.kind != Kind::Session && !signal.title.is_empty() && signal.title.matches(title)
        })
    }

    /// 窓の中を調べるときに読む部品の種類。
    pub fn control_types(&self) -> Vec<ControlType> {
        let mut types: Vec<ControlType> = Vec::new();
        for signal in self.signals().filter(|signal| signal.kind == Kind::Control) {
            for kind in &signal.types {
                if !types.contains(kind) {
                    types.push(*kind);
                }
            }
        }
        types
    }

    /// 調べた部品のうち、残すものか（どれかの部品の印の種類と名前に当たる）。
    pub fn keeps_control(&self, kind: ControlType, name: &str) -> bool {
        self.signals()
            .any(|signal| signal.kind == Kind::Control && signal.types.contains(&kind) && signal.names.matches(name))
    }

    /// 再生の音の大きさを読むアプリか（再生の音の印の対象）。
    pub fn wants_peak(&self, exe: &str, browsers: &Browsers) -> bool {
        self.signals().any(|signal| signal.kind == Kind::Session && signal.audible && signal.apps.matches(exe, browsers))
    }
}

fn compile(raw: RawSignal, words: &HashMap<String, Vec<String>>) -> Result<Signal, String> {
    if raw.apps.is_empty() {
        return Err("apps を書いてください".into());
    }
    let mut apps = Apps::default();
    for app in &raw.apps {
        if app == "browser" {
            apps.browser = true;
        } else {
            apps.exes.push(app.to_lowercase());
        }
    }
    let title = Patterns::parse(&raw.title, words)?;
    let names = Patterns::parse(&raw.names, words)?;
    let exclude = Patterns::parse(&raw.exclude, words)?;
    let types = if raw.types.is_empty() { vec![ControlType::Button] } else { raw.types };
    match raw.kind {
        Kind::Window if title.is_empty() && raw.class.is_empty() => return Err("window には title か class を書いてください".into()),
        Kind::Control if names.is_empty() => return Err("control には names を書いてください".into()),
        Kind::Session if raw.flow.is_none() => return Err("session には flow（capture か render）を書いてください".into()),
        Kind::Session if raw.audible && raw.flow != Some(Flow::Render) => {
            return Err("audible は flow = \"render\" のときだけ書けます".into())
        }
        _ => {}
    }
    if raw.kind != Kind::Control && !(names.is_empty() && exclude.is_empty()) {
        return Err("names と exclude は control のときだけ書けます".into());
    }
    if raw.kind == Kind::Session && !(title.is_empty() && raw.class.is_empty()) {
        return Err("session には title と class を書けません".into());
    }
    if raw.after_start && raw.strength != Strength::Weak && raw.strength != Strength::Medium {
        return Err("after_start は中・弱い印だけに付けられます".into());
    }
    Ok(Signal {
        name: raw.name,
        strength: raw.strength,
        kind: raw.kind,
        after_start: raw.after_start,
        apps,
        title,
        names,
        exclude,
        types,
        class: raw.class,
        flow: raw.flow,
        audible: raw.audible,
    })
}

/// 立っている印 1 つ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Held {
    pub name: String,
    pub strength: Strength,
    pub kind: Kind,
    /// 開始のあとだけ数える印
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub after_start: bool,
}

impl Held {
    /// ログに書く形（例: 「退出ボタン（強）」）。
    pub fn label(&self) -> String {
        format!("{}（{}）", self.name, self.strength.label())
    }
}

/// 文脈 1 つと、そこで立っている印。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    /// 窓の文脈なら、その窓のアプリの実行ファイルの名前。デスクトップの会議アプリの文脈なら、
    /// 録音・再生の印の立ったアプリの実行ファイルの名前（複数なら「・」でつなぐ）
    pub app: String,
    /// 窓の文脈なら、その窓の識別子（HWND の値）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<u64>,
    /// この回に、この文脈の窓の中の部品を調べたか（デスクトップの会議アプリの文脈では、どれかの窓を調べたか）
    #[serde(default)]
    pub scanned: bool,
    pub signals: Vec<Held>,
}

impl Context {
    /// ログに書く文脈の説明（例: 「chrome.exe の窓」「slack.exe（アプリ）」）。
    pub fn describe(&self) -> String {
        match self.window {
            Some(_) => format!("{} の窓", self.app),
            None => format!("{}（アプリ）", self.app),
        }
    }
}

/// サービス 1 つの、印の立っている文脈。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceEvaluation {
    pub app: MeetingApp,
    pub contexts: Vec<Context>,
}

/// 1 回分の当てはめの結果（印の立っている文脈のあるサービスだけ）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Evaluation {
    pub services: Vec<ServiceEvaluation>,
}

impl Evaluation {
    pub fn service(&self, app: MeetingApp) -> Option<&ServiceEvaluation> {
        self.services.iter().find(|service| service.app == app)
    }
}

/// オーディオのセッションの印が、そのアプリで立っているか。
fn session_holds(signal: &Signal, exe: &str, facts: &Facts, audible_peak: f32) -> bool {
    let sessions: &AudioSessions = match signal.flow {
        Some(Flow::Capture) => &facts.capture,
        Some(Flow::Render) => &facts.render,
        None => return false,
    };
    sessions.sessions.iter().any(|session| {
        session.app_exe() == exe
            && if signal.audible {
                session.audible(audible_peak)
            } else {
                session.state == SessionState::Active
            }
    })
}

/// 窓の印・部品の印が、その窓で立っているか。
fn window_holds(signal: &Signal, window: &WindowFact, facts: &Facts) -> bool {
    if !window.visible || !(signal.title.is_empty() || signal.title.matches(&window.title)) {
        return false;
    }
    match signal.kind {
        Kind::Window => signal.class.is_empty() || signal.class.iter().any(|class| *class == window.class),
        Kind::Control => facts.scanned_window(window.hwnd).is_some_and(|scanned| {
            scanned.controls.iter().any(|control| {
                signal.types.contains(&control.kind) && signal.names.matches(&control.name) && !signal.exclude.matches(&control.name)
            })
        }),
        Kind::Session => false,
    }
}

/// 窓の中の部品のうち、その窓で立っている部品の印に当たったもの 1 つ（詳しい記録に書く）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedControl {
    pub window: u64,
    pub kind: ControlType,
    /// 部品の名前。文字（Text）の部品は、本文が入りうるので、当たった判定表の語
    pub name: String,
}

/// 当てはめの結果で立っている部品の印に、実際に当たった部品（印の立たないサービスの語に当たっただけの部品は含めない）。
pub fn matched_controls(rules: &Rules, facts: &Facts, evaluation: &Evaluation) -> Vec<MatchedControl> {
    let mut found: Vec<MatchedControl> = Vec::new();
    for service in &evaluation.services {
        let Some(table) = rules.service(service.app) else { continue };
        for context in &service.contexts {
            let Some(scanned) = context.window.and_then(|hwnd| facts.scanned_window(hwnd)) else { continue };
            for held in context.signals.iter().filter(|held| held.kind == Kind::Control) {
                let Some(signal) = table.signals.iter().find(|signal| signal.name == held.name) else { continue };
                for control in &scanned.controls {
                    if !signal.types.contains(&control.kind) || signal.exclude.matches(&control.name) {
                        continue;
                    }
                    let name = match control.kind {
                        ControlType::Text => signal.names.matched_word(&control.name),
                        _ => signal.names.matches(&control.name).then(|| control.name.clone()),
                    };
                    let Some(name) = name else { continue };
                    let matched = MatchedControl { window: scanned.hwnd, kind: control.kind, name };
                    if !found.contains(&matched) {
                        found.push(matched);
                    }
                }
            }
        }
    }
    found
}

fn held(signal: &Signal) -> Held {
    Held { name: signal.name.clone(), strength: signal.strength, kind: signal.kind, after_start: signal.after_start }
}

/// 1 回分の PC の様子に判定表を当てはめる。
///
/// - 窓の文脈: 対象のアプリの窓ごとに、その窓の窓の印・部品の印と、その窓のアプリの録音・再生の印。
///   窓の印か部品の印が 1 つも立っていない窓は、文脈にしない（ブラウザのマイクの使用だけでは、会議の印にしない）
/// - デスクトップの会議アプリの文脈: サービスごとに、ブラウザでないアプリの録音・再生の印と、そのアプリの窓の打ち消しの印
pub fn evaluate(rules: &Rules, facts: &Facts, browsers: &Browsers) -> Evaluation {
    let mut services = Vec::new();
    for service in &rules.services {
        let mut contexts = Vec::new();
        let mut desktop_vetoes: Vec<Held> = Vec::new();
        for window in &facts.windows {
            let signals: Vec<&Signal> =
                service.signals.iter().filter(|signal| signal.apps.matches(&window.exe, browsers)).collect();
            if signals.is_empty() {
                continue;
            }
            let held_here: Vec<Held> = signals
                .iter()
                .filter(|signal| match signal.kind {
                    Kind::Session => session_holds(signal, &window.exe, facts, rules.audible_peak),
                    _ => window_holds(signal, window, facts),
                })
                .map(|signal| held(signal))
                .collect();
            if !held_here.iter().any(|held| held.kind != Kind::Session) {
                continue;
            }
            if !browsers.contains(&window.exe) {
                for veto in held_here.iter().filter(|held| held.strength == Strength::Veto) {
                    if !desktop_vetoes.contains(veto) {
                        desktop_vetoes.push(veto.clone());
                    }
                }
            }
            contexts.push(Context {
                app: window.exe.clone(),
                window: Some(window.hwnd),
                scanned: facts.scan_covers(window.hwnd),
                signals: held_here,
            });
        }
        // デスクトップの会議アプリの文脈（録音・再生の印だけ）
        let mut apps: Vec<&str> = Vec::new();
        let mut held_app: Vec<Held> = Vec::new();
        for signal in service.signals.iter().filter(|signal| signal.kind == Kind::Session) {
            let hit = signal.apps.exes.iter().find(|exe| session_holds(signal, exe, facts, rules.audible_peak));
            if let Some(exe) = hit {
                if !apps.contains(&exe.as_str()) {
                    apps.push(exe);
                }
                held_app.push(held(signal));
            }
        }
        if !held_app.is_empty() {
            apps.sort_unstable();
            held_app.extend(desktop_vetoes);
            // そのアプリの窓のどれかの中を、この回に調べられたときだけ、調べた回に数える
            let scanned = facts
                .windows
                .iter()
                .any(|window| apps.contains(&window.exe.as_str()) && facts.scan_covers(window.hwnd));
            contexts.push(Context { app: apps.join("・"), window: None, scanned, signals: held_app });
        }
        if !contexts.is_empty() {
            services.push(ServiceEvaluation { app: service.app, contexts });
        }
    }
    Evaluation { services }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patterns(raw: &[&str]) -> Patterns {
        let words = HashMap::new();
        Patterns::parse(&raw.iter().map(|s| s.to_string()).collect::<Vec<_>>(), &words).unwrap()
    }

    /// 組み込みの判定表は読めて、5 つのサービスがそろう。
    // [MTG-001]
    #[test]
    fn the_builtin_rules_parse_and_cover_every_service() {
        let rules = Rules::builtin();
        let apps: Vec<MeetingApp> = rules.services.iter().map(|s| s.app).collect();
        assert_eq!(
            apps,
            [MeetingApp::GoogleMeet, MeetingApp::SlackHuddle, MeetingApp::Discord, MeetingApp::Teams, MeetingApp::Zoom]
        );
        assert_eq!((rules.end_after_ms, rules.combination_ms, rules.combination_scans), (20_000, 4_000, 2));
        assert_eq!(rules.audible_peak, 0.01);
        assert!(rules.services.iter().all(|s| s.signals.iter().any(|signal| signal.strength == Strength::Strong)));
    }

    /// 判定表の誤りは、どの印のどこが悪いかを返す。
    // [MTG-001]
    #[test]
    fn mistakes_in_a_table_are_reported_with_the_place() {
        let head = "version = 1\nend_after_seconds = 20\ncombination_seconds = 4\ncombination_scans = 2\naudible_peak = 0.01\n";
        let service = |signal: &str| format!("{head}[[service]]\napp = \"zoom\"\n[[service.signal]]\n{signal}");
        let error = Rules::parse(&service("name = \"a\"\nstrength = \"weak\"\nkind = \"session\"\napps = [\"zoom.exe\"]\n")).unwrap_err();
        assert!(error.contains("Zoom の「a」") && error.contains("flow"), "{error}");
        let error = Rules::parse(&service("name = \"a\"\nstrength = \"strong\"\nkind = \"control\"\napps = [\"zoom.exe\"]\nnames = [\"@nothing\"]\n")).unwrap_err();
        assert!(error.contains("nothing"), "{error}");
        let error = Rules::parse(&service("name = \"a\"\nstrength = \"strong\"\nkind = \"window\"\napps = [\"zoom.exe\"]\nclass = [\"X\"]\nafter_start = true\n")).unwrap_err();
        assert!(error.contains("after_start"), "{error}");
        assert!(Rules::parse(&service("name = \"a\"\nstrength = \"strong\"\nkind = \"window\"\napps = [\"zoom.exe\"]\nclass = [\"X\"]\ncolor = 1\n")).is_err(), "知らない項目");
    }

    /// 題と名前は、小文字・ダッシュをそろえて比べる。始まり・終わり・すべてを含むの書き方がある。
    // [MTG-028]
    #[test]
    fn patterns_compare_normalized_titles() {
        let meet = patterns(&["^meet - "]);
        assert!(meet.matches("Meet - abc-defg-hij - Google Chrome"));
        assert!(meet.matches("Meet – abc-defg-hij — Zen Browser"), "全角・半角のダッシュをそろえる");
        assert!(meet.matches("(2) Meet - abc"), "前の未読の印を除く");
        assert!(!meet.matches("Meetup - Google Chrome"));
        let discord = patterns(&[" | discord$", " - discord$"]);
        assert!(discord.matches("#general | Acme - Discord - Google Chrome"), "末尾のブラウザの名前を除いても比べる");
        assert!(discord.matches("#general | Acme - Discord — Zen Browser"), "区切りが「 — 」でもよい");
        assert!(discord.matches("#general | Acme - Discord - 個人 - Microsoft\u{200b} Edge"), "Edge のプロファイルの名前と幅のない空白");
        assert!(!discord.matches("Discord の使い方 - ブログ - Google Chrome"));
        let slack = patterns(&["slack + ハドル"]);
        assert!(slack.matches("ハドル: #dev - Acme - Slack - Google Chrome"));
        assert!(!slack.matches("#dev - Acme - Slack - Google Chrome"));
        assert_eq!(normalize("A—B–C"), "a-b-c");
    }
}
