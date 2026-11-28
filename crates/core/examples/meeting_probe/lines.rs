//! 確認用の道具の表示（OS によらない部分）: 判定の様子と、読んだ PC の様子を行にし、前回から変わった行だけを出す。
//!
//! 窓の題・部品の名前・ページのサイトは、`--titles` のときだけ出す（会議の名前・参加者の名前・メールの件名が含まれうるため）。
//! ページのサイトは、判定表のサービスのサイトのときだけホスト名とサービスを出し、ほかのサイトはホスト名を出さない。
//! 部品の名前は、立っている印に実際に当たった部品だけを出し、文字（Text）の部品は本文ではなく当たった判定表の語を出す（製品のログと同じ）。

use std::collections::HashMap;

use kiroscribe_core::audio_sessions::{AudioSessions, Flow};
use kiroscribe_core::debug_log::mask_text;
use kiroscribe_core::meeting_facts::Facts;
use kiroscribe_core::meeting_rules::{matched_controls, Evaluation, Rules};
use kiroscribe_core::meetings::{Browsers, Decision, Judge, MeetingEvent, Phase, ServiceState};

/// 表示する行 1 つ。`key` が同じ行どうしを前回と比べ、`compare` が変わったときだけ `text` を出す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub key: String,
    pub text: String,
    pub compare: String,
}

impl Line {
    fn new(key: String, text: String) -> Self {
        Line { compare: text.clone(), key, text }
    }

    fn compared_by(mut self, compare: String) -> Self {
        self.compare = compare;
        self
    }
}

/// 前回の行を覚え、変化があった行だけを返す。
#[derive(Debug, Default)]
pub struct Differ {
    previous: Vec<(String, String)>,
    seen: bool,
}

impl Differ {
    /// 前回からの変化（新しい行・変わった行・消えた行）。最初の 1 回は、すべての行。
    pub fn changes(&mut self, lines: &[Line]) -> Vec<String> {
        let before: HashMap<&str, &str> = self.previous.iter().map(|(key, compare)| (key.as_str(), compare.as_str())).collect();
        let mut out = Vec::new();
        for line in lines {
            if !self.seen || before.get(line.key.as_str()) != Some(&line.compare.as_str()) {
                out.push(line.text.clone());
            }
        }
        for (key, _) in &self.previous {
            if !lines.iter().any(|line| &line.key == key) {
                out.push(format!("  {key}: 消えた"));
            }
        }
        self.previous = lines.iter().map(|line| (line.key.clone(), line.compare.clone())).collect();
        self.seen = true;
        out
    }
}

fn phase_text(phase: Phase) -> String {
    match phase {
        Phase::Idle => "会議なし".into(),
        Phase::Confirming(app) => format!("開始の確定待ち（{}）", app.label()),
        Phase::Active(app) => format!("会議中（{}）", app.label()),
        Phase::Fading(app) => format!("終了の猶予の間（{}）", app.label()),
    }
}

/// 段階と、サービスごとの判定の行。
pub fn judge_lines(judge: &Judge) -> Vec<Line> {
    let others: Vec<&str> = judge.others().into_iter().map(|app| app.label()).collect();
    let others = if others.is_empty() { "なし".to_string() } else { others.join("・") };
    let mut lines = vec![Line::new("段階".into(), format!("  段階: {}、ほかの会議: {others}", phase_text(judge.phase())))];
    for status in judge.statuses() {
        if status.state == ServiceState::None {
            continue;
        }
        let context = status.context.as_ref().map(|context| format!("（{}）", context.describe())).unwrap_or_default();
        lines.push(Line::new(
            format!("判定/{}", status.app.label()),
            format!("  {}: {}{context} {}", status.app.label(), status.state.label(), status.signals.join("・")),
        ));
    }
    lines
}

