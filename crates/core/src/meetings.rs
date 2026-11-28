//! 会議の自動検出。
//!
//! サービス（Google Meet・Slack ハドル・Discord・Teams・Zoom）ごとの判定表（[`crate::meeting_rules`]）で、
//! 会議に参加中かを決める。2 秒ごとに次を行う（[`MeetingDetector::step`]）。
//! 1. PC の様子を読む（[`crate::meeting_facts`]）: 対象のアプリの窓、窓の中の部品、録音・再生のオーディオのセッション
//! 2. 判定表を当てはめる（[`crate::meeting_rules::evaluate`]）: サービスごとの文脈と、立っている印
//! 3. 開始・継続・終了を決める（[`crate::meeting_judge::Judge`]）
//!
//! 当てはめと判定は OS に依存しないのでテストできる。PC の様子を読むのは Windows だけ。

use serde::{Deserialize, Serialize};

pub use crate::meeting_judge::{Decision, Judge, JudgeConfig, Phase, Reason, ServiceState, ServiceStatus};
use crate::meeting_facts::{Facts, Observer};
use crate::meeting_rules::{evaluate, Context, Evaluation, Held, Kind, Rules, ServiceEvaluation, Strength};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MeetingApp {
    GoogleMeet,
    SlackHuddle,
    Zoom,
    Teams,
    Discord,
}

impl MeetingApp {
    pub fn label(self) -> &'static str {
        match self {
            MeetingApp::GoogleMeet => "Google Meet",
            MeetingApp::SlackHuddle => "Slack ハドル",
            MeetingApp::Zoom => "Zoom",
            MeetingApp::Teams => "Teams",
            MeetingApp::Discord => "Discord",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeetingEvent {
    Started(MeetingApp),
    Ended(MeetingApp),
}

/// パスの最後の要素（実行ファイル名）を小文字で返す。
pub fn exe_name(id: &str) -> String {
    id.rsplit(['\\', '/']).next().unwrap_or(id).to_lowercase()
}

/// 記録やログに残すアプリの名前: パスなら実行ファイルの名前だけ（利用者の名前を含むフルパスは残さない）、
/// パッケージ名ならそのまま。
pub fn app_name(id: &str) -> String {
    if id.contains(['\\', '/']) {
        exe_name(id)
    } else {
        id.to_string()
    }
}

/// 対象のブラウザ（実行ファイルの名前、小文字。MTG-002）。同じ実行ファイルの名前を使う版（Chrome の Beta・Dev・Canary、
/// Edge の Beta・Dev・Canary、Firefox の Developer Edition・Nightly、Opera GX の opera.exe など）も含む。
/// 一覧にないアプリは、ブラウザと推定しない。
pub const BROWSERS: &[&str] = &[
    // Chromium 系
    "chrome.exe",
    "msedge.exe",
    "brave.exe",
    "vivaldi.exe",
    "opera.exe",
    "opera_gx.exe",
    "arc.exe",
    "thorium.exe",
    "chromium.exe",
    "supermium.exe",
    // Yandex Browser の実行ファイルは browser.exe
    "browser.exe",
    "whale.exe",
    "comet.exe",
    "dia.exe",
    "atlas.exe",
    "sidekick.exe",
    "island.exe",
    "duckduckgo.exe",
    // Firefox 系
    "firefox.exe",
    "zen.exe",
    "floorp.exe",
    "librewolf.exe",
    "waterfox.exe",
    "mullvadbrowser.exe",
];

/// ブラウザとして扱わないプロセス（実行ファイルの名前、小文字）。設定で足しても対象にしない（SET-021）。
/// Kiroscribe 自身と、MTG-003 で対象にしない、会議アプリと一緒に入る更新・起動のためのプロセスと、ほかのアプリも使う部品。
pub const NOT_BROWSERS: &[&str] = &[
    "kiroscribe.exe",
    "update.exe",
    "ms-teamsupdate.exe",
    "msteams_autostarter.exe",
    "msedgewebview2.exe",
    "zcscpthost.exe",
];

/// 判定の対象のブラウザ: 組み込みの一覧（[`BROWSERS`]）と、設定で足したもの（SET-021）。[`NOT_BROWSERS`] と、
/// 判定表の `apps` に実行ファイルの名前を書いたデスクトップの会議アプリ（slack.exe・zoom.exe など）は含めない。
/// 会議アプリは設定で足しても、その実行ファイルの名前の印で判定する（MTG-002）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Browsers {
    extra: Vec<String>,
}

impl Browsers {
    /// 組み込みの一覧だけ。
    pub fn builtin() -> Self {
        Browsers { extra: Vec::new() }
    }

