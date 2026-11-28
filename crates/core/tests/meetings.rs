//! 会議の自動検出の仕様のテスト: PC の様子（窓・部品・オーディオのセッション）に組み込みの判定表を当てはめ、
//! 開始・継続・終了が仕様どおりになることを確かめる。実機で判定が外れた場面は、ここで回帰のテストにする。

use kiroscribe_core::audio_sessions::{AudioSession, AudioSessions, SessionState};
use kiroscribe_core::meeting_facts::{ControlFact, ControlScan, ControlType, Facts, ScannedWindow, WindowFact};
use kiroscribe_core::meeting_rules::{evaluate, Evaluation, Rules};
use kiroscribe_core::meetings::{Browsers, Judge, MeetingApp, MeetingEvent, Phase, Reason, ServiceState, POLL_MS};

const CHROME: &str = "Chrome_WidgetWin_1";
const FIREFOX: &str = "MozillaWindowClass";

fn window(hwnd: u64, exe: &str, class: &str, title: &str) -> WindowFact {
    WindowFact { hwnd, exe: exe.into(), class: class.into(), title: title.into(), visible: true, site: None }
}

const MEET_SITE: &str = "meet.google.com";

fn hidden(hwnd: u64, exe: &str, class: &str, title: &str) -> WindowFact {
    WindowFact { visible: false, ..window(hwnd, exe, class, title) }
}

fn button(name: &str) -> ControlFact {
    ControlFact { kind: ControlType::Button, name: name.into() }
}

fn control(kind: ControlType, name: &str) -> ControlFact {
    ControlFact { kind, name: name.into() }
}

fn session(exe: &str, state: SessionState, peak: Option<f32>) -> AudioSession {
    AudioSession { pid: 1, exe_path: format!(r"C:\Program Files\App\{exe}"), package: None, state, peak }
}

fn sessions(list: Vec<AudioSession>) -> AudioSessions {
    AudioSessions { available: true, endpoints: 1, sessions: list, ..Default::default() }
}

/// PC の様子を組み立てる。
#[derive(Default, Clone)]
struct Pc {
    windows: Vec<WindowFact>,
    mics: Vec<AudioSession>,
    sounds: Vec<AudioSession>,
    /// 窓の中を調べた結果（`None` ならこの回は調べていない）
    scan: Option<Vec<ScannedWindow>>,
}

impl Pc {
    fn new() -> Self {
        Pc { scan: Some(Vec::new()), ..Default::default() }
    }

    fn window(mut self, window: WindowFact, controls: Vec<ControlFact>) -> Self {
        if let Some(scan) = &mut self.scan {
            if window.visible {
                scan.push(ScannedWindow { hwnd: window.hwnd, controls, seen: 10, error: None, incomplete: None });
            }
        }
        self.windows.push(window);
        self
    }

    fn mic(mut self, exe: &str) -> Self {
        self.mics.push(session(exe, SessionState::Active, None));
        self
    }

    fn sound(mut self, exe: &str, peak: f32) -> Self {
        self.sounds.push(session(exe, SessionState::Active, Some(peak)));
        self
    }

    fn unscanned(mut self) -> Self {
        self.scan = None;
        self
    }

    fn facts(&self) -> Facts {
        Facts {
            windows: self.windows.clone(),
            capture: sessions(self.mics.clone()),
            render: sessions(self.sounds.clone()),
            scan: self.scan.clone().map(|windows| ControlScan { windows, elapsed_ms: 50, error: None }),
        }
    }

    fn evaluation(&self) -> Evaluation {
        evaluate(Rules::builtin(), &self.facts(), &Browsers::builtin())
    }
}

/// 判定を、製品と同じ 2 秒ごとに進める。
struct Run {
    judge: Judge,
    now: i64,
}

impl Run {
    fn new() -> Self {
        Run { judge: Judge::new(Rules::builtin()), now: 0 }
    }

    /// 1 回進め、知らせる出来事を返す。
    fn tick(&mut self, pc: &Pc) -> Vec<MeetingEvent> {
        let events = self.judge.tick(&pc.evaluation(), self.now).into_iter().map(|d| d.event).collect();
        self.now += POLL_MS;
        events
    }

    /// `seconds` 秒のあいだ同じ様子で進め、起きた出来事をまとめて返す。
    fn hold(&mut self, pc: &Pc, seconds: i64) -> Vec<MeetingEvent> {
        (0..seconds * 1000 / POLL_MS).flat_map(|_| self.tick(pc)).collect()
    }

    fn state(&self, app: MeetingApp) -> ServiceState {
        self.judge.statuses().into_iter().find(|status| status.app == app).map(|status| status.state).unwrap()
    }
}

fn started(app: MeetingApp) -> Vec<MeetingEvent> {
    vec![MeetingEvent::Started(app)]
}

fn ended(app: MeetingApp) -> Vec<MeetingEvent> {
    vec![MeetingEvent::Ended(app)]
}

