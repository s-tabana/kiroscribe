//! 会議の自動検出の判定（どのアプリ・どのタイトルを会議とみなすか）の仕様のテスト。

use kiroscribe_core::meetings::{
    detect, observe, web_meeting, Detection, MeetingApp, MeetingEvent, MeetingTracker, MicUser,
    Observation, WindowInfo,
};

fn mic(id: &str) -> MicUser {
    MicUser {
        id: id.to_string(),
        in_use: true,
    }
}

fn window(exe: &str, title: &str) -> WindowInfo {
    WindowInfo {
        exe: exe.to_string(),
        title: title.to_string(),
    }
}

fn observation(mic_users: Vec<MicUser>, windows: Vec<WindowInfo>) -> Observation {
    Observation { mic_users, windows }
}

fn desktop(app: MeetingApp) -> Option<Detection> {
    Some(Detection::desktop(app))
}

/// Chrome での会議。
fn browser(app: MeetingApp) -> Option<Detection> {
    browser_in(app, "chrome.exe")
}

fn browser_in(app: MeetingApp, exe: &str) -> Option<Detection> {
    Some(Detection::in_browser(app, exe))
}

// [MTG-001]
#[test]
fn every_listed_desktop_app_is_a_meeting_while_it_uses_the_mic() {
    let cases = [
        (r"C:\Users\me\AppData\Local\slack\app-4.41.105\slack.exe", MeetingApp::SlackHuddle),
        (r"C:\Users\me\AppData\Roaming\Zoom\bin\Zoom.exe", MeetingApp::Zoom),
        (r"C:\Users\me\AppData\Roaming\Zoom\bin\CptHost.exe", MeetingApp::Zoom),
        (r"C:\Program Files\WindowsApps\MSTeams_25000.1.2.3_x64__8wekyb3d8bbwe\ms-teams.exe", MeetingApp::Teams),
        (r"C:\Users\me\AppData\Local\Microsoft\Teams\current\Teams.exe", MeetingApp::Teams),
        ("MSTeams_8wekyb3d8bbwe", MeetingApp::Teams),
        ("MicrosoftTeams_8wekyb3d8bbwe", MeetingApp::Teams),
        ("msteams_8wekyb3d8bbwe", MeetingApp::Teams),
        (r"C:\Users\me\AppData\Local\Discord\app-1.0.9163\Discord.exe", MeetingApp::Discord),
        (r"C:\Users\me\AppData\Local\DiscordPTB\app-1.0.1\DiscordPTB.exe", MeetingApp::Discord),
        (r"C:\Users\me\AppData\Local\DiscordCanary\app-1.0.1\DISCORDCANARY.EXE", MeetingApp::Discord),
    ];
    for (id, app) in cases {
        assert_eq!(detect(&observation(vec![mic(id)], vec![]), None), desktop(app), "{id}");
        // マイクを使っていなければ対象にしない
        let idle = MicUser {
            id: id.to_string(),
            in_use: false,
        };
        assert_eq!(detect(&observation(vec![idle], vec![]), None), None, "{id}");
    }
    // 名前が似ているだけのアプリは対象にしない
    for id in [r"C:\Tools\zoomit.exe", r"C:\Tools\slack-helper.exe", "MSTeamsX", r"C:\Apps\Audacity\audacity.exe"] {
        assert_eq!(detect(&observation(vec![mic(id)], vec![]), None), None, "{id}");
    }
}