/// 録音か再生のセッションの行（対象のアプリのものだけ）。
pub fn session_lines(flow: Flow, sessions: &AudioSessions, rules: &Rules, browsers: &Browsers) -> Vec<Line> {
    let label = flow.label();
    if !sessions.available {
        return vec![Line::new(
            format!("{label}/一覧"),
            format!("  {label}のセッション: 取れない（{}）", sessions.error.as_deref().unwrap_or("Windows 以外")),
        )];
    }
    let targets: Vec<_> = sessions.sessions.iter().filter(|session| rules.is_target(&session.app_exe(), browsers)).collect();
    let mut lines = vec![Line::new(
        format!("{label}/一覧"),
        format!("  {label}のセッション: デバイス {} 台、{} 件（対象のアプリ {} 件）", sessions.endpoints, sessions.sessions.len(), targets.len()),
    )];
    let mut seen: HashMap<String, usize> = HashMap::new();
    for session in targets {
        let name = session.app_exe();
        let ordinal = seen.entry(name.clone()).or_insert(0);
        *ordinal += 1;
        let package = session.package.as_deref().map(|package| format!(" [{package}]")).unwrap_or_default();
        let (sound, level) = match session.peak {
            Some(peak) if session.audible(rules.audible_peak) => (format!(" 音あり（ピーク {peak:.3}）"), "音あり"),
            Some(peak) => (format!(" 音なし（ピーク {peak:.3}）"), "音なし"),
            None => (String::new(), ""),
        };
        let text = format!("  {label} {} {name}{package}{sound}", session.state.name());
        lines.push(
            Line::new(format!("{label}/{name}/{ordinal}"), text).compared_by(format!("{} {level}", session.state.name())),
        );
    }
    lines
}

/// 対象のアプリの窓と、その中の部品の行。立っている印を添える。`titles` のときだけ、題・サイト・当たった部品の名前を出す。
/// 題と部品の名前は、人に渡すファイルにも書くので、API キーの形の文字列・URL のクエリ・利用者のフォルダの名前を伏せる。
pub fn window_lines(rules: &Rules, facts: &Facts, evaluation: &Evaluation, titles: bool) -> Vec<Line> {
    let matched = matched_controls(rules, facts, evaluation);
    let mut lines = Vec::new();
    for window in &facts.windows {
        let mut signals: Vec<String> = Vec::new();
        for service in &evaluation.services {
            for context in service.contexts.iter().filter(|context| context.window == Some(window.hwnd)) {
                for held in &context.signals {
                    signals.push(format!("{} の{}", service.app.label(), held.label()));
                }
            }
        }
        let shown = if window.visible { "表示" } else { "見えない" };
        let signals = if signals.is_empty() { "なし".to_string() } else { signals.join("・") };
        let title = if titles { format!(" 題「{}」", mask_text(&window.title)) } else { String::new() };
        let site = match (&window.site, titles) {
            (Some(site), true) => site_text(rules, site),
            _ => String::new(),
        };
        let label = format!("窓 {} 0x{:x}", window.exe, window.hwnd);
        lines.push(Line::new(
            format!("窓/{:x}", window.hwnd),
            format!("  {label} クラス={} {shown}{title}{site} 印: {signals}", window.class),
        ));
        if let Some(scanned) = facts.scan.as_ref().and_then(|scan| scan.windows.iter().find(|w| w.hwnd == window.hwnd)) {
            let found = match (&scanned.error, titles) {
                (Some(error), _) => format!("調べられない（{error}）"),
                (None, true) => {
                    let names: Vec<String> = matched
                        .iter()
                        .filter(|control| control.window == window.hwnd)
                        .map(|control| format!("{}（{}）", mask_text(&control.name), control.kind.name()))
                        .collect();
                    if names.is_empty() { "印に当たったものなし".into() } else { names.join("・") }
                }
                (None, false) => format!("判定表の名前に当たるもの {} 個", scanned.controls.len()),
            };
            let found = match &scanned.incomplete {
                Some(reason) => format!("{found}（一部を読めない: {reason}）"),
                None => found,
            };
            lines.push(Line::new(
                format!("部品/{:x}", window.hwnd),
                format!("  {label} の部品: {} 個を調べた。{found}", scanned.seen),
            ).compared_by(found));
        }
    }
    lines
}

/// ページのサイトの表示。判定表のサービスのサイト（とそのサブドメイン）のときだけホスト名を出す。
fn site_text(rules: &Rules, site: &str) -> String {
    match rules.services.iter().find(|service| service.is_site(site)) {
        Some(service) => format!(" サイト={site}（{}）", service.app.label()),
        None => " サイト=会議のサービスのサイトではない".to_string(),
    }
}

