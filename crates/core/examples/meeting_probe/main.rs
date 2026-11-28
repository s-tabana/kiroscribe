//! 会議の検出の確認用の道具: 数秒ごとに、オーディオのセッションの一覧と、それを会議と判定したかを表示する。
//! あわせて、会議中を示しうるほかの手がかり（`signals.rs`）を読み、変化があった行だけを出す。
//!
//! ```text
//! cargo build -p kiroscribe-core --release --example meeting_probe
//! ```
//!
//! 製品（Kiroscribe）と同じ部品（`kiroscribe_core::meetings`）で、同じ間隔（2 秒）・同じ判定をする。
//! 表示するのは、実行ファイルの名前・パッケージファミリー名・セッションの状態・数値・分類と、判定の結果だけ
//! （フルパス・音声は出さない）。出力は、画面とファイル（既定は exe と同じ場所の probe-output.txt）の両方に書く。
//! 引数と測り方の手順は `--help`（[`HELP`]）と docs/meeting-probe.md。

mod signals;
#[cfg(windows)]
mod win;

use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use kiroscribe_core::meetings::{
    browser_titles, diagnose, observe_detailed, MeetingApp, MeetingEvent, MeetingTracker, MicSource, MicSources,
    Observation, Phase,
};
use kiroscribe_core::mic_sessions::{MicSessions, ReadStatus};
use kiroscribe_core::mic_store::{self, MicStore};
use kiroscribe_core::time::now_ms;

const HELP: &str = "\
Kiroscribe 会議の検出の確認用の道具（meeting_probe）

使い方: meeting_probe.exe [引数]

2 秒ごとに次を調べ、変化があった行だけを出します。出力は画面と probe-output.txt（exe と同じフォルダ）に書きます。
- 製品と同じ判定（判定に使う手がかり・録音のセッションの一覧・判定・段階）
- 録音と再生のセッション（端点ごと・アプリごとの状態と音の大きさ。音の大きさはピークの数値だけ）
- 会議アプリとブラウザ（Chrome・Edge・Zen・Firefox など）の窓（プロセス・クラス名・見えているか・大きさ・最小化か・名前の分類）
- 窓の中の部品（退出・ミュート・カメラ・画面共有のボタンがあるか。既定は 5 秒ごと）
- カメラの使用（Windows の記録 ConsentStore の webcam）
Enter で区切った 1 行（例: discord 入った）は、時刻つきの目印として記録します。Ctrl+C で終わります。

引数:
  --titles              窓・部品・端点の実際の名前も出す（チャンネル名・参加者名が含まれうるので、人に渡す前に確かめる）
  --output <ファイル>   書き出す先（既定は exe と同じフォルダの probe-output.txt。追記する）
  --interval <秒>       調べる間隔（既定は 2 秒）
  --uia-interval <秒>   窓の中の部品を調べる間隔（既定は 5 秒）
  --once                1 回だけ調べて終わる
  --require-sessions    --once で、録音のセッションの一覧を取れなければ終了コード 1
  --expect <アプリ>     --once で、その会議と判定しなければ終了コード 1（zoom・slack・teams・discord・meet）
  --help                この説明を出す

測り方（1 つの会議ごとに 2〜3 分、全体で 20 分ほど。各手順の区切りで、目印を打って Enter）:
 1. meeting_probe.exe を起動する（名前を見たいときだけ --titles）。
 2. Slack のハドル: ミュートのまま入る → 2 分いる → ミュートを外して話す → 退出して 1 分待つ。
    続けて、Slack の設定（環境設定 → オーディオとビデオ）のマイクのテストを 1 回。
 3. Discord の通話（デスクトップのアプリ）: 同じ流れ。
 4. Google Meet（ふだん使うブラウザ）: 会議でない別のタブを開いたまま、待機の画面 → 参加 → ミュート → 退出。
 5. Teams・Zoom: 入れる会議があれば同じ流れ（なければ省く）。
 6. 会議でないマイクの使用: ブラウザの音声入力か録音のアプリを 1 つ、30 秒ほど。
 7. Ctrl+C で終え、probe-output.txt を渡す。