const MEET: MeetingApp = MeetingApp::GoogleMeet;
const SLACK: MeetingApp = MeetingApp::SlackHuddle;

fn meet_in_chrome(controls: Vec<ControlFact>) -> Pc {
    Pc::new().window(window(10, "chrome.exe", CHROME, "Meet - abc-defg-hij - Google Chrome").at_site(MEET_SITE), controls)
}

/// デスクトップの会議アプリは、実行ファイルの名前（大文字小文字は区別しない）の窓・部品・セッションで判定する。
// [MTG-001]
#[test]
fn desktop_meeting_apps_are_judged_by_their_exe_names() {
    let mut run = Run::new();
    let slack = Pc::new().window(window(1, "slack.exe", CHROME, "Huddle: 山田 - Acme - Slack"), vec![button("退出")]);
    assert_eq!(run.tick(&slack), started(SLACK));

    let mut run = Run::new();
    let discord = Pc::new().window(window(2, "discordptb.exe", CHROME, "#一般 | 開発 - Discord"), vec![button("Disconnect")]);
    assert_eq!(run.tick(&discord), started(MeetingApp::Discord));

    let mut run = Run::new();
    let teams = Pc::new().window(window(3, "ms-teams.exe", CHROME, "会議 | Microsoft Teams"), vec![button("退出")]);
    assert_eq!(run.tick(&teams), started(MeetingApp::Teams));

    // パッケージのアプリで実行ファイルのパスを読めないときは、パッケージファミリー名から Slack とする
    let store_slack = AudioSession { package: Some("91750D7E.Slack_8she8kybcnzg4".into()), exe_path: String::new(), ..session("x", SessionState::Active, None) };
    let mut pc = Pc::new().window(window(4, "slack.exe", CHROME, "Huddle: 山田 - Acme - Slack"), vec![]);
    pc.mics.push(store_slack);
    let mut run = Run::new();
    assert_eq!(run.hold(&pc, 6), started(SLACK), "ハドルの窓とマイクの組み合わせ");
}

/// 対象のブラウザ（Zen を含む）で、Meet のタブの退出ボタンが見えたら参加中。区切りが「 — 」の題でもよい。
// [MTG-002][MTG-028]
#[test]
fn meet_is_detected_in_every_listed_browser() {
    for (exe, class, title) in [
        ("chrome.exe", CHROME, "Meet - abc-defg-hij - Google Chrome"),
        ("msedge.exe", CHROME, "Meet - abc-defg-hij - 個人 - Microsoft\u{200b} Edge"),
        ("zen.exe", FIREFOX, "Meet – abc-defg-hij — Zen Browser"),
        ("firefox.exe", FIREFOX, "Meet - abc-defg-hij — Mozilla Firefox"),
        ("brave.exe", CHROME, "Google Meet - Brave"),
    ] {
        let mut run = Run::new();
        let pc = Pc::new().window(window(5, exe, class, title).at_site(MEET_SITE), vec![button("通話から退出")]);
        assert_eq!(run.tick(&pc), started(MEET), "{exe}");
    }
    // 一覧にないアプリは、ブラウザとして扱わない
    let mut run = Run::new();
    let unknown = Pc::new().window(window(5, "newbrowser.exe", CHROME, "Meet - abc-defg-hij - New Browser"), vec![button("通話から退出")]).mic("newbrowser.exe");
    assert!(run.hold(&unknown, 10).is_empty());
}

/// 一覧にないアプリ（Aqua Voice・Thunderbird）のマイクと再生の音は数えない。退出したあとの Meet のタブの題が残っていても、
/// それと組み合わせて始めない。ブラウザのマイクだけ、
/// 補助のプロセスのマイクも会議にしない。
// [MTG-003]
#[test]
fn other_apps_microphones_and_sounds_are_not_meeting_signals() {
    let stale_meet = Pc::new()
        .window(window(10, "zen.exe", FIREFOX, "Meet – abc-defg-hij — Zen Browser").at_site(MEET_SITE), vec![])
        .window(window(11, "thunderbird.exe", FIREFOX, "受信トレイ - Mozilla Thunderbird"), vec![])
        .mic("aquawindowsbridge.exe")
        .sound("thunderbird.exe", 0.4)
        .sound("aquavoice.exe", 0.4);
    let mut run = Run::new();
    assert!(run.hold(&stale_meet, 60).is_empty());
    assert_eq!(run.state(MEET), ServiceState::Waiting, "題だけ（中が 1 つ）");

    let only_mic = Pc::new().window(window(12, "chrome.exe", CHROME, "受信トレイ - Gmail - Google Chrome").at_site("mail.google.com"), vec![]).mic("chrome.exe").sound("chrome.exe", 0.3);
    let mut run = Run::new();
    assert!(run.hold(&only_mic, 30).is_empty(), "会議のタブのない窓の、ブラウザのマイクと再生の音");
    assert_eq!(run.state(MEET), ServiceState::None);

    let helpers = Pc::new().mic("msedgewebview2.exe").mic("update.exe").mic("ms-teamsupdate.exe").mic("zcscpthost.exe").sound("msedgewebview2.exe", 0.5);
    assert!(helpers.evaluation().services.is_empty());
}