    /// 組み込みの一覧に、設定で足したブラウザ（実行ファイルの名前）を加える。名前は設定と同じ形に整え、
    /// デスクトップの会議アプリは除く。
    pub fn with_extra(names: &[String]) -> Self {
        let mut extra = crate::settings::normalize_browsers(names);
        extra.retain(|exe| !Rules::builtin().is_desktop_app(exe));
        Browsers { extra }
    }

    /// 設定で足したブラウザ（整えた実行ファイルの名前）。
    pub fn extra(&self) -> &[String] {
        &self.extra
    }

    /// 対象のブラウザか（実行ファイルの名前。大文字小文字は区別しない）。
    pub fn contains(&self, exe: &str) -> bool {
        let exe = exe.to_lowercase();
        !NOT_BROWSERS.contains(&exe.as_str())
            && (BROWSERS.contains(&exe.as_str()) || (self.extra.contains(&exe) && !Rules::builtin().is_desktop_app(&exe)))
    }
}

/// 製品が検出を調べる間隔（ミリ秒）。記録の再生もこの間隔で行う。
pub const POLL_MS: i64 = 2_000;
/// 印がどれも立っていないときに、窓の中の部品を調べる間隔（調べる回の何回に 1 回か。2 秒 × 3 = 6 秒）。
pub const IDLE_SCAN_EVERY: u64 = 3;

/// 1 回分の検出の結果。
#[derive(Debug, Clone)]
pub struct Step {
    pub facts: Facts,
    pub evaluation: Evaluation,
    pub decisions: Vec<Decision>,
}

/// 会議の検出（PC の様子を読み、判定表を当てはめ、開始・終了を決める）。作ったスレッドで使うこと。
pub struct MeetingDetector {
    rules: &'static Rules,
    browsers: Browsers,
    pub judge: Judge,
    observer: Observer,
    ticks: u64,
}

impl MeetingDetector {
    pub fn new(browsers: Browsers) -> Self {
        let rules = Rules::builtin();
        MeetingDetector { rules, browsers, judge: Judge::new(rules), observer: Observer::new(), ticks: 0 }
    }

    pub fn rules(&self) -> &'static Rules {
        self.rules
    }

    pub fn browsers(&self) -> &Browsers {
        &self.browsers
    }

    /// 判定の対象のブラウザを替える（設定を保存したとき。次の回から使う）。
    pub fn set_browsers(&mut self, browsers: Browsers) {
        self.browsers = browsers;
    }

    /// 1 回分を進める。窓の中の部品は、印が立っているときと会議中は毎回、それ以外は [`IDLE_SCAN_EVERY`] 回に 1 回調べる。
    pub fn step(&mut self, now: i64) -> Step {
        let scan = self.judge.busy() || self.ticks.is_multiple_of(IDLE_SCAN_EVERY);
        self.ticks += 1;
        let facts = self.observer.observe(self.rules, &self.browsers, &self.judge.meeting_windows(), scan);
        let evaluation = evaluate(self.rules, &facts, &self.browsers);
        let decisions = self.judge.tick(&evaluation, now);
        Step { facts, evaluation, decisions }
    }
}

/// E2E テストの差し替え（`meeting.txt`）で使う、その会議アプリの強い印だけが立っている様子。
pub fn simulated_evaluation(app: Option<MeetingApp>) -> Evaluation {
    let services = app
        .map(|app| ServiceEvaluation {
            app,
            contexts: vec![Context {
                app: "e2e".into(),
                window: None,
                scanned: true,
                signals: vec![Held {
                    name: "E2E テストの会議".into(),
                    strength: Strength::Strong,
                    kind: Kind::Window,
                    after_start: false,
                }],
            }],
        })
        .into_iter()
        .collect();
    Evaluation { services }
}

#[cfg(test)]
mod tests {
    use super::*;

    // [MTG-001]
    #[test]
    fn exe_name_handles_both_separators() {
        assert_eq!(exe_name(r"C:\A\Slack.EXE"), "slack.exe");
        assert_eq!(exe_name("/usr/bin/zoom"), "zoom");
        assert_eq!(exe_name("MSTeams_8wekyb3d8bbwe"), "msteams_8wekyb3d8bbwe");
        assert_eq!(app_name(r"C:\Users\山田\AppData\Local\slack\slack.exe"), "slack.exe");
        assert_eq!(app_name("MSTeams_8wekyb3d8bbwe"), "MSTeams_8wekyb3d8bbwe");
    }

