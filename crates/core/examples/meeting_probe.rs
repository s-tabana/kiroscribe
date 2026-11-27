//! 会議の検出の確認用の道具: 数秒ごとに、オーディオのセッションの一覧と、それを会議と判定したかを表示する。
//!
//! ```text
//! cargo build -p kiroscribe-core --release --example meeting_probe
//! ```
//!
//! 製品（Kiroscribe）と同じ部品（`kiroscribe_core::meetings`）で、同じ間隔（2 秒）・同じ判定をする。
//! 表示するのは、実行ファイルの名前・パッケージファミリー名・セッションの状態と、判定の結果だけ（フルパスは出さない）。
//!
//! 引数:
//! - `--once`: 1 回だけ調べて終わる
//! - `--interval <秒>`: 調べる間隔（既定は 2 秒）
//! - `--titles`: マイクを使っているブラウザと、デスクトップの会議アプリ（slack.exe など）の、表示中のウィンドウのタイトルも出す
//!   （メールの件名・チャンネルや相手の名前なども出るので、人に見せる前に確かめる）
//! - `--require-sessions`: `--once` で録音セッションの一覧を取得できなければ終了コード 1 を返す
//! - `--expect <アプリ>`: `--once` で指定した会議と判定できなければ終了コード 1 を返す
//!   （`zoom`・`slack`・`teams`・`discord`・`meet`）

use std::time::Duration;

use kiroscribe_core::meetings::{
    browser_titles, diagnose, observe_detailed, MeetingApp, MeetingEvent, MeetingTracker, MicSource, MicSources,
    Observation, Phase,
};
use kiroscribe_core::mic_sessions::{MicSessions, ReadStatus};
use kiroscribe_core::mic_store::{self, MicStore};
use kiroscribe_core::time::now_ms;

struct Options {
    once: bool,
    interval: Duration,
    titles: bool,
    require_sessions: bool,
    expect: Option<MeetingApp>,
}

fn expected_app(name: &str) -> Option<MeetingApp> {
    match name.to_lowercase().as_str() {
        "zoom" => Some(MeetingApp::Zoom),
        "slack" => Some(MeetingApp::SlackHuddle),
        "teams" => Some(MeetingApp::Teams),
        "discord" => Some(MeetingApp::Discord),
        "meet" => Some(MeetingApp::GoogleMeet),
        _ => None,
    }
}

fn options() -> Result<Options, String> {
    let mut options =
        Options { once: false, interval: Duration::from_secs(2), titles: false, require_sessions: false, expect: None };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--once" => options.once = true,
            "--titles" => options.titles = true,
            "--require-sessions" => options.require_sessions = true,
            "--expect" => {
                let app = args
                    .next()
                    .and_then(|value| expected_app(&value))
                    .ok_or("--expect には zoom・slack・teams・discord・meet のどれかを指定してください")?;
                options.expect = Some(app);
            }
            "--interval" => {
                let seconds: u64 = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .filter(|seconds| *seconds > 0)
                    .ok_or("--interval には 1 以上の秒数を指定してください")?;
                options.interval = Duration::from_secs(seconds);
            }
            other => {
                return Err(format!(
                    "分からない引数です: {other}（使えるのは --once・--interval <秒>・--titles・--require-sessions・--expect <アプリ>）"
                ))
            }
        }
    }
    if !options.once && (options.require_sessions || options.expect.is_some()) {
        return Err("--require-sessions と --expect は --once と組み合わせてください".into());
    }
    Ok(options)
}

fn phase_text(phase: Phase) -> String {
    match phase {
        Phase::Idle => "会議なし".into(),
        Phase::Confirming(app) => format!("開始の確定待ち（{}）", app.label()),
        Phase::Active(app) => format!("会議中（{}）", app.label()),
        Phase::Fading(app) => format!("終了の猶予の間（{}）", app.label()),
    }
}