/// 印は同じ窓の中だけで組み合わせる。Zen の別の窓の「退出」ボタンで、待機画面の Meet を始めない。
/// Edge の待機画面の参加ボタン（打ち消し）で、Zen で続いている Meet を終えない。
// [MTG-004]
#[test]
fn signals_and_vetoes_combine_only_within_one_window() {
    let waiting = Pc::new()
        .window(window(20, "zen.exe", FIREFOX, "Meet – abc-defg-hij — Zen Browser").at_site(MEET_SITE), vec![button("今すぐ参加")])
        .window(window(21, "zen.exe", FIREFOX, "設定 — Zen Browser"), vec![button("退出")])
        .mic("zen.exe");
    let mut run = Run::new();
    assert!(run.hold(&waiting, 20).is_empty(), "別の窓の退出ボタン");

    let joined_in_zen = Pc::new().window(window(20, "zen.exe", FIREFOX, "Meet – abc-defg-hij — Zen Browser").at_site(MEET_SITE), vec![button("通話から退出")]).mic("zen.exe");
    let mut run = Run::new();
    assert_eq!(run.tick(&joined_in_zen), started(MEET));
    let edge_waiting = joined_in_zen
        .clone()
        .window(window(30, "msedge.exe", CHROME, "Meet - xyz-abcd-efg - Microsoft\u{200b} Edge").at_site(MEET_SITE), vec![button("参加をリクエスト")]);
    assert!(run.hold(&edge_waiting, 60).is_empty(), "Edge の待機画面で、Zen の会議を終えない");
    assert_eq!(run.judge.phase(), Phase::Active(MEET));
}

/// Kiroscribe 自身（録音中）のマイクと窓は、判定に入れない。
// [MTG-005]
#[test]
fn kiroscribe_itself_is_not_a_meeting() {
    let own = Pc::new().window(window(40, "kiroscribe.exe", CHROME, "Kiroscribe - 会議の録音"), vec![button("録音を停止")]).mic("kiroscribe.exe").sound("kiroscribe.exe", 0.5);
    assert!(own.evaluation().services.is_empty());
}

/// 部品の印は Button の部品だけ。Slack の環境設定のチェックボックス「ハドルミーティングから自動的に退出する」や、
/// 名前に「自動的に」を含むボタンでは、参加中にしない。
// [MTG-006]
#[test]
fn only_buttons_count_and_excluded_names_do_not() {
    let preferences = |controls| Pc::new().window(window(50, "slack.exe", CHROME, "環境設定 - Acme - Slack"), controls).mic("slack.exe");
    let mut run = Run::new();
    assert!(run.hold(&preferences(vec![control(ControlType::CheckBox, "ハドルミーティングから自動的に退出する")]), 20).is_empty());
    let mut run = Run::new();
    assert!(run.hold(&preferences(vec![button("ハドルミーティングから自動的に退出する")]), 20).is_empty());
    let mut run = Run::new();
    assert!(run.hold(&preferences(vec![control(ControlType::MenuItem, "チャンネルから退出する")]), 20).is_empty());
    // Discord のサーバーからの脱退は、通話の切断ではない
    let mut run = Run::new();
    let server = Pc::new().window(window(51, "discord.exe", CHROME, "Discord"), vec![button("Leave Server")]);
    assert!(run.hold(&server, 20).is_empty());
    let mut run = Run::new();
    assert_eq!(run.tick(&preferences(vec![button("ハドルから退出する")])), started(SLACK));
}

/// 強い印は 1 つで、その回に開始。組み合わせ（中・弱 2 つ）は、4 秒続き、その間に部品を 2 回以上調べてから開始。
/// 待機画面を開いた直後（部品をまだ読めない約 2 秒）や、タブを閉じる途中に、組み合わせだけで始めない。
// [MTG-007]
#[test]
fn a_combination_waits_for_two_scans_and_a_strong_signal_starts_at_once() {
    // 待機画面を開いた直後: 1 回目の走査では部品が読めず、2 回目で参加ボタン（打ち消し）が見える
    let opening = meet_in_chrome(vec![]).mic("chrome.exe");
    let waiting = meet_in_chrome(vec![button("今すぐ参加")]).mic("chrome.exe");
    let mut run = Run::new();
    assert!(run.tick(&opening).is_empty());
    assert_eq!(run.state(MEET), ServiceState::Confirming);
    assert!(run.hold(&waiting, 30).is_empty(), "待機画面");
    assert_eq!(run.state(MEET), ServiceState::Waiting);
    // 参加すると、退出ボタン（強）で、その回に始まる
    assert_eq!(run.tick(&meet_in_chrome(vec![button("通話から退出")]).mic("chrome.exe")), started(MEET));

    // 部品を調べない回は数えない。調べた回が 2 回になり、4 秒たってから始める
    let mut run = Run::new();
    let unscanned = opening.clone().unscanned();
    assert!(run.tick(&opening).is_empty());
    assert!(run.tick(&unscanned).is_empty());
    assert!(run.tick(&unscanned).is_empty(), "4 秒たったが、調べたのは 1 回");
    assert_eq!(run.tick(&opening), started(MEET));

    // タブを閉じる途中（題とマイクが残り、部品が読めない）は、閉じ終わると数え直す
    let mut run = Run::new();
    assert!(run.tick(&opening).is_empty());
    assert!(run.tick(&Pc::new()).is_empty());
    assert!(run.tick(&opening).is_empty());
    assert!(run.tick(&opening).is_empty());
    assert_eq!(run.tick(&opening), started(MEET), "途切れてから 4 秒");

    // マイクだけ（弱い印 1 つ）では始めない
    let mut run = Run::new();
    assert!(run.hold(&Pc::new().mic("slack.exe"), 60).is_empty());
}

