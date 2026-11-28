//! 判定表: サービスごとの印（強い・中・弱い）の組み合わせで、会議に参加中かを判定する（製品の判定には使わない）。
//!
//! 判定表はデータのファイル（既定は exe と同じ場所の meeting-rules.toml）で持つ。ないときは既定の表（[`DEFAULT_RULES`]）を
//! 書き出し、編集されたら、作り直さずに次の回から使う。
//!
//! 始める・続ける・止める（[`Judge`]）:
//! - 開始: 強い印が 1 つ、または中・弱い印が合わせて 2 つ。ただし打ち消しの印（参加前の待機画面のボタンなど）があれば始めない
//! - 継続: 強い印がある間（打ち消しの印がある間は、強い印がないものとして扱う）
//! - 終了: 強い印が消えて、表で指定した秒数のあいだ戻らない。終わったあとは、開始の条件がいったん崩れるまで始め直さない
//!   （強い印がなく、中・弱い印だけが続いているときに、始めて止めてを繰り返さないため）

use std::collections::HashMap;

use serde::Deserialize;

use crate::signals::{control_category, AudioSnapshot, UiaScan, WindowSnapshot, BROWSERS};

/// 既定の判定表の版の印。読んだファイルにこれがなければ、古い版の既定の表から書き出したものかもしれないと知らせる。
pub const VERSION_MARK: &str = "判定表の版: 2";

/// 既定の判定表。初めて動かしたときに、exe と同じ場所に書き出す。
pub const DEFAULT_RULES: &str = r#"# Kiroscribe 会議の検出の確認用の道具（meeting_probe）の判定表。
# 判定表の版: 2
# 編集して保存すると、次の回（2 秒ごと）から使う。書き方を誤ると、その旨を出して前の表を使い続ける。
# 新しい版の既定の表を使うには、このファイルを消して起動し直す。
#
# 判定:
# - 開始: 強い印（strong）が 1 つ、または中（medium）・弱い（weak）印が合わせて 2 つ。
#   ただし打ち消しの印（veto。参加前の待機画面の「今すぐ参加」など）があれば始めない
# - 継続: 強い印がある間（打ち消しの印がある間は、強い印がないものとして扱う）
# - 終了: 強い印が消えて end_after_seconds 秒のあいだ戻らない
# - 窓の名前と部品の名前は、全角・半角のダッシュ（— – -）を同じものとして比べる
#
# 印の種類（kind）:
# - control: 窓の中の部品（UI Automation）。category は 退出・参加・ミュート・カメラ・画面共有。
#   names を書くと、部品の名前（小文字にして比べる）にどれかを含むものだけ
# - window: 窓。title は窓の名前（小文字にして比べる）にどれかを含むもの、class は窓のクラス名のどれかと同じもの
# - session: オーディオのセッション。flow は capture（録音）か render（再生）。
#   active（既定は true）は状態が Active か、audible（既定は false）は音の大きさが無音でないか
# 共通:
# - apps: 実行ファイルの名前（小文字）。"browser" はブラウザ（一覧のものと、一覧にないが推定したもの）
# - control と window では、title も書くと、その名前の窓に限る

[[service]]
name = "Google Meet"
end_after_seconds = 20

[[service.signal]]
name = "退出ボタン"
strength = "strong"
kind = "control"
category = "退出"
apps = ["browser"]
title = ["meet"]

# 参加前の待機画面（題は参加中と同じ「Meet - <会議コード>」で、マイクのプレビューで録音のセッションも Active になる）
[[service.signal]]
name = "待機画面の参加ボタン"
strength = "veto"
kind = "control"
category = "参加"
apps = ["browser"]
title = ["meet"]

[[service.signal]]
name = "Meet の窓の名前"
strength = "medium"
kind = "window"
apps = ["browser"]
title = ["meet - ", "google meet", "meet.google.com"]

[[service.signal]]
name = "ブラウザのマイク"
strength = "weak"
kind = "session"
flow = "capture"
apps = ["browser"]