    /// 対象のブラウザは、Chromium 系と Firefox 系の一覧（Zen を含む）。一覧にないアプリはブラウザとしない。
    // [MTG-002][MTG-003]
    #[test]
    fn browsers_are_the_listed_chromium_and_firefox_family() {
        let browsers = Browsers::builtin();
        for exe in ["chrome.exe", "MSEdge.exe", "zen.exe", "firefox.exe", "brave.exe", "vivaldi.exe", "opera.exe", "arc.exe", "floorp.exe"] {
            assert!(browsers.contains(exe), "{exe}");
        }
        assert_eq!(BROWSERS.len(), 24);
        for exe in ["thunderbird.exe", "aquawindowsbridge.exe", "msedgewebview2.exe", "slack.exe", "kiroscribe.exe"] {
            assert!(!browsers.contains(exe), "{exe}");
        }
    }

    /// 設定で足したブラウザも、組み込みの一覧と同じく対象にする。名前は実行ファイルの名前にそろえる。
    // [MTG-002][SET-021]
    #[test]
    fn browsers_added_in_settings_are_also_targets() {
        let browsers = Browsers::with_extra(&[" MyBrowser ".into(), r"C:\Tools\Other.EXE".into(), "kiroscribe.exe".into()]);
        assert_eq!(browsers.extra(), ["mybrowser.exe", "other.exe"]);
        for exe in ["mybrowser.exe", "MyBrowser.exe", "other.exe", "chrome.exe"] {
            assert!(browsers.contains(exe), "{exe}");
        }
        assert!(!browsers.contains("kiroscribe.exe") && !browsers.contains("thunderbird.exe"));
        assert!(Browsers::with_extra(&[]).extra().is_empty());
    }

    /// MTG-003 で対象にしないプロセスは、設定で足してもブラウザとして扱わない。
    // [MTG-003][SET-021]
    #[test]
    fn processes_excluded_by_mtg_003_cannot_be_added_as_browsers() {
        let names: Vec<String> = NOT_BROWSERS.iter().map(|exe| exe.to_uppercase()).chain(["MSEdgeWebView2".into(), "update".into()]).collect();
        let browsers = Browsers::with_extra(&names);
        assert!(browsers.extra().is_empty(), "{:?}", browsers.extra());
        // 整える前の名前が混ざっても（保存の経路を通らない場合）、判定では対象にしない
        let browsers = Browsers { extra: NOT_BROWSERS.iter().map(|exe| exe.to_string()).collect() };
        for exe in NOT_BROWSERS {
            assert!(!browsers.contains(exe), "{exe}");
            assert!(!browsers.contains(&exe.to_uppercase()), "{exe}");
        }
    }

    /// 判定表のデスクトップの会議アプリは、設定で足してもブラウザとして扱わない。
    // [MTG-002][SET-021]
    #[test]
    fn desktop_meeting_apps_added_in_settings_are_not_browsers() {
        let browsers = Browsers::with_extra(&["Slack.exe".into(), "zoom".into(), "cpthost.exe".into(), "mybrowser".into()]);
        assert_eq!(browsers.extra(), ["mybrowser.exe"]);
        // 整える前の名前が混ざっても（保存の経路を通らない場合）、判定では対象にしない
        let browsers = Browsers { extra: vec!["slack.exe".into(), "zoom.exe".into()] };
        for exe in ["slack.exe", "SLACK.EXE", "zoom.exe"] {
            assert!(!browsers.contains(exe), "{exe}");
        }
    }

    /// 検出が調べるのは PC の中だけ: 通信・録音・文字起こし・画面の取り込みの部品を、検出の部品は使わない。
    // [SEC-013]
    #[test]
    fn detection_uses_no_network_audio_or_screen_capture() {
        for (name, source) in [
            ("meetings.rs", include_str!("meetings.rs")),
            ("meeting_facts.rs", include_str!("meeting_facts.rs")),
            ("meeting_rules.rs", include_str!("meeting_rules.rs")),
            ("meeting_judge.rs", include_str!("meeting_judge.rs")),
        ] {
            let source = source.replace("\r\n", "\n");
            let code = &source[..source.find("\n#[cfg(test)]\nmod tests").expect("テストの手前まで")];
            for forbidden in ["reqwest", "crate::http", "crate::audio::", "crate::transcribe", "crate::claude", "BitBlt", "PrintWindow", "IAudioCaptureClient"] {
                assert!(!code.contains(forbidden), "{name} が {forbidden} を使っている");
            }
        }
    }

    // [MTG-021]
    #[cfg(not(windows))]
    #[test]
    fn nothing_is_detected_outside_windows() {
        let mut detector = MeetingDetector::new(Browsers::builtin());
        for at in 0..5 {
            let step = detector.step(at * POLL_MS);
            assert_eq!(step.facts, Facts::default());
            assert!(step.evaluation.services.is_empty() && step.decisions.is_empty());
        }
    }
}