/// 窓の中を調べられなかった回は、組み合わせの開始に要る調べた回に数えない。UI Automation を使えないとき、
/// 対象の窓をどれも調べられなかったとき、ほかのサービスの窓だけを調べられたときは、組み合わせで始めない。
// [MTG-007]
#[test]
fn a_combination_does_not_start_when_the_windows_cannot_be_read() {
    let teams = || Pc::new().window(window(90, "ms-teams.exe", CHROME, "会議 | Microsoft Teams"), vec![]).mic("ms-teams.exe");
    let failed = |pc: Pc, uia: Option<&str>, window_error: bool| {
        let mut facts = pc.facts();
        let scan = facts.scan.as_mut().unwrap();
        scan.error = uia.map(String::from);
        if uia.is_some() {
            scan.windows.clear();
        }
        for scanned in scan.windows.iter_mut().filter(|scanned| scanned.hwnd == 90 && window_error) {
            scanned.error = Some("0x80004005".into());
        }
        facts
    };
    let cases = [
        ("UI Automation を使えない", failed(teams(), Some("0x80040154"), false)),
        ("対象の窓をどれも調べられない", failed(teams(), None, true)),
        (
            "ほかのサービスの窓だけを調べられた",
            failed(teams().window(window(60, "slack.exe", CHROME, "#dev - Acme - Slack"), vec![]), None, true),
        ),
        ("Meet のタブを調べられない", {
            let mut facts = meet_in_chrome(vec![]).mic("chrome.exe").facts();
            facts.scan.as_mut().unwrap().error = Some("0x80040154".into());
            facts.scan.as_mut().unwrap().windows.clear();
            facts
        }),
    ];
    for (label, facts) in cases {
        let evaluation = evaluate(Rules::builtin(), &facts, &Browsers::builtin());
        let mut judge = Judge::new(Rules::builtin());
        let events: Vec<MeetingEvent> = (0..15).flat_map(|i| judge.tick(&evaluation, i * POLL_MS)).map(|d| d.event).collect();
        assert!(events.is_empty(), "{label}: {events:?}");
    }
    // 窓を調べられれば、組み合わせで始める
    let mut run = Run::new();
    assert_eq!(run.hold(&teams(), 6), started(MeetingApp::Teams));
}

/// 強い印も組み合わせもない状態が 20 秒続いたら終了。戻れば続き。Meet の退出後の画面のボタン（打ち消し）が出たら、すぐ終了。
/// 退出したあともタブの題が残るだけでは、続いているとみなさない。
// [MTG-008]
#[test]
fn a_meeting_ends_after_the_grace_period_or_at_once_on_the_left_screen() {
    let joined = meet_in_chrome(vec![button("通話から退出")]).mic("chrome.exe");
    let mut run = Run::new();
    assert_eq!(run.tick(&joined), started(MEET));
    // 退出したが、題が残っていて、部品は読めない
    let title_only = meet_in_chrome(vec![]);
    assert!(run.hold(&title_only, 18).is_empty());
    assert!(run.tick(&joined).is_empty(), "20 秒以内に戻れば続き");
    assert!(run.hold(&title_only, 18).is_empty());
    assert_eq!(run.hold(&title_only, 4), ended(MEET));
    assert!(run.hold(&title_only, 30).is_empty(), "題だけで始め直さない");

    // 退出後の画面が出たら、すぐに終わる
    let mut run = Run::new();
    assert_eq!(run.tick(&joined), started(MEET));
    for screen in [
        vec![control(ControlType::Text, "ミーティングから退出しました"), button("再参加"), button("ホーム画面に戻る")],
        vec![control(ControlType::Text, "You left the meeting"), button("Rejoin")],
    ] {
        let left = meet_in_chrome(screen).mic("chrome.exe");
        let decisions = run.judge.tick(&left.evaluation(), run.now);
        assert_eq!(decisions.iter().map(|d| d.event).collect::<Vec<_>>(), ended(MEET));
        assert!(matches!(&decisions[0].reason, Reason::Veto { signal, .. } if signal == "退出後の画面のボタン"), "{:?}", decisions[0].reason);
        run.now += POLL_MS;
        assert!(run.hold(&left, 10).is_empty());
        assert_eq!(run.tick(&joined), started(MEET), "再参加");
    }
}