[[service.signal]]
name = "ブラウザの再生の音"
strength = "weak"
kind = "session"
flow = "render"
audible = true
apps = ["browser"]

[[service]]
name = "Slack ハドル"
end_after_seconds = 20

[[service.signal]]
name = "退出ボタン"
strength = "strong"
kind = "control"
category = "退出"
apps = ["slack.exe"]

[[service.signal]]
name = "ハドルの窓"
strength = "medium"
kind = "window"
apps = ["slack.exe"]
title = ["huddle", "ハドル"]

[[service.signal]]
name = "Slack のマイク"
strength = "weak"
kind = "session"
flow = "capture"
apps = ["slack.exe"]

[[service.signal]]
name = "Slack の再生の音"
strength = "weak"
kind = "session"
flow = "render"
audible = true
apps = ["slack.exe"]

[[service]]
name = "Discord の通話"
end_after_seconds = 20

[[service.signal]]
name = "切断ボタン"
strength = "strong"
kind = "control"
category = "退出"
apps = ["discord.exe", "discordptb.exe", "discordcanary.exe", "discorddevelopment.exe"]

[[service.signal]]
name = "ブラウザ版の切断ボタン"
strength = "strong"
kind = "control"
category = "退出"
apps = ["browser"]
title = ["discord"]

[[service.signal]]
name = "Discord のマイク"
strength = "weak"
kind = "session"
flow = "capture"
apps = ["discord.exe", "discordptb.exe", "discordcanary.exe", "discorddevelopment.exe"]

[[service.signal]]
name = "Discord の再生の音"
strength = "weak"
kind = "session"
flow = "render"
audible = true
apps = ["discord.exe", "discordptb.exe", "discordcanary.exe", "discorddevelopment.exe"]

[[service]]
name = "Teams"
end_after_seconds = 20

[[service.signal]]
name = "退出ボタン"
strength = "strong"
kind = "control"
category = "退出"
apps = ["ms-teams.exe", "teams.exe"]

[[service.signal]]
name = "ブラウザ版の退出ボタン"
strength = "strong"
kind = "control"
category = "退出"
apps = ["browser"]
title = ["teams"]

[[service.signal]]
name = "参加前の画面の参加ボタン"
strength = "veto"
kind = "control"
category = "参加"
apps = ["ms-teams.exe", "teams.exe"]

[[service.signal]]
name = "会議の窓の名前"
strength = "medium"
kind = "window"
apps = ["ms-teams.exe", "teams.exe"]
title = ["meeting", "会議", "call", "通話"]

[[service.signal]]
name = "Teams のマイク"
strength = "weak"
kind = "session"
flow = "capture"
apps = ["ms-teams.exe", "ms-teams_modulehost.exe", "teams.exe"]

[[service.signal]]
name = "Teams の再生の音"
strength = "weak"
kind = "session"
flow = "render"
audible = true
apps = ["ms-teams.exe", "ms-teams_modulehost.exe", "teams.exe"]

[[service]]
name = "Zoom"
end_after_seconds = 20

[[service.signal]]
name = "退出ボタン"
strength = "strong"
kind = "control"
category = "退出"
apps = ["zoom.exe"]

[[service.signal]]
name = "ミーティングの窓"
strength = "medium"
kind = "window"
apps = ["zoom.exe"]
class = ["ZPContentViewWndClass"]

[[service.signal]]
name = "Zoom のマイク"
strength = "weak"
kind = "session"
flow = "capture"
apps = ["zoom.exe", "cpthost.exe"]

[[service.signal]]
name = "Zoom の再生の音"
strength = "weak"
kind = "session"
flow = "render"
audible = true
apps = ["zoom.exe", "cpthost.exe"]
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Strength {
    Strong,
    Medium,
    Weak,
    /// 打ち消し: 立っている間は始めず、強い印がないものとして扱う
    Veto,
}

