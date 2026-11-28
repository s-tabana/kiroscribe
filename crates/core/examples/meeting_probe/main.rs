//! 会議の検出の確認用の道具: 製品と同じ部品（`kiroscribe_core::meetings::MeetingDetector`）で、同じ間隔（2 秒）・
//! 同じ判定表で判定し、判定の様子と、読んだ PC の様子（対象のアプリの窓・窓の中の部品・録音と再生のセッション）を、
//! 変化があった行だけ出す。
//!
//! ```text
//! cargo build -p kiroscribe-core --release --example meeting_probe
//! ```
//!
//! 出すのは、実行ファイルの名前・パッケージファミリー名・状態・音の大きさの数値・判定表の印の名前と、判定の結果だけ。
//! 窓の題と部品の名前は `--titles` のときだけ出す。フルパス・音声は出さない。出力は、画面とファイル
//! （既定は exe と同じ場所の probe-output.txt）の両方に書く。引数は `--help`（[`HELP`]）、測り方は docs/meeting-detection-check.md。

mod lines;

use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use kiroscribe_core::audio_sessions::Flow;
use kiroscribe_core::meetings::{Browsers, MeetingApp, MeetingDetector, Step};
use kiroscribe_core::time::now_ms;

const HELP: &str = "\
Kiroscribe 会議の検出の確認用の道具（meeting_probe）

使い方: meeting_probe.exe [引数]

製品（Kiroscribe）と同じ判定表で、2 秒ごとに会議の判定をし、変化があった行だけを出します。
出力は画面と probe-output.txt（exe と同じフォルダ）に書きます。
- 段階（会議なし・開始の確定待ち・会議中・終了の猶予の間）と、ほかに続いている会議
- サービスごとの判定（待機・確定待ち・参加中・猶予の間）と、立っている印、その文脈
- 会議の開始・終了を確定したときは、その理由（立っていた印・猶予の経過・打ち消しの印・別の会議への切り替え）
- 対象のアプリ（会議アプリとブラウザ）の窓（クラス名・見えているか・立っている印）と、窓の中の部品
- 録音と再生のオーディオのセッション（対象のアプリのもの。再生は音の大きさ）
Enter で区切った 1 行（例: discord 入った）は、時刻つきの目印として記録します。Ctrl+C で終わります。

引数:
  --titles              窓の題と部品の名前も出す（チャンネル名・参加者名が含まれうるので、人に渡す前に確かめる）
  --output <ファイル>   書き出す先（既定は exe と同じフォルダの probe-output.txt。追記する）
  --interval <秒>       調べる間隔（既定は 2 秒。製品と同じ）
  --once                1 回だけ調べて終わる
  --require-sessions    --once で、録音のセッションの一覧を取れなければ終了コード 1
  --expect <アプリ>     --once で、その会議の開始を確定しなければ終了コード 1（zoom・slack・teams・discord・meet）
  --help                この説明を出す

測り方は docs/meeting-detection-check.md（サービスごとの操作と、期待する判定）。
";