/// 開始のあとは、背面を含むタブの印も数える。Meet のタブを背面にしても、タブの一覧にあり、マイクを使っている間は続く。
/// 開始の前は、背面のタブの印では始めない。
// [MTG-009]
#[test]
fn a_browser_meeting_continues_in_a_background_tab() {
    let joined = meet_in_chrome(vec![button("通話から退出"), control(ControlType::TabItem, "Meet - abc-defg-hij")]).mic("chrome.exe");
    let docs = |mic: bool| {
        let pc = Pc::new().window(
            window(10, "chrome.exe", CHROME, "企画書 - Google ドキュメント - Google Chrome").at_site("docs.google.com"),
            vec![control(ControlType::TabItem, "企画書 - Google ドキュメント"), control(ControlType::TabItem, "Meet - abc-defg-hij - マイクを使用中")],
        );
        if mic { pc.mic("chrome.exe") } else { pc }
    };
    let mut run = Run::new();
    assert!(run.hold(&docs(true), 30).is_empty(), "開始の前は、背面のタブでは始めない");
    assert_eq!(run.tick(&joined), started(MEET));
    assert!(run.hold(&docs(true), 120).is_empty(), "背面のタブとマイクで続く");
    assert_eq!(run.judge.phase(), Phase::Active(MEET));
    assert_eq!(run.judge.meeting_windows(), vec![10], "会議の窓の中を調べ続ける");
    assert_eq!(run.hold(&docs(false), 22), ended(MEET), "マイクも再生の音もなくなったら、猶予のあとで終わる");
}

/// 2 つの会議が重なったとき: あとの会議の開始は知らせないが、判定は続ける。先の会議が終わると、あとの会議に切り替わり、
/// 段階が先の会議のまま残らない。
// [MTG-010]
#[test]
fn overlapping_meetings_switch_to_the_one_still_going() {
    let slack = |leave: bool| {
        Pc::new().window(window(60, "slack.exe", CHROME, "Huddle: 山田 - Acme - Slack"), if leave { vec![button("退出")] } else { vec![] })
    };
    let with_meet = |pc: Pc| pc.window(window(10, "chrome.exe", CHROME, "Meet - abc-defg-hij - Google Chrome").at_site(MEET_SITE), vec![button("通話から退出")]);
    let mut run = Run::new();
    assert_eq!(run.tick(&slack(true)), started(SLACK));
    assert!(run.hold(&with_meet(slack(true)), 10).is_empty(), "あとの会議の開始は知らせない");
    assert_eq!(run.judge.phase(), Phase::Active(SLACK));
    assert_eq!(run.judge.others(), vec![MEET]);
    // Slack のハドルを抜けた（ハドルの窓だけが少し残る）
    let left_slack = with_meet(Pc::new().window(window(60, "slack.exe", CHROME, "Huddle: 山田 - Acme - Slack"), vec![]));
    assert_eq!(run.tick(&left_slack), vec![MeetingEvent::Ended(SLACK), MeetingEvent::Started(MEET)]);
    assert_eq!(run.judge.phase(), Phase::Active(MEET));
    assert!(run.judge.others().is_empty());
    assert!(run.hold(&left_slack, 30).is_empty());
    assert_eq!(run.hold(&Pc::new(), 22), ended(MEET));

    // 同じサービスの印がまた立てば、前の会議の続き
    let mut run = Run::new();
    assert_eq!(run.tick(&slack(true)), started(SLACK));
    assert!(run.hold(&Pc::new(), 10).is_empty());
    assert!(run.hold(&slack(true), 10).is_empty());
}

/// 再生の音は、音の大きさが 0.01 以上のときだけ印にする（ピーク 0.001 の音では立たない）。
/// オーディオのセッションの一覧を読めなくても、ほかの印で判定を続ける。
// [MTG-020]
#[test]
fn sound_needs_a_level_above_the_floor_and_missing_sessions_do_not_stop_judging() {
    let discord = || Pc::new().window(window(70, "discord.exe", CHROME, "#一般 | 開発 - Discord"), vec![]);
    let mut run = Run::new();
    assert!(run.hold(&discord().mic("discord.exe").sound("discord.exe", 0.001), 20).is_empty());
    let mut run = Run::new();
    assert_eq!(run.hold(&discord().mic("discord.exe").sound("discord.exe", 0.05), 6), started(MeetingApp::Discord));

    let pc = Pc::new().window(window(70, "discord.exe", CHROME, "#一般 | 開発 - Discord"), vec![button("切断")]);
    let mut facts = pc.facts();
    facts.capture = AudioSessions { available: false, error: Some("0x88890004".into()), ..Default::default() };
    let evaluation = evaluate(Rules::builtin(), &facts, &Browsers::builtin());
    let mut judge = Judge::new(Rules::builtin());
    assert_eq!(judge.tick(&evaluation, 0).iter().map(|d| d.event).collect::<Vec<_>>(), started(MeetingApp::Discord));
}