impl Strength {
    fn label(self) -> &'static str {
        match self {
            Strength::Strong => "強",
            Strength::Medium => "中",
            Strength::Weak => "弱",
            Strength::Veto => "打ち消し",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Control,
    Window,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlowName {
    Capture,
    Render,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signal {
    pub name: String,
    pub strength: Strength,
    pub kind: Kind,
    pub apps: Vec<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub names: Vec<String>,
    #[serde(default)]
    pub title: Vec<String>,
    #[serde(default)]
    pub class: Vec<String>,
    #[serde(default)]
    pub flow: Option<FlowName>,
    #[serde(default = "yes")]
    pub active: bool,
    #[serde(default)]
    pub audible: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    pub name: String,
    pub end_after_seconds: u64,
    #[serde(rename = "signal")]
    pub signals: Vec<Signal>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    #[serde(rename = "service")]
    pub services: Vec<Service>,
}

/// 判定表を読む。誤りがあれば、その理由。
pub fn parse(text: &str) -> Result<Rules, String> {
    let rules: Rules = toml::from_str(text).map_err(|err| {
        // 位置の行と、理由の行（最後の行）だけにする
        let message = err.to_string();
        let lines: Vec<&str> = message.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        match (lines.first(), lines.last()) {
            (Some(first), Some(last)) if first != last => format!("{first}: {last}"),
            (Some(first), _) => first.to_string(),
            _ => message,
        }
    })?;
    for service in &rules.services {
        for signal in &service.signals {
            let place = format!("{} の「{}」", service.name, signal.name);
            match signal.kind {
                Kind::Control if signal.category.is_none() && signal.names.is_empty() => {
                    return Err(format!("{place}: control には category か names を書いてください"))
                }
                Kind::Session if signal.flow.is_none() => {
                    return Err(format!("{place}: session には flow（capture か render）を書いてください"))
                }
                Kind::Window if signal.title.is_empty() && signal.class.is_empty() => {
                    return Err(format!("{place}: window には title か class を書いてください"))
                }
                _ => {}
            }
        }
    }
    Ok(rules)
}

/// 1 回分の手がかり（判定表を当てはめる材料）。
pub struct Facts<'a> {
    pub windows: &'a [WindowSnapshot],
    pub uia: Option<&'a UiaScan>,
    pub capture: Option<&'a AudioSnapshot>,
    pub render: Option<&'a AudioSnapshot>,
}

fn app_matches(apps: &[String], exe: &str, guessed_browser: bool) -> bool {
    apps.iter().any(|app| {
        let app = app.to_lowercase();
        if app == "browser" {
            BROWSERS.contains(&exe) || guessed_browser
        } else {
            app == exe
        }
    })
}

/// 小文字にし、全角・半角のダッシュ（— – ー以外のもの）を「-」にそろえる。
fn normalize(text: &str) -> String {
    text.to_lowercase().replace(['—', '–', '―', '‐', '−'], "-")
}

fn contains_any(text: &str, words: &[String]) -> bool {
    let text = normalize(text);
    words.iter().any(|word| text.contains(&normalize(word)))
}

fn window_matches(signal: &Signal, window: &WindowSnapshot) -> bool {
    app_matches(&signal.apps, &window.exe, window.guessed_browser.is_some())
        && (signal.title.is_empty() || contains_any(&window.title, &signal.title))
}

/// 印 1 つが、いま立っているか。
pub fn holds(signal: &Signal, facts: &Facts) -> bool {
    match signal.kind {
        Kind::Window => facts.windows.iter().any(|window| {
            window.visible
                && window_matches(signal, window)
                && (signal.class.is_empty() || signal.class.iter().any(|class| *class == window.class))
        }),
        Kind::Control => {
            let Some(scan) = facts.uia else { return false };
            scan.windows.iter().any(|found| {
                let Some(window) = facts.windows.iter().find(|w| w.hwnd == found.hwnd) else { return false };
                window_matches(signal, window)
                    && found.matched.iter().any(|control| {
                        signal.category.as_deref().is_none_or(|category| control_category(&control.name) == Some(category))
                            && (signal.names.is_empty() || contains_any(&control.name, &signal.names))
                    })
            })
        }
        Kind::Session => {
            let snapshot = match signal.flow {
                Some(FlowName::Capture) => facts.capture,
                Some(FlowName::Render) => facts.render,
                None => None,
            };
            let Some(snapshot) = snapshot else { return false };
            snapshot.sessions.iter().any(|session| {
                app_matches(&signal.apps, &session.exe, session.guessed_browser.is_some())
                    && (!signal.active || session.state == kiroscribe_core::mic_sessions::SessionState::Active)
                    && (!signal.audible || session.peak.is_some_and(|peak| crate::signals::peak_level(peak) != "無音"))
            })
        }
    }
}

/// サービス 1 つの判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 印がどれも立っていない
    None,
    /// 印は立っているが、開始の条件を満たしていない
    Waiting,
    /// 参加中（`since_strong_lost` は、強い印が消えた時刻）
    Joined { started_ms: i64, strong_lost_ms: Option<i64> },
}

#[derive(Debug, Default)]
struct ServiceState {
    verdict: Option<Verdict>,
    /// 終わったあと、開始の条件がいったん崩れるのを待っている
    blocked: bool,
}

/// 判定表を 2 秒ごとに当てはめ、サービスごとの判定を覚えておく。
#[derive(Debug, Default)]
pub struct Judge {
    states: HashMap<String, ServiceState>,
}

/// 1 サービスの 1 回分の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub service: String,
    /// 1 行の表示
    pub text: String,
    /// 前回と比べる値（残りの秒数は入れない）
    pub compare: String,
    /// 開始・終了したなら、その知らせ
    pub event: Option<String>,
}