struct Options {
    once: bool,
    interval: Duration,
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

fn options(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        once: false,
        interval: Duration::from_secs(2),
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
            "--expect" => {
                let app = args
                    .next()
                    .and_then(|value| expected_app(&value))
                    .ok_or("--expect には zoom・slack・teams・discord・meet のどれかを指定してください")?;
                options.expect = Some(app);
            }
            "--interval" => {
                options.interval = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .filter(|seconds| *seconds > 0)
                    .map(Duration::from_secs)
                    .ok_or("--interval には 1 以上の秒数を指定してください")?;
            }
            other => return Err(format!("分からない引数です: {other}（使える引数は --help で確かめてください）")),
        }
    }
    if !options.once && (options.require_sessions || options.expect.is_some()) {
        return Err("--require-sessions と --expect は --once と組み合わせてください".into());
    }
    Ok(options)
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
fn wait(interval: Duration, markers: &mpsc::Receiver<String>, count: &mut usize, out: &mut Out) {
    let deadline = Instant::now() + interval;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        match markers.recv_timeout(left) {
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

/// 1 回分の行（判定・セッション・窓）。
fn step_lines(detector: &MeetingDetector, step: &Step, titles: bool) -> Vec<lines::Line> {
    let rules = detector.rules();
    let browsers = detector.browsers();
    let mut all = lines::judge_lines(&detector.judge);
    all.extend(lines::session_lines(Flow::Capture, &step.facts.capture, rules, browsers));
    all.extend(lines::session_lines(Flow::Render, &step.facts.render, rules, browsers));
    all.extend(lines::window_lines(&step.facts, &step.evaluation, titles));
    all.extend(lines::scan_line(&step.facts));
    all
}

/// `--require-sessions`・`--expect` の確認。満たさないなら、その理由。
fn check(options: &Options, detector: &MeetingDetector, step: &Step) -> Option<String> {
    if options.require_sessions && !step.facts.capture.available {
        return Some(format!(
            "録音のオーディオのセッションの一覧を取れない（{}）",
            step.facts.capture.error.as_deref().unwrap_or("Windows 以外")
        ));
    }
    let expected = options.expect?;
    let primary = detector.judge.primary();
    (primary != Some(expected)).then(|| {
        format!("{} の会議の開始を確定していない（いまの会議: {}）", expected.label(), primary.map(|app| app.label()).unwrap_or("なし"))
    })
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
    let opened = path.as_ref().map(|path| std::fs::OpenOptions::new().create(true).append(true).open(path));
    let file = match opened {
        Some(Ok(file)) => Some(file),
        Some(Err(err)) => {
            let shown = path.as_ref().map(|path| path.display().to_string()).unwrap_or_default();
            eprintln!("書き出す先を開けません（{shown}）: {err}");
            // 指定した書き出す先に書けないなら、提出する記録が残らないので、続けない
            if options.output.is_some() {
                std::process::exit(1);
            }
            None
        }
        None => None,
    };
    let written = file.is_some();
    let mut out = Out { file };
    out.line(&format!(
        "=== meeting_probe {}（{}）{} ===",
        env!("CARGO_PKG_VERSION"),
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
        if options.titles { " 題と部品の名前も出す" } else { "" }
    ));
    // フルパスには利用者のフォルダの名前が入るので、人に渡すファイルには書かず、画面にだけ出す
    match path.as_ref().filter(|_| written) {
        Some(path) => println!("  書き出す先: {}", path.display()),
        None => println!("  書き出す先: なし（画面だけ）"),
    }
    let mut detector = MeetingDetector::new(Browsers::builtin());
    out.line(&format!("  判定表の版: {}", detector.rules().version));
    let markers = (!options.once).then(markers);
    let mut marker_count = 0;
    let mut differ = lines::Differ::default();
    let mut quiet_since = Instant::now();
    loop {
        let step = detector.step(now_ms());
        let time = chrono::Local::now().format("%H:%M:%S");
        let mut printed = false;
        for decision in &step.decisions {
            out.line(&format!("[{time}] {}", lines::decision_line(decision)));
            printed = true;
        }
        let changes = differ.changes(&step_lines(&detector, &step, options.titles));
        if !changes.is_empty() {
            out.line(&format!("[{time}]"));
            for line in &changes {
                out.line(line);
            }
            printed = true;
        }
        if printed {
            quiet_since = Instant::now();
        } else if quiet_since.elapsed() >= QUIET_NOTICE {
            out.line(&format!("[{time}] 変化なし（{} 秒）", QUIET_NOTICE.as_secs()));
            quiet_since = Instant::now();
        }
        if options.once {
            if let Some(message) = check(&options, &detector, &step) {
                out.error(&format!("確認に失敗しました: {message}"));
                std::process::exit(1);
            }
            return;
        }
        if let Some(markers) = &markers {
            wait(options.interval, markers, &mut marker_count, &mut out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiroscribe_core::audio_sessions::{summarize, AudioSessions};
    use kiroscribe_core::meeting_facts::{Facts, WindowFact};
    use kiroscribe_core::meeting_rules::evaluate;

    /// --once で、録音のセッションの一覧と、いまの会議を確かめられる。
    // [MTG-020]
    #[test]
    fn once_can_require_the_session_list_and_the_meeting() {
        let options = options(["--once", "--require-sessions", "--expect", "zoom"].map(String::from)).unwrap();
        assert_eq!(options.expect, Some(MeetingApp::Zoom));
        let mut detector = MeetingDetector::new(Browsers::builtin());
        let zoom = Facts {
            windows: vec![WindowFact { hwnd: 1, exe: "zoom.exe".into(), class: "ZPContentViewWndClass".into(), title: "Zoom".into(), visible: true }],
            capture: summarize(vec![]),
            ..Default::default()
        };
        let evaluation = evaluate(detector.rules(), &zoom, detector.browsers());
        let empty = Step { facts: Facts { capture: summarize(vec![]), ..Default::default() }, evaluation: Default::default(), decisions: vec![] };
        assert!(check(&options, &detector, &empty).unwrap().contains("Zoom の会議の開始を確定していない"));
        let decisions = detector.judge.tick(&evaluation, 0);
        let step = Step { facts: zoom, evaluation, decisions };
        assert!(check(&options, &detector, &step).is_none());
        let failed = Step { facts: Facts { capture: AudioSessions { error: Some("0x80070005".into()), ..Default::default() }, ..Default::default() }, ..step };
        assert!(check(&options, &detector, &failed).unwrap().contains("0x80070005"));
    }

    #[test]
    fn options_for_the_owner_run() {
        let parsed = options(["--titles", "--output", "out.txt"].map(String::from)).unwrap();
        assert!(parsed.titles && !parsed.once);
        assert_eq!(parsed.output, Some(PathBuf::from("out.txt")));
        assert_eq!(parsed.interval, Duration::from_secs(2));
        assert!(options(["--help"].map(String::from)).unwrap().help);
        assert!(options(["--interval", "0"].map(String::from)).is_err());
        assert!(options(["--output"].map(String::from)).is_err());
        assert!(options(["--expect", "zoom"].map(String::from)).is_err(), "--expect は --once と組み合わせる");
        assert!(options(["--rules", "x.toml"].map(String::from)).is_err(), "判定表は製品に組み込み");
    }

    #[test]
    fn help_points_to_the_check_procedure() {
        for word in ["判定表", "目印", "probe-output.txt", "--titles", "meeting-detection-check.md"] {
            assert!(HELP.contains(word), "{word}");
        }
    }
}