詳しくは docs/meeting-probe.md。
";

struct Options {
    once: bool,
    interval: Duration,
    uia_interval: Duration,
    titles: bool,
    require_sessions: bool,
    expect: Option<MeetingApp>,
    output: Option<PathBuf>,
    help: bool,
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

fn seconds(value: Option<String>, flag: &str) -> Result<Duration, String> {
    value
        .and_then(|value| value.parse().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .ok_or(format!("{flag} には 1 以上の秒数を指定してください"))
}

fn options(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        once: false,
        interval: Duration::from_secs(2),
        uia_interval: Duration::from_secs(5),
        titles: false,
        require_sessions: false,
        expect: None,
        output: None,
        help: false,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--once" => options.once = true,
            "--titles" => options.titles = true,
            "--require-sessions" => options.require_sessions = true,
            "--help" | "-h" => options.help = true,
            "--output" => options.output = Some(args.next().ok_or("--output には書き出す先を指定してください")?.into()),
            "--uia-interval" => options.uia_interval = seconds(args.next(), "--uia-interval")?,
            "--expect" => {
                let app = args
                    .next()
                    .and_then(|value| expected_app(&value))
                    .ok_or("--expect には zoom・slack・teams・discord・meet のどれかを指定してください")?;
                options.expect = Some(app);
            }
            "--interval" => options.interval = seconds(args.next(), "--interval")?,
            other => return Err(format!("分からない引数です: {other}（使える引数は --help で確かめてください）")),
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
    let latest = filetime_text(store.entries.iter().map(|entry| entry.last_start).max().unwrap_or(0));
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

/// 画面とファイルの両方に書く。
struct Out {
    file: Option<std::fs::File>,
}

impl Out {
    fn line(&mut self, text: &str) {
        println!("{text}");
        self.to_file(text);
    }

    fn error(&mut self, text: &str) {
        eprintln!("{text}");
        self.to_file(text);
    }

    fn to_file(&mut self, text: &str) {
        if let Some(file) = &mut self.file {
            if writeln!(file, "{text}").is_err() {
                eprintln!("書き出す先に書けなくなりました。以降は画面だけに出します");
                self.file = None;
            }
        }
    }
}

const OUTPUT_FILE: &str = "probe-output.txt";

fn default_output() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.join(OUTPUT_FILE))
}

/// 標準入力の 1 行ずつを、目印として渡す（標準入力が閉じたら終わる）。
fn markers() -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lines() {
            let Ok(line) = line else { break };
            let line = line.trim().to_string();
            if !line.is_empty() && sender.send(line).is_err() {
                break;
            }
        }
    });
    receiver
}

/// 次に調べるまで待つ。その間に打たれた目印は、すぐに時刻つきで書く。
fn wait(interval: Duration, markers: Option<&mpsc::Receiver<String>>, count: &mut usize, out: &mut Out) {
    let deadline = Instant::now() + interval;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        let Some(receiver) = markers else {
            std::thread::sleep(left);
            return;
        };
        match receiver.recv_timeout(left) {
            Ok(text) => {
                *count += 1;
                out.line(&format!("[{}] ◆ 目印 {count}: {text}", chrono::Local::now().format("%H:%M:%S")));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => return,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                std::thread::sleep(left);
                return;
            }
        }
    }
}

/// 窓の中の部品の走査の、最新の結果を受け取る口。
enum UiaFeed {
    /// 別のスレッドで、一定の間隔で調べる
    Background(std::sync::Arc<std::sync::Mutex<Option<signals::UiaScan>>>),
    /// その場で 1 回だけ調べる（--once）
    Now,
}