fn sessions_lines(sessions: &MicSessions) -> Vec<String> {
    let report = kiroscribe_core::mic_sessions::report(sessions, true);
    let status = match report.status {
        ReadStatus::Ok => "取得済み",
        ReadStatus::NoDevices => "録音デバイスなし",
        ReadStatus::Partial => "一部取得",
        ReadStatus::Failed => "取得失敗",
    };
    let mut lines = vec![format!(
        "  オーディオのセッション: {status}（録音デバイス {} 台、取得したセッション {} 件）",
        report.endpoints, report.total
    )];
    if let Some(error) = report.error {
        lines.push(format!(
            "  取得失敗: デバイス {} 台、セッション {} 件（最初の理由 {error}）",
            report.failed_endpoints, report.failed_sessions
        ));
    }
    for session in &report.sessions {
        let package = session.package.as_deref().map(|p| format!("  [{p}]")).unwrap_or_default();
        let meaning = match (session.app, session.browser) {
            (Some(app), _) => format!("  → 会議アプリ: {}", app.label()),
            (None, true) => "  → ブラウザ（タブの題で判定）".into(),
            (None, false) => String::new(),
        };
        lines.push(format!("    {:<8}  {}{}{}", session.state.name(), session.exe, package, meaning));
    }
    lines
}

/// 再生のセッションのうち、会議アプリとブラウザのもの（判定には使わない。通話中に音を鳴らしているかを見るため）。
fn playback_lines() -> Vec<String> {
    let playback = kiroscribe_core::mic_sessions::read_playback();
    if !playback.available {
        return vec![format!("  再生のセッション: 取れない（{}）", playback.error.as_deref().unwrap_or("Windows 以外"))];
    }
    let report = kiroscribe_core::mic_sessions::report(&playback, true);
    let sessions: Vec<_> = report.sessions.iter().filter(|s| s.app.is_some() || s.browser).collect();
    let mut lines = vec![format!(
        "  再生のセッション（会議アプリ・ブラウザだけ。判定には使わない）: 再生のデバイス {} 台、{} 件",
        report.endpoints,
        sessions.len()
    )];
    for session in sessions {
        let package = session.package.as_deref().map(|p| format!("  [{p}]")).unwrap_or_default();
        lines.push(format!("    {:<8}  {}{}", session.state.name(), session.exe, package));
    }
    lines
}

fn store_line(store: &MicStore) -> String {
    if !store.opened {
        return "  ConsentStore: 開けない".into();
    }
    let in_use: Vec<String> = store
        .entries
        .iter()
        .filter(|entry| entry.in_use())
        .map(|entry| kiroscribe_core::meetings::app_name(&entry.id))
        .collect();
    let latest = store.entries.iter().map(|entry| entry.last_start).max().unwrap_or(0);
    let latest = if latest > 0 {
        chrono::DateTime::from_timestamp_millis(mic_store::filetime_to_unix_ms(latest))
            .map(|at| at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default()
    } else {
        "なし".into()
    };
    format!(
        "  ConsentStore（補助）: 記録 {} 件、使用中 {}、最新の使用開始 {}",
        store.entries.len(),
        if in_use.is_empty() { "なし".into() } else { in_use.join("・") },
        latest
    )
}

fn source_text(sources: Option<&MicSources>) -> &'static str {
    match sources.map(MicSources::source) {
        Some(MicSource::AudioSessions) => "オーディオのセッション",
        Some(MicSource::ConsentStore) => "ConsentStore（オーディオのセッションを取れないため）",
        None => "なし（Windows 以外では検出しない）",
    }
}

/// 1 回分の表示。最初の行（時刻）を除いて、前回と同じなら短く出す。
fn render(observation: &Observation, sources: Option<&MicSources>, tracker: &MeetingTracker, titles: bool) -> Vec<String> {
    let diagnosis = diagnose(observation, None);
    let mut lines = vec![format!("  判定に使う手がかり: {}", source_text(sources))];
    if let Some(sources) = sources {
        lines.extend(sessions_lines(&sources.sessions));
        lines.extend(playback_lines());
        lines.push(store_line(&sources.store));
    }
    lines.push(format!(
        "  マイクを使っているアプリ: {}",
        if diagnosis.mic_in_use.is_empty() { "なし".into() } else { diagnosis.mic_in_use.join("・") }
    ));
    let verdict = match diagnosis.app {
        Some(app) => format!("会議とみなす: {}（{}）", app.label(), diagnosis.verdict.describe()),
        None => format!("会議とみなさない（{}）", diagnosis.verdict.describe()),
    };
    lines.push(format!("  判定: {verdict}"));
    lines.push(format!("  段階: {}", phase_text(tracker.phase())));
    if titles {
        for title in browser_titles(observation, None) {
            lines.push(format!("  ブラウザのタブ: {title}"));
        }
        for title in meeting_app_titles() {
            lines.push(format!("  会議アプリのウィンドウ: {title}"));
        }
    }
    lines
}