impl Judge {
    pub fn apply(&mut self, rules: &Rules, facts: &Facts, now_ms: i64) -> Vec<Outcome> {
        let mut outcomes = Vec::new();
        for service in &rules.services {
            let held: Vec<(&Signal, bool)> = service.signals.iter().map(|signal| (signal, holds(signal, facts))).collect();
            let count = |strengths: &[Strength]| held.iter().filter(|(s, on)| *on && strengths.contains(&s.strength)).count();
            let veto = count(&[Strength::Veto]);
            let raw_strong = count(&[Strength::Strong]);
            let others = count(&[Strength::Medium, Strength::Weak]);
            // 打ち消しの印がある間は、強い印がないものとして扱い、始めない
            let strong = if veto > 0 { 0 } else { raw_strong };
            let start = veto == 0 && (strong >= 1 || others >= 2);
            let state = self.states.entry(service.name.clone()).or_default();
            if !start {
                state.blocked = false;
            }
            let before = state.verdict.unwrap_or(Verdict::None);
            let mut event = None;
            let verdict = match before {
                Verdict::Joined { started_ms, strong_lost_ms } => {
                    if strong >= 1 {
                        Verdict::Joined { started_ms, strong_lost_ms: None }
                    } else {
                        let lost = strong_lost_ms.unwrap_or(now_ms);
                        if now_ms - lost >= service.end_after_seconds as i64 * 1000 {
                            event = Some(format!("終了（強い印が {} 秒戻らなかった）", service.end_after_seconds));
                            state.blocked = start;
                            if start || others + veto + raw_strong > 0 { Verdict::Waiting } else { Verdict::None }
                        } else {
                            Verdict::Joined { started_ms, strong_lost_ms: Some(lost) }
                        }
                    }
                }
                _ if start && !state.blocked => {
                    event = Some(if strong >= 1 { "開始（強い印）".to_string() } else { "開始（中・弱い印が 2 つ）".to_string() });
                    Verdict::Joined { started_ms: now_ms, strong_lost_ms: (strong == 0).then_some(now_ms) }
                }
                _ if raw_strong + others + veto > 0 => Verdict::Waiting,
                _ => Verdict::None,
            };
            state.verdict = Some(verdict);
            let (status, detail) = match verdict {
                Verdict::None => ("なし".to_string(), String::new()),
                Verdict::Waiting if state.blocked => ("待機".to_string(), "（終了のあと、開始の条件がいったん崩れるまで始めない）".to_string()),
                Verdict::Waiting if veto > 0 => ("待機".to_string(), "（打ち消しの印がある）".to_string()),
                Verdict::Waiting => ("待機".to_string(), String::new()),
                Verdict::Joined { strong_lost_ms: None, .. } => ("参加中".to_string(), String::new()),
                Verdict::Joined { strong_lost_ms: Some(lost), .. } => {
                    let left = (service.end_after_seconds as i64 * 1000 - (now_ms - lost)).max(0) / 1000;
                    ("参加中".to_string(), format!("（強い印なし、あと {left} 秒で終了）"))
                }
            };
            let list = |on: bool| {
                let names: Vec<String> = held
                    .iter()
                    .filter(|(_, held)| *held == on)
                    .map(|(s, _)| format!("{}（{}）", s.name, s.strength.label()))
                    .collect();
                if names.is_empty() { "なし".to_string() } else { names.join("・") }
            };
            let signals = format!("立った: {} 立たない: {}", list(true), list(false));
            let grace = matches!(verdict, Verdict::Joined { strong_lost_ms: Some(_), .. });
            outcomes.push(Outcome {
                service: service.name.clone(),
                text: format!("  判定表 {}: {status}{detail} {signals}", service.name),
                compare: format!("{status} {grace} {} {signals}", state.blocked),
                event,
            });
        }
        outcomes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signals::{AudioSession, Control, Endpoint, Flow, UiaWindow};
    use kiroscribe_core::mic_sessions::SessionState;

    fn window(hwnd: usize, exe: &str, title: &str) -> WindowSnapshot {
        WindowSnapshot {
            hwnd,
            exe: exe.into(),
            class: "Chrome_WidgetWin_1".into(),
            title: title.into(),
            visible: true,
            minimized: false,
            maximized: false,
            width: 800,
            height: 600,
            guessed_browser: None,
        }
    }

    fn audio(flow: Flow, sessions: Vec<(&str, SessionState, f32)>) -> AudioSnapshot {
        AudioSnapshot {
            flow,
            endpoints: vec![Endpoint { index: 1, ..Default::default() }],
            sessions: sessions
                .into_iter()
                .map(|(exe, state, peak)| AudioSession {
                    endpoint: 1,
                    pid: 1,
                    exe: exe.into(),
                    package: None,
                    state,
                    peak: Some(peak),
                    identity_error: None,
                    exe_from_list: false,
                    session_id: None,
                    child: None,
                    guessed_browser: None,
                })
                .collect(),
            error: None,
            failures: 0,
            first_failure: None,
            problems: Vec::new(),
            default_errors: Vec::new(),
        }
    }

    fn leave_button(hwnd: usize, exe: &str) -> UiaScan {
        UiaScan {
            windows: vec![UiaWindow {
                hwnd,
                exe: exe.into(),
                scanned: 10,
                matched: vec![Control { name: "Leave call".into(), kind: "Button" }],
                error: None,
            }],
            elapsed_ms: 10,
            error: None,
        }
    }

    fn meet(outcomes: &[Outcome]) -> &Outcome {
        outcomes.iter().find(|o| o.service == "Google Meet").unwrap()
    }

    #[test]
    fn the_default_rules_parse_and_cover_every_service() {
        let rules = parse(DEFAULT_RULES).unwrap();
        let names: Vec<&str> = rules.services.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Google Meet", "Slack ハドル", "Discord の通話", "Teams", "Zoom"]);
        assert!(DEFAULT_RULES.contains(VERSION_MARK));
        assert!(parse("[[service]]\nname = \"x\"\nend_after_seconds = 5\n[[service.signal]]\nname = \"a\"\nstrength = \"strong\"\nkind = \"session\"\napps = []\n")
            .unwrap_err()
            .contains("flow"));
        let error = parse("[[service]]\nend_after_seconds = 5\nsignal = []\n").unwrap_err();
        assert!(error.contains("line 1") && error.contains("name"), "位置と理由: {error}");
    }

    #[test]
    fn a_strong_signal_starts_and_keeps_a_meeting_until_it_is_gone_for_the_grace_period() {
        let rules = parse(DEFAULT_RULES).unwrap();
        let mut judge = Judge::default();
        let windows = [window(7, "chrome.exe", "Meet - abc-defg-hij - Google Chrome")];
        let scan = leave_button(7, "chrome.exe");
        let joined = Facts { windows: &windows, uia: Some(&scan), capture: None, render: None };
        let first = judge.apply(&rules, &joined, 0);
        let meet_line = meet(&first);
        assert_eq!(meet_line.event.as_deref(), Some("開始（強い印）"));
        assert!(meet_line.text.contains("Google Meet: 参加中 立った: 退出ボタン（強）・Meet の窓の名前（中）"), "{}", meet_line.text);
        assert!(meet_line.text.contains("立たない: 待機画面の参加ボタン（打ち消し）・ブラウザのマイク（弱）・ブラウザの再生の音（弱）"), "{}", meet_line.text);
        let slack = first.iter().find(|o| o.service == "Slack ハドル").unwrap();
        assert!(slack.text.contains("Slack ハドル: なし"), "{}", slack.text);

        // 退出ボタンが消えても、20 秒までは参加中
        let left_page = Facts { windows: &windows, uia: None, capture: None, render: None };
        let grace = meet(&judge.apply(&rules, &left_page, 2_000)).clone();
        assert!(grace.text.contains("参加中（強い印なし、あと 20 秒で終了）"), "{}", grace.text);
        assert!(grace.event.is_none());
        let later = meet(&judge.apply(&rules, &left_page, 12_000)).clone();
        assert_eq!(later.compare, grace.compare, "残りの秒数だけの変化では出し直さない");
        // 強い印が戻れば、猶予は取り消し
        assert!(meet(&judge.apply(&rules, &joined, 14_000)).text.contains("Google Meet: 参加中 立った"));
        judge.apply(&rules, &left_page, 16_000);
        let ended = meet(&judge.apply(&rules, &left_page, 36_000)).clone();
        assert_eq!(ended.event.as_deref(), Some("終了（強い印が 20 秒戻らなかった）"));
        assert!(ended.text.contains("Google Meet: 待機"), "窓の名前だけが残っている: {}", ended.text);
    }

    #[test]
    fn two_weaker_signals_start_once_and_do_not_restart_until_the_condition_breaks() {
        let rules = parse(DEFAULT_RULES).unwrap();
        let mut judge = Judge::default();
        let capture = audio(Flow::Capture, vec![("slack.exe", SessionState::Active, 0.0)]);
        let render = audio(Flow::Render, vec![("slack.exe", SessionState::Active, 0.05)]);
        let both = Facts { windows: &[], uia: None, capture: Some(&capture), render: Some(&render) };
        let slack = |outcomes: Vec<Outcome>| outcomes.into_iter().find(|o| o.service == "Slack ハドル").unwrap();
        let started = slack(judge.apply(&rules, &both, 0));
        assert_eq!(started.event.as_deref(), Some("開始（中・弱い印が 2 つ）"));
        assert!(started.text.contains("参加中（強い印なし、あと 20 秒で終了）"), "{}", started.text);
        let ended = slack(judge.apply(&rules, &both, 20_000));
        assert!(ended.event.unwrap().starts_with("終了"));
        let blocked = slack(judge.apply(&rules, &both, 22_000));
        assert!(blocked.text.contains("待機（終了のあと、開始の条件がいったん崩れるまで始めない）"), "{}", blocked.text);
        assert!(blocked.event.is_none());
        // 再生の音が止まって条件が崩れたあと、また 2 つそろえば始める
        let silent = audio(Flow::Render, vec![("slack.exe", SessionState::Active, 0.0)]);
        let one = Facts { windows: &[], uia: None, capture: Some(&capture), render: Some(&silent) };
        assert!(slack(judge.apply(&rules, &one, 24_000)).text.contains("待機 立った: Slack のマイク（弱）"));
        assert_eq!(slack(judge.apply(&rules, &both, 26_000)).event.as_deref(), Some("開始（中・弱い印が 2 つ）"));
    }

    #[test]
    fn the_waiting_room_is_not_a_meeting_even_with_the_title_and_the_microphone() {
        let rules = parse(DEFAULT_RULES).unwrap();
        let mut judge = Judge::default();
        // Zen の題は区切りが全角ダッシュ。待機画面では、マイクのプレビューで録音のセッションも Active
        let windows = [window(5, "zen.exe", "Meet – abc-defg-hij — Zen Browser")];
        let capture = audio(Flow::Capture, vec![("zen.exe", SessionState::Active, 0.0)]);
        let mut scan = leave_button(5, "zen.exe");
        scan.windows[0].matched = vec![Control { name: "今すぐ参加".into(), kind: "Button" }];
        let waiting = Facts { windows: &windows, uia: Some(&scan), capture: Some(&capture), render: None };
        let first = meet(&judge.apply(&rules, &waiting, 0)).clone();
        assert!(first.event.is_none(), "待機画面では始めない");
        assert!(first.text.contains("Google Meet: 待機（打ち消しの印がある） 立った: 待機画面の参加ボタン（打ち消し）・Meet の窓の名前（中）・ブラウザのマイク（弱）"), "{}", first.text);
        // 参加すると、参加ボタンが消えて退出ボタンが出る
        let joined_scan = leave_button(5, "zen.exe");
        let joined = Facts { windows: &windows, uia: Some(&joined_scan), capture: Some(&capture), render: None };
        assert_eq!(meet(&judge.apply(&rules, &joined, 14_000)).event.as_deref(), Some("開始（強い印）"));
        // 退出後も題とマイクが残っても、強い印が戻らなければ終わる
        let left = Facts { windows: &windows, uia: None, capture: Some(&capture), render: None };
        judge.apply(&rules, &left, 16_000);
        assert!(meet(&judge.apply(&rules, &left, 36_000)).event.as_deref().unwrap().starts_with("終了"));
        assert!(meet(&judge.apply(&rules, &left, 38_000)).event.is_none(), "題とマイクだけで始め直さない");
    }

    #[test]
    fn browser_signals_include_guessed_browsers_and_respect_the_window_title() {
        let rules = parse(DEFAULT_RULES).unwrap();
        let mut zen = window(9, "zen.exe", "Meet - abc-defg-hij — Zen Browser");
        let scan = leave_button(9, "zen.exe");
        let facts = Facts { windows: std::slice::from_ref(&zen), uia: Some(&scan), capture: None, render: None };
        assert!(meet(&Judge::default().apply(&rules, &facts, 0)).text.contains("参加中"));
        zen.exe = "newbrowser.exe".into();
        zen.guessed_browser = Some("版情報".into());
        let scan = leave_button(9, "newbrowser.exe");
        let facts = Facts { windows: std::slice::from_ref(&zen), uia: Some(&scan), capture: None, render: None };
        assert!(meet(&Judge::default().apply(&rules, &facts, 0)).text.contains("参加中"), "推定したブラウザも当たる");
        // 会議でないページの退出ボタンは、Meet の印にしない
        let other = [window(3, "chrome.exe", "Settings - Google Chrome")];
        let scan = leave_button(3, "chrome.exe");
        let facts = Facts { windows: &other, uia: Some(&scan), capture: None, render: None };
        assert!(meet(&Judge::default().apply(&rules, &facts, 0)).text.contains("Google Meet: なし"));
    }
}