/// Zoom の会議の窓（ZPContentViewWndClass・ConfMultiTabContentWndClass）は、見えているものだけ強い印。
/// 見えない窓と、参加前のプレビューの窓では立たない。
// [MTG-027]
#[test]
fn zoom_meeting_windows_count_only_while_visible() {
    for class in ["ZPContentViewWndClass", "ConfMultiTabContentWndClass"] {
        let mut run = Run::new();
        assert_eq!(run.tick(&Pc::new().window(window(80, "zoom.exe", class, "Zoom ミーティング"), vec![])), started(MeetingApp::Zoom), "{class}");
    }
    let mut run = Run::new();
    let not_yet = Pc::new()
        .window(hidden(81, "zoom.exe", "ConfMultiTabContentWndClass", "Zoom ミーティング"), vec![])
        .window(window(82, "zoom.exe", "VideoPreviewWndClass", "ビデオのプレビュー"), vec![])
        .mic("zoom.exe");
    assert!(run.hold(&not_yet, 30).is_empty());
    // ボタンの列が隠れて部品が読めない間も、会議の窓で続く
    let mut run = Run::new();
    let meeting = Pc::new().window(window(80, "zoom.exe", "ZPContentViewWndClass", "Zoom ミーティング"), vec![button("退出")]);
    assert_eq!(run.tick(&meeting), started(MeetingApp::Zoom));
    assert!(run.hold(&Pc::new().window(window(80, "zoom.exe", "ZPContentViewWndClass", "Zoom ミーティング"), vec![]), 60).is_empty());
}

/// Teams（実機で測っていない）: 参加前の画面の参加ボタン（打ち消し）の間は始めず、参加して退出ボタンが出たら始める。
// [MTG-004][MTG-007]
#[test]
fn teams_starts_after_the_pre_join_screen() {
    let pre_join = Pc::new().window(window(90, "ms-teams.exe", CHROME, "会議 | Microsoft Teams"), vec![button("今すぐ参加")]).mic("ms-teams.exe");
    let mut run = Run::new();
    assert!(run.hold(&pre_join, 20).is_empty());
    let joined = Pc::new().window(window(90, "ms-teams.exe", CHROME, "会議 | Microsoft Teams"), vec![button("退出")]).mic("ms-teams.exe");
    assert_eq!(run.tick(&joined), started(MeetingApp::Teams));
}

/// 開始の理由には、立っていた印の名前と文脈を書く（ログと記録に残す）。
// [MTG-025][LOG-005]
#[test]
fn decisions_tell_which_signals_started_and_ended_a_meeting() {
    let mut judge = Judge::new(Rules::builtin());
    let joined = meet_in_chrome(vec![button("通話から退出")]).mic("chrome.exe");
    let decisions = judge.tick(&joined.evaluation(), 0);
    assert_eq!(decisions[0].reason.describe(), "強い印（chrome.exe の窓）: 退出ボタン");
    let decisions = judge.tick(&Pc::new().evaluation(), 2_000);
    assert!(decisions.is_empty());
    let decisions = judge.tick(&Pc::new().evaluation(), 22_000);
    assert_eq!(decisions[0].reason.describe(), "強い印も組み合わせもない状態が 20 秒続いた");

    let mut judge = Judge::new(Rules::builtin());
    let combination = Pc::new().window(window(70, "discord.exe", CHROME, "#一般 | 開発 - Discord"), vec![]).mic("discord.exe").sound("discord.exe", 0.3);
    judge.tick(&combination.evaluation(), 0);
    judge.tick(&combination.evaluation(), 2_000);
    let decisions = judge.tick(&combination.evaluation(), 4_000);
    assert_eq!(decisions[0].reason.describe(), "中・弱い印の組み合わせ（discord.exe（アプリ））: Discord のマイク・Discord の再生の音");
}