/// 窓の中を調べた様子の行。
pub fn scan_line(facts: &Facts) -> Option<Line> {
    let scan = facts.scan.as_ref()?;
    let text = match &scan.error {
        Some(error) => format!("  窓の中の部品: UI Automation を使えない（{error}）"),
        None => format!("  窓の中の部品: {} 個の窓を {} ミリ秒で調べた", scan.windows.len(), scan.elapsed_ms),
    };
    let compare = match &scan.error {
        Some(error) => error.clone(),
        None => scan.windows.len().to_string(),
    };
    Some(Line::new("部品/一覧".into(), text).compared_by(compare))
}

/// 開始・終了を確定したことの行。
pub fn decision_line(decision: &Decision) -> String {
    match decision.event {
        MeetingEvent::Started(app) => format!("★ 会議の開始を確定: {}（{}。製品なら録音を始める）", app.label(), decision.reason.describe()),
        MeetingEvent::Ended(app) => format!("★ 会議の終了を確定: {}（{}。製品なら録音を止める）", app.label(), decision.reason.describe()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiroscribe_core::audio_sessions::{AudioSession, SessionState};
    use kiroscribe_core::meeting_facts::{ControlFact, ControlScan, ControlType, ScannedWindow, WindowFact};
    use kiroscribe_core::meeting_rules::evaluate;

    fn facts() -> Facts {
        Facts {
            windows: vec![WindowFact {
                hwnd: 0x1a,
                exe: "chrome.exe".into(),
                class: "Chrome_WidgetWin_1".into(),
                title: "Meet - abc-defg-hij - 山田 - Google Chrome".into(),
                visible: true,
                site: Some("meet.google.com".into()),
            }],
            capture: AudioSessions {
                available: true,
                endpoints: 1,
                sessions: vec![AudioSession { pid: 1, exe_path: r"C:\x\chrome.exe".into(), package: None, state: SessionState::Active, peak: None }],
                ..Default::default()
            },
            render: AudioSessions::default(),
            scan: Some(ControlScan {
                windows: vec![ScannedWindow {
                    hwnd: 0x1a,
                    controls: vec![ControlFact::new(ControlType::Button, "通話から退出")],
                    seen: 30,
                    error: None,
                    incomplete: None,
                }],
                elapsed_ms: 40,
                error: None,
            }),
        }
    }

    /// 判定と、立っている印を出す。窓の題と部品の名前は、--titles のときだけ出す。
    #[test]
    fn lines_show_the_judgment_and_hide_titles_unless_asked() {
        let rules = Rules::builtin();
        let browsers = Browsers::builtin();
        let facts = facts();
        let evaluation = evaluate(rules, &facts, &browsers);
        let mut judge = Judge::new(rules);
        let decisions = judge.tick(&evaluation, 0);
        assert_eq!(decision_line(&decisions[0]), "★ 会議の開始を確定: Google Meet（強い印（chrome.exe の窓）: 退出ボタン。製品なら録音を始める）");
        let text: Vec<String> = judge_lines(&judge).into_iter().map(|line| line.text).collect();
        assert_eq!(text[0], "  段階: 会議中（Google Meet）、ほかの会議: なし");
        assert!(text[1].starts_with("  Google Meet: 参加中（chrome.exe の窓） 退出ボタン（強）"), "{}", text[1]);
        let hidden: Vec<String> = window_lines(rules, &facts, &evaluation, false).into_iter().map(|line| line.text).collect();
        let joined = hidden.join("\n");
        assert!(!joined.contains("山田") && !joined.contains("通話から退出"), "{joined}");
        assert!(joined.contains("印: Google Meet の退出ボタン（強）"), "{joined}");
        let shown = window_lines(rules, &facts, &evaluation, true).into_iter().map(|line| line.text).collect::<Vec<_>>().join("\n");
        assert!(shown.contains("題「Meet - abc-defg-hij - 山田 - Google Chrome」") && shown.contains("通話から退出（Button）"), "{shown}");
        assert!(shown.contains("サイト=meet.google.com（Google Meet）"), "{shown}");
        assert!(!hidden.join("\n").contains("meet.google.com"), "サイトも --titles のときだけ");
        let capture = session_lines(Flow::Capture, &facts.capture, rules, &browsers);
        assert_eq!(capture[1].text, "  録音 active chrome.exe");
        assert_eq!(session_lines(Flow::Render, &facts.render, rules, &browsers)[0].text, "  再生のセッション: 取れない（Windows 以外）");
        assert_eq!(scan_line(&facts).unwrap().text, "  窓の中の部品: 1 個の窓を 40 ミリ秒で調べた");
    }

    /// --titles でも、題と部品の名前の API キーの形の文字列・URL のクエリ・利用者のフォルダの名前は伏せる。
    #[test]
    fn titles_hide_secrets() {
        let mut facts = facts();
        facts.windows[0].title = r"Meet - https://meet.google.com/abc?authuser=SECRETQUERY sk-proj-SECRETKEY123 - Google Chrome".into();
        facts.scan.as_mut().unwrap().windows[0].controls[0].name = r"C:\Users\山田 太郎\x 通話から退出".into();
        let evaluation = evaluate(Rules::builtin(), &facts, &Browsers::builtin());
        let shown = window_lines(Rules::builtin(), &facts, &evaluation, true).into_iter().map(|line| line.text).collect::<Vec<_>>().join("\n");
        for (label, secret) in [("URL のクエリ", "SECRETQUERY"), ("API キー", "SECRETKEY"), ("利用者の名前", "山田")] {
            assert!(!shown.contains(secret), "{label}が残っている");
        }
        assert!(shown.contains("通話から退出（Button）"));
    }

    /// --titles でも、部品の名前は立っている印に実際に当たったものだけを出す。文字の部品は本文ではなく、当たった語を出す
    /// （ほかのサービスの語を含むだけのチャットの本文や、退出後の画面の語を含む会議中のチャットは出さない）。
    #[test]
    fn titles_show_only_the_controls_that_hold_a_signal() {
        let mut facts = facts();
        facts.scan.as_mut().unwrap().windows[0].controls = vec![
            ControlFact::new(ControlType::Button, "通話から退出"),
            ControlFact::new(ControlType::Text, "山田: 明日また再参加します"),
            ControlFact::new(ControlType::Text, "Acme の会議に参加（Join meeting）"),
        ];
        let evaluation = evaluate(Rules::builtin(), &facts, &Browsers::builtin());
        let lines = window_lines(Rules::builtin(), &facts, &evaluation, true);
        let shown = &lines.iter().find(|line| line.key.starts_with("部品/")).unwrap().text;
        assert!(shown.contains("通話から退出（Button）"), "{shown}");
        assert!(!shown.contains("山田") && !shown.contains("再参加") && !shown.contains("Acme"), "{shown}");
    }

    /// --titles でも、会議のサービスのサイトでないページ（会議中に切り替えた別のタブ・会議に似た題の社内のサイト）の
    /// ホスト名は出さない。
    #[test]
    fn titles_show_only_the_hosts_of_the_services() {
        let mut facts = facts();
        facts.windows[0].site = Some("wiki.intra.example.co.jp".into());
        let evaluation = evaluate(Rules::builtin(), &facts, &Browsers::builtin());
        let shown = window_lines(Rules::builtin(), &facts, &evaluation, true).into_iter().map(|line| line.text).collect::<Vec<_>>().join("\n");
        assert!(!shown.contains("intra") && shown.contains("サイト=会議のサービスのサイトではない"), "{shown}");
        facts.windows[0].site = Some("acme.slack.com".into());
        let shown = window_lines(Rules::builtin(), &facts, &evaluation, true).into_iter().map(|line| line.text).collect::<Vec<_>>().join("\n");
        assert!(shown.contains("サイト=acme.slack.com（Slack ハドル）"), "{shown}");
    }

    /// 前回から変わった行と、消えた行だけを出す。
    #[test]
    fn only_changed_lines_are_printed() {
        let mut differ = Differ::default();
        let a = Line::new("a".into(), "  a 1".into());
        let b = Line::new("b".into(), "  b 1".into());
        assert_eq!(differ.changes(&[a.clone(), b.clone()]), vec!["  a 1", "  b 1"]);
        assert!(differ.changes(&[a.clone(), b]).is_empty());
        let b2 = Line::new("b".into(), "  b 2".into());
        assert_eq!(differ.changes(&[a.clone(), b2]), vec!["  b 2"]);
        assert_eq!(differ.changes(&[a]), vec!["  b: 消えた"]);
    }
}