// [MTG-002]
#[test]
fn browser_titles_that_count_as_meetings() {
    let cases = [
        ("Meet - abc-defg-hij - Google Chrome", Some(MeetingApp::GoogleMeet)),
        ("Meet – 週次定例", Some(MeetingApp::GoogleMeet)),
        ("Meet: 週次定例", Some(MeetingApp::GoogleMeet)),
        ("Meet：週次定例", Some(MeetingApp::GoogleMeet)),
        ("Google Meet", Some(MeetingApp::GoogleMeet)),
        ("週次定例 - Meet - Microsoft Edge", Some(MeetingApp::GoogleMeet)),
        ("meet.google.com/abc-defg-hij", Some(MeetingApp::GoogleMeet)),
        ("Huddle with 田中 - Slack", Some(MeetingApp::SlackHuddle)),
        ("Slack | ハドルミーティング", Some(MeetingApp::SlackHuddle)),
        ("Meeting with 田中 | Microsoft Teams", Some(MeetingApp::Teams)),
        ("会議 | Microsoft Teams", Some(MeetingApp::Teams)),
        ("Call with 田中 | Microsoft Teams", Some(MeetingApp::Teams)),
        ("通話 | Microsoft Teams", Some(MeetingApp::Teams)),
        ("Zoom Meeting", Some(MeetingApp::Zoom)),
        ("Zoom ミーティング", Some(MeetingApp::Zoom)),
        ("Discord | #雑談 | 開発チーム", Some(MeetingApp::Discord)),
        ("Discord | #雑談 | 開発チーム - Google Chrome", Some(MeetingApp::Discord)),
        ("#general | Acme - Discord - Google Chrome", Some(MeetingApp::Discord)),
        ("#general | Acme - Discord", Some(MeetingApp::Discord)),
        ("フレンド | Discord — Mozilla Firefox", Some(MeetingApp::Discord)),
        ("(3) Discord | #general | Dev Team", Some(MeetingApp::Discord)),
        ("• Discord | フレンド", Some(MeetingApp::Discord)),
        ("(99+) • Discord | 🔊 作業部屋 | 開発チーム — Mozilla Firefox", Some(MeetingApp::Discord)),
        // 会議とみなさないもの
        // Discord についての記事・検索結果・ヘルプのページ（タイトルに「discord」を含むだけ）
        ("How to use Discord - Google 検索", None),
        ("How to use Discord - Google 検索 - Google Chrome", None),
        ("Discord とは - Wikipedia - Microsoft Edge", None),
        ("Discord とは - Wikipedia", None),
        ("Voice Channels FAQ – Discord", None),
        ("Discord - Group Chat That’s All Fun & Games", None),
        ("Discord の使い方 | 技術ブログ", None),
        ("(メモ) Discord | の設定", None),
        ("Slack | general | 開発チーム", None),
        ("チャット | Microsoft Teams", None),
        ("Zoom - 料金プラン", None),
        ("Meetup - 東京の勉強会", None),
        ("Gmail - 受信トレイ", None),
    ];
    for (title, expected) in cases {
        assert_eq!(web_meeting(title), expected, "{title}");
    }
}

// [MTG-002]
#[test]
fn every_listed_browser_is_checked_and_other_apps_are_not() {
    for exe in ["chrome.exe", "msedge.exe", "firefox.exe", "brave.exe", "vivaldi.exe", "opera.exe", "arc.exe"] {
        let seen = observation(
            vec![mic(&format!(r"C:\Program Files\Browser\{exe}"))],
            vec![window(exe, "Meet - abc-defg-hij")],
        );
        assert_eq!(detect(&seen, None), browser_in(MeetingApp::GoogleMeet, exe), "{exe}");
    }
    // ブラウザでないアプリのウィンドウのタイトルは見ない
    let other = observation(
        vec![mic(r"C:\Program Files\Google\Chrome\Application\chrome.exe")],
        vec![window("notepad.exe", "Meet - メモ.txt")],
    );
    assert_eq!(detect(&other, None), None);
    // ブラウザでないアプリがマイクを使っていても、ブラウザの会議にはしない
    let recorder = observation(
        vec![mic(r"C:\Apps\Audacity\audacity.exe")],
        vec![window("chrome.exe", "Meet - abc-defg-hij")],
    );
    assert_eq!(detect(&recorder, None), None);
}