impl UiaFeed {
    fn start(options: &Options) -> Self {
        if options.once {
            return UiaFeed::Now;
        }
        let latest = std::sync::Arc::new(std::sync::Mutex::new(None));
        #[cfg(windows)]
        {
            let shared = latest.clone();
            let interval = options.uia_interval;
            std::thread::spawn(move || {
                let _com = win::Com::init();
                let uia = win::Uia::new();
                loop {
                    let scan = win::scan_once(&uia);
                    *shared.lock().unwrap() = Some(scan);
                    std::thread::sleep(interval);
                }
            });
        }
        UiaFeed::Background(latest)
    }

    fn latest(&self) -> Option<signals::UiaScan> {
        match self {
            UiaFeed::Background(latest) => latest.lock().unwrap().clone(),
            UiaFeed::Now => {
                #[cfg(windows)]
                {
                    // UI Automation の部品は、COM を戻す（_com を捨てる）前に解放する
                    let _com = win::Com::init();
                    let uia = win::Uia::new();
                    let scan = win::scan_once(&uia);
                    drop(uia);
                    Some(scan)
                }
                #[cfg(not(windows))]
                {
                    None
                }
            }
        }
    }
}

/// FILETIME（1601-01-01 からの 100 ナノ秒単位）を、手元の時刻（分まで）にする。0 は「なし」。
fn filetime_text(filetime: u64) -> String {
    if filetime == 0 {
        return "なし".into();
    }
    chrono::DateTime::from_timestamp_millis(mic_store::filetime_to_unix_ms(filetime))
        .map(|at| at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// 会議中を示しうる手がかり（判定には使わない）の行。
fn signal_lines(titles: bool, uia: Option<&signals::UiaScan>) -> Vec<signals::Line> {
    #[cfg(windows)]
    {
        let mut lines = signals::audio_lines(&win::read_audio(signals::Flow::Capture, titles));
        lines.extend(signals::audio_lines(&win::read_audio(signals::Flow::Render, titles)));
        lines.extend(signals::window_lines(&win::list_windows(), titles));
        lines.extend(match uia {
            Some(scan) => signals::uia_lines(scan, titles),
            None => vec![signals::Line::new("部品/一覧".into(), "窓の中の部品".into(), "  窓の中の部品: 最初の走査の途中".into())],
        });
        lines.extend(signals::camera_lines(Some(&win::read_camera()), filetime_text));
        lines
    }
    #[cfg(not(windows))]
    {
        let _ = (titles, uia);
        let mut lines = vec![signals::Line::new(
            "手がかり".into(),
            "手がかり".into(),
            "  録音と再生のセッション・窓・窓の中の部品: 取れない（Windows 以外）".into(),
        )];
        lines.extend(signals::camera_lines(None, filetime_text));
        lines
    }
}

fn windows_version() -> String {
    #[cfg(windows)]
    {
        win::windows_version()
    }
    #[cfg(not(windows))]
    {
        "Windows 以外".into()
    }
}

/// 変化がない時間がこれだけ続いたら、動いていることが分かるように 1 行出す。
const QUIET_NOTICE: Duration = Duration::from_secs(30);

fn main() {
    let options = match options(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    if options.help {
        print!("{HELP}");
        return;
    }
    let path = options.output.clone().or_else(default_output);
    let file = path.as_ref().and_then(|path| {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|err| eprintln!("書き出す先を開けません（{err}）。画面だけに出します"))
            .ok()
    });
    let destination = match (&file, &options.output) {
        (None, _) => "なし（画面だけ）".to_string(),
        (Some(_), Some(path)) => path.display().to_string(),
        (Some(_), None) => format!("exe と同じフォルダの {OUTPUT_FILE}（追記）"),
    };
    let mut out = Out { file };
    out.line(&format!(
        "Kiroscribe 会議の検出の確認（meeting_probe {}）。{} 秒ごとに調べます。Ctrl+C で終わります。",
        env!("CARGO_PKG_VERSION"),
        options.interval.as_secs()
    ));
    out.line(&format!(
        "  開始 {}、{}、名前の表示 {}、窓の中の部品は {} 秒ごと",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
        windows_version(),
        if options.titles { "あり（--titles）" } else { "なし（分類だけ）" },
        options.uia_interval.as_secs()
    ));
    out.line(&format!("  書き出す先: {destination}"));
    if !options.once {
        out.line("  目印: 1 行打って Enter を押すと、時刻つきで記録します（例: discord 入った）");
    }
    let markers = (!options.once).then(markers);
    let mut marker_count = 0;
    let uia = UiaFeed::start(&options);
    let mut tracker = MeetingTracker::default();
    let mut last: Option<Vec<String>> = None;
    let mut differ = signals::Differ::default();
    let mut quiet_since = Instant::now();
    loop {
        let now = now_ms();
        let (observation, sources) = observe_detailed();
        let event = tracker.observe(&observation, None, now);
        let lines = render(&observation, sources.as_ref(), &tracker, options.titles);
        let changes = differ.changes(&signal_lines(options.titles, uia.latest().as_ref()));
        let time = chrono::Local::now().format("%H:%M:%S");
        let mut printed = false;
        match event {
            Some(MeetingEvent::Started(app)) => {
                out.line(&format!("[{time}] ★ 会議の開始を確定: {}（製品なら録音を始める）", app.label()));
                printed = true;
            }
            Some(MeetingEvent::Ended(app)) => {
                out.line(&format!("[{time}] ★ 会議の終了を確定: {}（製品なら録音を止める）", app.label()));
                printed = true;
            }
            None => {}
        }
        let block_changed = last.as_ref() != Some(&lines);
        if block_changed || !changes.is_empty() {
            out.line(&format!("[{time}]"));
            printed = true;
        }
        if block_changed {
            for line in &lines {
                out.line(line);
            }
            last = Some(lines);
        }
        if !changes.is_empty() {
            out.line("  -- 会議中を示しうる手がかり（判定には使わない。変化した行だけ）");
            for line in &changes {
                out.line(line);
            }
        }
        if printed {
            quiet_since = Instant::now();
        } else if quiet_since.elapsed() >= QUIET_NOTICE {
            out.line(&format!("[{time}] 変化なし（{} 秒）", QUIET_NOTICE.as_secs()));
            quiet_since = Instant::now();
        }
        if options.once {
            if let Some(message) = check(&options, &observation, sources.as_ref()) {
                out.error(&format!("確認に失敗しました: {message}"));
                std::process::exit(1);
            }
            return;
        }
        wait(options.interval, markers.as_ref(), &mut marker_count, &mut out);
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
        let options = options(["--once", "--require-sessions", "--expect", "zoom"].map(String::from)).unwrap();
        assert_eq!(options.expect, Some(MeetingApp::Zoom));
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

    #[test]
    fn options_for_the_owner_run() {
        let parsed = options(["--titles", "--output", "out.txt", "--uia-interval", "10"].map(String::from)).unwrap();
        assert!(parsed.titles && !parsed.once);
        assert_eq!(parsed.output, Some(PathBuf::from("out.txt")));
        assert_eq!(parsed.uia_interval, Duration::from_secs(10));
        assert_eq!(parsed.interval, Duration::from_secs(2));
        let defaults = options(Vec::<String>::new()).unwrap();
        assert!(!defaults.titles && defaults.output.is_none());
        assert_eq!(defaults.uia_interval, Duration::from_secs(5));
        assert!(options(["--help"].map(String::from)).unwrap().help);
        assert!(options(["--uia-interval", "0"].map(String::from)).is_err());
        assert!(options(["--output"].map(String::from)).is_err());
        assert!(options(["--expect", "zoom"].map(String::from)).is_err(), "--expect は --once と組み合わせる");
    }

    #[test]
    fn help_covers_every_meeting_in_the_owner_procedure() {
        for word in ["Slack のハドル", "マイクのテスト", "Discord", "Google Meet", "Teams・Zoom", "会議でないマイクの使用", "目印", "probe-output.txt", "--titles"] {
            assert!(HELP.contains(word), "{word}");
        }
    }
}