/// ブラウザの窓・部品の印は、アドレスバーが示すサイトがそのサービスのときだけ立つ。会議のサイトでないページ
/// （同じ題と「Leave call」のボタンを置いたページ・サイトを読めなかった窓）では、マイクを使っていても始めない。
/// 判定表にサイトを足せば（模擬のページで確かめるテスト）、そのページで始める。
// [MTG-031]
#[test]
fn browser_signals_need_the_service_site() {
    let page = |site: Option<&str>| {
        let mut tab = window(10, "msedge.exe", CHROME, "Meet - abc-defg-hij - Microsoft\u{200b} Edge");
        tab.site = site.map(String::from);
        Pc::new().window(tab, vec![button("Leave call")]).mic("msedge.exe").sound("msedge.exe", 0.3)
    };
    for (label, site) in [("ほかのサイト", Some("127.0.0.1")), ("似た名前のサイト", Some("meet.google.com.example.net")), ("サイトを読めない", None)] {
        let mut run = Run::new();
        assert!(run.hold(&page(site), 30).is_empty(), "{label}");
        assert_eq!(run.state(MEET), ServiceState::None, "{label}");
    }
    let mut run = Run::new();
    assert_eq!(run.tick(&page(Some("meet.google.com"))), started(MEET));

    let rules = Rules::builtin().with_site(MEET, "127.0.0.1");
    let evaluation = evaluate(&rules, &page(Some("127.0.0.1")).facts(), &Browsers::builtin());
    let mut judge = Judge::new(&rules);
    assert_eq!(judge.tick(&evaluation, 0).iter().map(|d| d.event).collect::<Vec<_>>(), started(MEET));

    // サブドメインも、そのサービスのサイト
    let slack = Pc::new()
        .window(window(11, "chrome.exe", CHROME, "ハドル: #dev - Acme - Slack - Google Chrome").at_site("acme.slack.com"), vec![button("ハドルから退出する")]);
    let mut run = Run::new();
    assert_eq!(run.tick(&slack), started(SLACK));
}

fn discord_web(site: &str, controls: Vec<ControlFact>) -> Pc {
    Pc::new().window(window(30, "chrome.exe", CHROME, "#general | Acme - Discord - Google Chrome").at_site(site), controls)
}

/// ブラウザ版 Discord は、切断ボタンで始め、切断ボタンで続ける。タブの題（チャンネルの名前）・ブラウザのマイク・再生の音は、
/// その通話のものか分からないので数えない。切断ボタンが読めない回が少しあっても、終了の猶予の間に戻れば続く。
/// 切断したあと、チャンネルの画面のまま、同じブラウザの別のタブがマイクを使い続けても（別の会議・音声入力）、20 秒で終える。
// [MTG-008][MTG-032]
#[test]
fn discord_in_a_browser_needs_the_call_itself() {
    let mut run = Run::new();
    assert!(run.hold(&discord_web("discord.com", vec![]).mic("chrome.exe").sound("chrome.exe", 0.3), 30).is_empty(), "題とマイクと再生の音");

    let mut run = Run::new();
    let call = discord_web("discord.com", vec![button("Disconnect")]).mic("chrome.exe");
    assert_eq!(run.tick(&call), started(MeetingApp::Discord));
    // 切断ボタンが読めない回が続いても、20 秒以内に戻れば続く
    assert!(run.hold(&discord_web("discord.com", vec![]).mic("chrome.exe"), 10).is_empty());
    assert!(run.hold(&call, 10).is_empty());
    assert_eq!(run.judge.phase(), Phase::Active(MeetingApp::Discord));
    // 切断した: チャンネルの画面のまま、別のタブがマイクを使い、音楽が流れている
    let left = discord_web("discord.com", vec![]).mic("chrome.exe").sound("chrome.exe", 0.4);
    assert!(run.hold(&left, 20).is_empty());
    assert_eq!(run.tick(&left), ended(MeetingApp::Discord), "切断から 20 秒");
    assert!(run.hold(&left, 30).is_empty());
}

/// 窓の中の部品の一部を読めなかった回（部品の数・部品・名前の取得の失敗）は、組み合わせの開始に要る調べた回に数えない。
/// 読めた部品の強い印では始める。
// [MTG-007]
#[test]
fn a_partly_read_window_does_not_count_as_a_scan() {
    let partly = |controls: Vec<ControlFact>| {
        let mut facts = meet_in_chrome(controls).mic("chrome.exe").facts();
        facts.scan.as_mut().unwrap().windows[0].incomplete = Some("3 個の部品を読めない（0x80040201）".into());
        facts
    };
    let evaluation = evaluate(Rules::builtin(), &partly(vec![]), &Browsers::builtin());
    let mut judge = Judge::new(Rules::builtin());
    let events: Vec<MeetingEvent> = (0..15).flat_map(|i| judge.tick(&evaluation, i * POLL_MS)).map(|d| d.event).collect();
    assert!(events.is_empty(), "{events:?}");
    assert_eq!(judge.phase(), Phase::Confirming(MEET), "調べ切れるまで確定を待つ");

    let evaluation = evaluate(Rules::builtin(), &partly(vec![button("通話から退出")]), &Browsers::builtin());
    let mut judge = Judge::new(Rules::builtin());
    assert_eq!(judge.tick(&evaluation, 0).iter().map(|d| d.event).collect::<Vec<_>>(), started(MEET));
}