// [MTG-002]
#[test]
fn the_meeting_title_must_belong_to_the_browser_that_uses_the_mic() {
    const CHROME: &str = r"C:\Program Files\Google\Chrome\Application\chrome.exe";
    let titles = [
        ("Meet - abc-defg-hij", MeetingApp::GoogleMeet),
        ("Huddle with 田中 - Slack", MeetingApp::SlackHuddle),
        ("Meeting with 田中 | Microsoft Teams", MeetingApp::Teams),
        ("Zoom Meeting", MeetingApp::Zoom),
        ("Discord | #雑談 | 開発チーム", MeetingApp::Discord),
    ];
    for (title, app) in titles {
        // Chrome がマイクを使っていて、会議のタブは Edge にある: 会議とみなさない
        let other_browser = observation(vec![mic(CHROME)], vec![window("msedge.exe", title)]);
        assert_eq!(detect(&other_browser, None), None, "{title}");

        // 同じブラウザなら会議とみなす（ほかのブラウザのウィンドウが並んでいても）
        let same_browser = observation(
            vec![mic(CHROME)],
            vec![window("msedge.exe", "ニュース - Microsoft Edge"), window("chrome.exe", title)],
        );
        assert_eq!(detect(&same_browser, None), browser(app), "{title}");
    }

    // 2 つのブラウザがどちらもマイクを使っているときは、どちらのタブでもよい
    let both = observation(
        vec![mic(CHROME), mic(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe")],
        vec![window("msedge.exe", "Zoom Meeting")],
    );
    assert_eq!(detect(&both, None), browser_in(MeetingApp::Zoom, "msedge.exe"));
}

/// Discord についてのページを開いたまま、別のサイトでマイクを使っても、会議とみなさない。
// [MTG-002]
#[test]
fn pages_about_discord_are_not_meetings() {
    for title in [
        "How to use Discord - Google 検索 - Google Chrome",
        "Discord とは - Wikipedia - Google Chrome",
        "Discord の使い方 - 技術ブログ - Google Chrome",
    ] {
        let seen = observation(
            vec![mic(r"C:\Program Files\Google\Chrome\Application\chrome.exe")],
            vec![window("chrome.exe", title)],
        );
        assert_eq!(detect(&seen, None), None, "{title}");
    }
}

// [MTG-006]
#[test]
fn desktop_app_takes_priority_over_a_browser_meeting() {
    let both = observation(
        vec![
            mic(r"C:\Program Files\Google\Chrome\Application\chrome.exe"),
            mic(r"C:\Users\me\AppData\Roaming\Zoom\bin\Zoom.exe"),
        ],
        vec![window("chrome.exe", "Meet - abc-defg-hij - Google Chrome")],
    );
    assert_eq!(detect(&both, None), desktop(MeetingApp::Zoom));
}

// [MTG-010]
#[test]
fn another_meeting_detected_while_one_continues_does_not_start_a_new_one() {
    let meet = browser(MeetingApp::GoogleMeet);
    let zoom = desktop(MeetingApp::Zoom);
    let mut tracker = MeetingTracker::default();
    assert_eq!(tracker.tick(meet, false, 0), None);
    assert_eq!(tracker.tick(meet, true, 4_000), Some(MeetingEvent::Started(MeetingApp::GoogleMeet)));

    // 会議が続いている間に別の会議が検出されても、開始にはしない
    for now in [6_000, 8_000, 30_000, 60_000] {
        assert_eq!(tracker.tick(zoom, true, now), None);
        assert_eq!(tracker.active().map(|d| d.app), Some(MeetingApp::GoogleMeet));
    }
}

/// 会議が終わって 20 秒たたないうちに別のアプリの会議が始まったら、前の会議を終わらせて、新しい会議を始める。
// [MTG-010]
#[test]
fn a_meeting_in_another_app_right_after_one_ends_is_a_new_meeting() {
    let meet = browser(MeetingApp::GoogleMeet);
    let zoom = desktop(MeetingApp::Zoom);
    let mut tracker = MeetingTracker::default();
    tracker.tick(meet, false, 0);
    assert_eq!(tracker.tick(meet, true, 4_000), Some(MeetingEvent::Started(MeetingApp::GoogleMeet)));

    // Meet を抜けて、6 秒後（終了が確定する 20 秒より前）に Zoom の会議に入る
    assert_eq!(tracker.tick(None, false, 10_000), None);
    assert_eq!(tracker.tick(zoom, false, 16_000), None, "一瞬の検出では切り替えない");
    assert_eq!(tracker.tick(zoom, false, 18_000), None);
    // Zoom が 4 秒続いたら、前の会議の終了を知らせ、次の回に新しい会議の開始を知らせる
    assert_eq!(tracker.tick(zoom, false, 20_000), Some(MeetingEvent::Ended(MeetingApp::GoogleMeet)));
    assert_eq!(tracker.active(), None);
    assert_eq!(tracker.tick(zoom, false, 22_000), Some(MeetingEvent::Started(MeetingApp::Zoom)));
    assert_eq!(tracker.active().map(|d| d.app), Some(MeetingApp::Zoom));
    assert_eq!(tracker.tick(zoom, true, 24_000), None, "開始は 1 回だけ");

    // Zoom が終われば、いつもどおり 20 秒後に終了
    assert_eq!(tracker.tick(None, false, 30_000), None);
    assert_eq!(tracker.tick(None, false, 50_000), Some(MeetingEvent::Ended(MeetingApp::Zoom)));
}

/// 別のアプリの会議が見えても、前の会議が戻ってくれば続き。別の会議が途切れたら数え直す。
// [MTG-010]
#[test]
fn the_previous_meeting_continues_if_it_comes_back_before_the_switch_is_confirmed() {
    let slack = desktop(MeetingApp::SlackHuddle);
    let teams = desktop(MeetingApp::Teams);
    let mut tracker = MeetingTracker::default();
    tracker.tick(slack, false, 0);
    assert_eq!(tracker.tick(slack, true, 4_000), Some(MeetingEvent::Started(MeetingApp::SlackHuddle)));

    // ハドルが一瞬途切れて Teams が見えたが、2 秒後にはハドルが戻った
    assert_eq!(tracker.tick(teams, false, 6_000), None);
    assert_eq!(tracker.tick(slack, true, 8_000), None);
    assert_eq!(tracker.active().map(|d| d.app), Some(MeetingApp::SlackHuddle));
    // そのあとまた Teams が見えても、4 秒を数え直す
    assert_eq!(tracker.tick(teams, false, 10_000), None);
    assert_eq!(tracker.tick(teams, false, 12_000), None);
    assert_eq!(tracker.tick(teams, false, 14_000), Some(MeetingEvent::Ended(MeetingApp::SlackHuddle)));
    assert_eq!(tracker.tick(teams, false, 16_000), Some(MeetingEvent::Started(MeetingApp::Teams)));

    // 別のアプリの会議が続かないまま 20 秒たてば、前の会議はふつうに終わる。そのあとの会議は、4 秒続いてから始まる
    let mut tracker = MeetingTracker::default();
    tracker.tick(slack, false, 0);
    tracker.tick(slack, true, 4_000);
    assert_eq!(tracker.tick(None, false, 6_000), None);
    assert_eq!(tracker.tick(teams, false, 22_000), None);
    assert_eq!(tracker.tick(teams, false, 24_000), Some(MeetingEvent::Ended(MeetingApp::SlackHuddle)), "見えなくなって 20 秒");
    assert_eq!(tracker.tick(teams, false, 26_000), Some(MeetingEvent::Started(MeetingApp::Teams)));
}

/// 同じアプリの会議がまた見えたときは、続きとして扱う（終了にしない）。
// [MTG-008][MTG-010]
#[test]
fn the_same_app_seen_again_within_the_grace_period_is_the_same_meeting() {
    let meet = browser(MeetingApp::GoogleMeet);
    let mut tracker = MeetingTracker::default();
    tracker.tick(meet, false, 0);
    tracker.tick(meet, true, 4_000);
    assert_eq!(tracker.tick(None, false, 10_000), None);
    for now in [20_000, 22_000, 24_000, 40_000] {
        assert_eq!(tracker.tick(meet, false, now), None);
    }
    assert_eq!(tracker.active().map(|d| d.app), Some(MeetingApp::GoogleMeet));
    assert_eq!(tracker.tick(None, false, 59_000), None);
    assert_eq!(tracker.tick(None, false, 60_000), Some(MeetingEvent::Ended(MeetingApp::GoogleMeet)));
}

// [MTG-021]
#[cfg(not(windows))]
#[test]
fn nothing_is_detected_outside_windows() {
    let seen = observe();
    assert_eq!(seen, Observation::default());
    assert_eq!(detect(&seen, None), None);
    let mut tracker = MeetingTracker::default();
    for tick in 0..20 {
        assert_eq!(tracker.observe(&seen, None, tick * 2_000), None);
    }
}

/// Windows では、調べた結果がどうであっても失敗しない（中身は環境による）。
#[cfg(windows)]
#[test]
fn observe_returns_on_windows() {
    let _ = observe();
}
