//! 録音デバイスの動作確認ツール（API には送らない）。
//!
//! ```text
//! cargo run -p kiroscribe-core --example record_probe -- [秒数] [--no-system]
//! ```
//!
//! マイク一覧を表示し、指定秒数だけ録音して、入力レベルと「API に送るはずだった発話」を表示する。

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kiroscribe_core::audio::capture::list_input_devices;
use kiroscribe_core::error::Result;
use kiroscribe_core::recorder::RecorderSink;
use kiroscribe_core::session::{RecordingSession, SessionOptions};
use kiroscribe_core::transcribe::{BoxFuture, Transcribe};
use kiroscribe_core::transcript::Speaker;

struct PrintSink {
    peak: Mutex<[f32; 2]>,
}

impl RecorderSink for PrintSink {
    fn on_transcript(&self, speaker: Speaker, started_at: i64, ended_at: i64, text: String) {
        println!(
            "[{}] {:.1}s の発話 → {text}",
            speaker.label(),
            (ended_at - started_at) as f64 / 1000.0
        );
    }
    fn on_level(&self, speaker: Speaker, level: f32) {
        let mut peak = self.peak.lock().unwrap();
        let slot = &mut peak[speaker as usize];
        *slot = slot.max(level);
    }
    fn on_error(&self, message: String) {
        println!("[警告] {message}");
    }
}

/// API を呼ばずに WAV の長さだけ返す
struct DryRun;

impl Transcribe for DryRun {
    fn transcribe(&self, wav: Vec<u8>, _prompt: Option<String>) -> BoxFuture<'_, Result<String>> {
        Box::pin(async move { Ok(format!("（WAV {} KB）", wav.len() / 1024)) })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let seconds: u64 = args.iter().find_map(|a| a.parse().ok()).unwrap_or(10);
    let system = !args.iter().any(|a| a == "--no-system");

    println!("== マイク ==");
    for device in list_input_devices()? {
        println!("{} {}  ({})", if device.is_default { "*" } else { " " }, device.name, device.id);
    }

    let sink = Arc::new(PrintSink { peak: Mutex::new([0.0; 2]) });
    let session = RecordingSession::start(
        SessionOptions {
            mic_device_id: None,
            capture_system_audio: system,
        },
        Arc::new(DryRun),
        sink.clone(),
        &tokio::runtime::Handle::current(),
    )?;
    println!("== {seconds} 秒録音します（取り込み: {:?}）==", session.sources());
    for _ in 0..seconds {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut peak = sink.peak.lock().unwrap();
        println!("  レベル 自分 {:.3} / 相手 {:.3}", peak[0], peak[1]);
        *peak = [0.0; 2];
    }
    let started = Instant::now();
    session.stop().await;
    println!("== 停止しました（{} ms）==", started.elapsed().as_millis());
    Ok(())
}