/// Meet の会議中に、チャットの本文（文字の部品）に「再参加」や、退出後の画面の見出しと同じ文が見えても、退出ボタンがあれば続く。
/// 退出後の画面は、退出後の画面のボタンで見分け、すぐに終える。ボタンを読めず見出しの文字だけが見えるときは、20 秒の猶予のあとで終える。
// [MTG-008]
#[test]
fn meeting_chat_text_does_not_end_a_meet_call() {
    let mut run = Run::new();
    assert_eq!(run.tick(&meet_in_chrome(vec![button("通話から退出")]).mic("chrome.exe")), started(MEET));
    let chatting = meet_in_chrome(vec![
        button("通話から退出"),
        control(ControlType::Text, "後で再参加します"),
        control(ControlType::Text, "山田さんはミーティングから退出しました"),
        control(ControlType::Text, "ミーティングから退出しました"),
        control(ControlType::Text, "You left the meeting"),
    ])
    .mic("chrome.exe");
    assert!(run.hold(&chatting, 60).is_empty(), "見出しと同じ文のチャット");
    assert_eq!(run.judge.phase(), Phase::Active(MEET));
    let heading_only = meet_in_chrome(vec![control(ControlType::Text, "ミーティングから退出しました")]);
    assert!(run.hold(&heading_only, 20).is_empty(), "見出しの文字だけでは、すぐには終えない");
    assert_eq!(run.tick(&heading_only), ended(MEET));
    let mut run = Run::new();
    assert_eq!(run.tick(&chatting), started(MEET));
    let left = meet_in_chrome(vec![control(ControlType::Text, "ミーティングから退出しました"), button("再参加")]);
    assert_eq!(run.tick(&left), ended(MEET));
}

/// デスクトップの会議アプリの文脈（マイクと再生の音）は、そのアプリの見えている題のある窓のすべての中を調べ切れた回だけを、
/// 組み合わせの開始に要る調べた回に数える。参加前の画面の窓を読めず、別の窓だけを読めた回では始めない。
// [MTG-007]
#[test]
fn a_desktop_app_counts_a_scan_only_when_all_its_windows_were_read() {
    let teams = || {
        Pc::new()
            .window(window(90, "ms-teams.exe", CHROME, "会議の準備 | Microsoft Teams"), vec![])
            .window(window(91, "ms-teams.exe", CHROME, "チャット | Microsoft Teams"), vec![])
            .window(window(92, "ms-teams.exe", CHROME, ""), vec![])
            .mic("ms-teams.exe")
            .sound("ms-teams.exe", 0.3)
    };
    let mut facts = teams().facts();
    for scanned in facts.scan.as_mut().unwrap().windows.iter_mut() {
        match scanned.hwnd {
            90 => scanned.incomplete = Some("本文を読めない".into()),
            92 => scanned.error = Some("0x80004005".into()),
            _ => {}
        }
    }
    let evaluation = evaluate(Rules::builtin(), &facts, &Browsers::builtin());
    let mut judge = Judge::new(Rules::builtin());
    let events: Vec<MeetingEvent> = (0..15).flat_map(|i| judge.tick(&evaluation, i * POLL_MS)).map(|d| d.event).collect();
    assert!(events.is_empty(), "{events:?}");
    // 題のない窓（ツールチップなど）を読めなくても、題のある窓をすべて読めれば数える
    let mut facts = teams().facts();
    facts.scan.as_mut().unwrap().windows.iter_mut().filter(|scanned| scanned.hwnd == 92).for_each(|scanned| scanned.error = Some("0x80004005".into()));
    let evaluation = evaluate(Rules::builtin(), &facts, &Browsers::builtin());
    let mut judge = Judge::new(Rules::builtin());
    let events: Vec<MeetingEvent> = (0..3).flat_map(|i| judge.tick(&evaluation, i * POLL_MS)).map(|d| d.event).collect();
    assert_eq!(events, started(MeetingApp::Teams));
}

/// 会議アプリの窓をすべてトレイに隠したとき（窓が見えない）: 最初から隠していれば、マイクと再生の音がそろっても始めない
/// （窓の中を調べられないため）。始まったあとに隠したときは、マイクと再生の音がある間は続き、マイクだけになると 20 秒で終える。
// [MTG-033]
#[test]
fn meeting_apps_hidden_in_the_tray() {
    let tray = || Pc::new().window(hidden(60, "slack.exe", CHROME, "Huddle: 山田 - Acme - Slack"), vec![]);
    let mut run = Run::new();
    assert!(run.hold(&tray().mic("slack.exe").sound("slack.exe", 0.3), 60).is_empty(), "最初から隠していると始めない");

    let mut run = Run::new();
    let shown = Pc::new().window(window(60, "slack.exe", CHROME, "Huddle: 山田 - Acme - Slack"), vec![button("退出")]).mic("slack.exe");
    assert_eq!(run.tick(&shown), started(SLACK));
    assert!(run.hold(&tray().mic("slack.exe").sound("slack.exe", 0.3), 60).is_empty(), "マイクと再生の音で続く");
    let silent = tray().mic("slack.exe");
    assert!(run.hold(&silent, 20).is_empty());
    assert_eq!(run.tick(&silent), ended(SLACK), "マイクだけ（弱い印 1 つ）になってから 20 秒");
}
