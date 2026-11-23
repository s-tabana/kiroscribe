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
    Some(Detection {
        app,
        via_browser: false,
    })
}

fn browser(app: MeetingApp) -> Option<Detection> {
    Some(Detection {
        app,
        via_browser: true,
    })
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
        assert_eq!(detect(&seen, None), browser(MeetingApp::GoogleMeet), "{exe}");
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
    assert_eq!(detect(&both, None), browser(MeetingApp::Zoom));
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