/// デスクトップの会議アプリの、表示中のウィンドウのタイトル（マイクを使っているかによらない）。
fn meeting_app_titles() -> Vec<String> {
    #[cfg(windows)]
    {
        kiroscribe_core::meetings::windows_observer::list_windows()
            .into_iter()
            .filter(|window| kiroscribe_core::meetings::desktop_app(&window.exe).is_some())
            .map(|window| format!("{}: {}", window.exe, window.title))
            .collect()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

fn main() {
    let options = match options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    println!(
        "Kiroscribe 会議の検出の確認（meeting_probe {}）。{} 秒ごとに調べます。Ctrl+C で終わります。",
        env!("CARGO_PKG_VERSION"),
        options.interval.as_secs()
    );
    let mut tracker = MeetingTracker::default();
    let mut last: Option<Vec<String>> = None;
    loop {
        let now = now_ms();
        let (observation, sources) = observe_detailed();
        let event = tracker.observe(&observation, None, now);
        let lines = render(&observation, sources.as_ref(), &tracker, options.titles);
        let time = chrono::Local::now().format("%H:%M:%S");
        match event {
            Some(MeetingEvent::Started(app)) => println!("[{time}] ★ 会議の開始を確定: {}（製品なら録音を始める）", app.label()),
            Some(MeetingEvent::Ended(app)) => println!("[{time}] ★ 会議の終了を確定: {}（製品なら録音を止める）", app.label()),
            None => {}
        }
        if last.as_ref() == Some(&lines) {
            println!("[{time}] 変化なし");
        } else {
            println!("[{time}]");
            for line in &lines {
                println!("{line}");
            }
            last = Some(lines);
        }
        if options.once {
            if let Some(message) = check(&options, &observation, sources.as_ref()) {
                eprintln!("確認に失敗しました: {message}");
                std::process::exit(1);
            }
            return;
        }
        std::thread::sleep(options.interval);
    }
}

/// `--require-sessions`・`--expect` の確認。満たさないなら、その理由。
fn check(options: &Options, observation: &Observation, sources: Option<&MicSources>) -> Option<String> {
    if options.require_sessions {
        match sources {
            None => return Some("Windows 以外では、オーディオのセッションを読めない".into()),
            Some(sources) if !sources.sessions.available => {
                return Some(format!(
                    "録音のオーディオのセッションの一覧を取れない（{}）",
                    sources.sessions.error.as_deref().unwrap_or("理由不明")
                ))
            }
            Some(_) => {}
        }
    }
    let expected = options.expect?;
    let diagnosis = diagnose(observation, None);
    (diagnosis.app != Some(expected)).then(|| {
        format!(
            "{} の会議と判定していない（判定: {}。{}）",
            expected.label(),
            diagnosis.app.map(|app| app.label()).unwrap_or("会議なし"),
            diagnosis.verdict.describe()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiroscribe_core::meetings::MicUser;
    use kiroscribe_core::mic_sessions::{summarize, EndpointSessions};

    // [MTG-020]
    #[test]
    fn once_can_require_the_session_list_and_the_final_verdict() {
        let options = Options {
            once: true,
            interval: Duration::from_secs(2),
            titles: false,
            require_sessions: true,
            expect: Some(MeetingApp::Zoom),
        };
        let mut observation = Observation::default();
        let mut sources = MicSources {
            sessions: summarize(vec![Ok(EndpointSessions::default())]),
            ..Default::default()
        };
        assert!(check(&options, &observation, Some(&sources)).is_some());
        observation.mic_users.push(MicUser { id: "Zoom.exe".into(), in_use: true });
        assert!(check(&options, &observation, Some(&sources)).is_none());
        sources.sessions = summarize(vec![Err("0x80070005".into())]);
        assert!(check(&options, &observation, Some(&sources)).unwrap().contains("0x80070005"));
        let lines = sessions_lines(&sources.sessions).join("\n");
        assert!(lines.contains("取得失敗") && lines.contains("デバイス 1 台") && lines.contains("0x80070005"));
    }
}
