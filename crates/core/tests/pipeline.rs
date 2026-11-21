//! 録音パイプライン全体の結合テスト:
//! 音声 → 処理スレッド（16kHz 化・区切り・WAV 化）→ 文字起こしワーカー → OpenAI 互換の HTTP サーバー → 結果

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kiroscribe_core::recorder::{
    run_transcription_worker, spawn_source_thread, AudioFrame, RecorderSink, SourceProcessor,
};
use kiroscribe_core::retry::RetryPolicy;
use kiroscribe_core::transcribe::OpenAiTranscriber;
use kiroscribe_core::transcript::Speaker;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const RATE: u32 = 44_100;
const T0: i64 = 1_800_000_000_000;

#[derive(Default)]
struct Collect {
    transcripts: Mutex<Vec<(Speaker, i64, i64, String)>>,
    errors: Mutex<Vec<String>>,
}

impl RecorderSink for Collect {
    fn on_transcript(&self, speaker: Speaker, started_at: i64, ended_at: i64, text: String) {
        self.transcripts
            .lock()
            .unwrap()
            .push((speaker, started_at, ended_at, text));
    }
    fn on_level(&self, _: Speaker, _: f32) {}
    fn on_error(&self, message: String) {
        self.errors.lock().unwrap().push(message);
    }
}

/// 1 回目は 500 を返し（再試行させる）、以降は「文N」を返すサーバー
struct Flaky(AtomicUsize);

impl Respond for Flaky {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let n = self.0.fetch_add(1, Ordering::SeqCst);
        assert!(String::from_utf8_lossy(&request.body).contains("RIFF"));
        if n == 0 {
            return ResponseTemplate::new(500).set_body_string("temporary");
        }
        ResponseTemplate::new(200).set_body_json(serde_json::json!({ "text": format!("文{n}。") }))
    }
}

fn speechlike(seconds: f32) -> Vec<f32> {
    (0..(RATE as f32 * seconds) as usize)
        .map(|i| {
            let s = 0.25 * (2.0 * std::f32::consts::PI * 180.0 * i as f32 / RATE as f32).sin();
            if (i as u64 * 1000 / RATE as u64) % 300 < 200 { s } else { s * 0.02 }
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn audio_flows_through_to_transcripts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/audio/transcriptions"))
        .respond_with(Flaky(AtomicUsize::new(0)))
        .mount(&server)
        .await;

    let clock_ms = Arc::new(AtomicI64::new(T0));
    let sink = Arc::new(Collect::default());
    let (job_tx, job_rx) = tokio::sync::mpsc::unbounded_channel();
    let processor = SourceProcessor::new(
        Speaker::Them,
        RATE,
        Arc::new(AtomicBool::new(false)),
        job_tx,
        sink.clone(),
    );
    let (frame_tx, frame_rx) = std::sync::mpsc::channel();
    let thread = spawn_source_thread(processor, frame_rx);

    let transcriber = Arc::new(
        OpenAiTranscriber::new("sk-test", "gpt-4o-transcribe", "ja").with_base_url(&server.uri()),
    );
    let policy = RetryPolicy {
        max_attempts: 3,
        base_delay: std::time::Duration::from_millis(5),
        max_delay: std::time::Duration::from_millis(10),
    };
    let worker = tokio::spawn(run_transcription_worker(job_rx, transcriber, sink.clone(), policy));

    // 実際のデバイスと同じく 10ms ごとに届く音声: 無音1s・発話2s・無音1.5s・発話1.5s
    let mut audio = vec![0.0; RATE as usize];
    audio.extend(speechlike(2.0));
    audio.extend(vec![0.0; (RATE as f32 * 1.5) as usize]);
    audio.extend(speechlike(1.5));
    // 処理より速く送る（処理側が遅れても時刻は取り込み時刻で決まることも確かめる）
    for block in audio.chunks(RATE as usize / 100) {
        let captured_at = clock_ms.fetch_add(10, Ordering::SeqCst) + 10;
        frame_tx
            .send(AudioFrame {
                captured_at,
                samples: block.to_vec(),
            })
            .unwrap();
    }
    drop(frame_tx); // 録音停止
    tokio::task::spawn_blocking(move || thread.join().unwrap())
        .await
        .unwrap();
    worker.await.unwrap();

    let transcripts = sink.transcripts.lock().unwrap().clone();
    assert!(sink.errors.lock().unwrap().is_empty(), "{:?}", sink.errors.lock().unwrap());
    assert_eq!(transcripts.len(), 2, "{transcripts:?}");
    // 1 件目は 500 のあと再試行で成功している
    assert_eq!(transcripts[0].3, "文1。");
    assert_eq!(transcripts[1].3, "文2。");
    assert!(transcripts.iter().all(|t| t.0 == Speaker::Them));
    // 実時間に沿った時刻（2 件目は約 4.5 秒後に始まる。語頭の余白 0.3 秒を含む）
    assert!((transcripts[0].1 - (T0 + 700)).abs() < 150, "{}", transcripts[0].1 - T0);
    assert!((transcripts[1].1 - (T0 + 4_200)).abs() < 150, "{}", transcripts[1].1 - T0);
    assert!(transcripts[0].2 < transcripts[1].1);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3, "再試行 1 回を含む");
}
