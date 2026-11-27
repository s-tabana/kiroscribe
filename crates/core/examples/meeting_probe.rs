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
//! - `--titles`: マイクを使っているブラウザの、表示中のウィンドウのタイトルも出す（メールの件名なども出るので、人に見せる前に確かめる）

use std::time::Duration;

use kiroscribe_core::meetings::{
    browser_titles, diagnose, observe_detailed, MeetingEvent, MeetingTracker, MicSource, MicSources, Observation, Phase,
};
use kiroscribe_core::mic_sessions::MicSessions;
use kiroscribe_core::mic_store::{self, MicStore};
use kiroscribe_core::time::now_ms;

struct Options {
    once: bool,
    interval: Duration,
    titles: bool,
}

fn options() -> Result<Options, String> {
    let mut options = Options { once: false, interval: Duration::from_secs(2), titles: false };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--once" => options.once = true,
            "--titles" => options.titles = true,
            "--interval" => {
                let seconds: u64 = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .filter(|seconds| *seconds > 0)
                    .ok_or("--interval には 1 以上の秒数を指定してください")?;
                options.interval = Duration::from_secs(seconds);
            }
            other => return Err(format!("分からない引数です: {other}（使えるのは --once・--interval <秒>・--titles）")),
        }
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
    if !sessions.available {
        return vec![format!(
            "  オーディオのセッション: 取れない（{}）",
            sessions.error.as_deref().unwrap_or("Windows 以外")
        )];
    }
    let report = kiroscribe_core::mic_sessions::report(sessions);
    let mut lines = vec![format!(
        "  オーディオのセッション: 録音のデバイス {} 台、セッション {} 件",
        report.endpoints,
        report.sessions.len()
    )];
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
    }
    lines
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
            break;
        }
        std::thread::sleep(options.interval);
    }
}
