//! 録音から文字起こしまでのパイプライン（音声デバイスに依存しない部分）。
//!
//! ```text
//! 取り込み（音源ごと）─▶ SourceProcessor（16kHz 化・発話で区切る・WAV 化）
//!                         ─▶ 文字起こしワーカー（API 呼び出し・再試行）─▶ RecorderSink（保存・画面通知）
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc;

use crate::audio::chunker::{Chunk, Chunker, ChunkerConfig};
use crate::audio::dsp::{rms, Resampler};
use crate::audio::wav::encode_wav_mono_i16;
use crate::audio::TARGET_SAMPLE_RATE;
use crate::error::CoreError;
use crate::retry::{retry_async, RetryPolicy};
use crate::transcribe::{clean_transcript, context_tail, Transcribe};
use crate::transcript::Speaker;

/// 録音の結果を受け取る側（アプリでは DB 保存と画面への通知）。
pub trait RecorderSink: Send + Sync {
    fn on_transcript(&self, speaker: Speaker, started_at: i64, ended_at: i64, text: String);
    /// 入力レベル（0.0〜1.0 目安）。約 0.1 秒ごと。
    fn on_level(&self, speaker: Speaker, level: f32);
    /// 録音は続けられるが知らせるべき問題。
    fn on_error(&self, message: String);
}

/// 取り込んだ音声のひとかたまり。`captured_at` は取り込み時点の時刻（UNIX ミリ秒、かたまりの末尾）。
/// 処理が遅れても時刻がずれないよう、時刻は処理側ではなく取り込み側で付ける。
#[derive(Debug, Clone)]
pub struct AudioFrame {
    pub captured_at: i64,
    pub samples: Vec<f32>,
}

#[derive(Debug)]
pub struct TranscribeJob {
    pub speaker: Speaker,
    pub started_at: i64,
    pub ended_at: i64,
    pub wav: Vec<u8>,
}

/// 実時間より音声が遅れたとみなす差。これを超えたら無音を補う。
const GAP_TOLERANCE_MS: i64 = 500;
const LEVEL_INTERVAL_SAMPLES: usize = (TARGET_SAMPLE_RATE / 10) as usize;

/// 1 つの音源（マイクまたはシステム音声）の加工を担う。
pub struct SourceProcessor {
    speaker: Speaker,
    input_rate: u32,
    resampler: Resampler,
    chunker: Chunker,
    /// 区切りの時刻の基準（開始・再開後に最初に届いた音声の先頭時刻）。未確定なら None
    base_ms: Option<i64>,
    /// 基準時刻以降に受け取った入力サンプル数
    input_samples: u64,
    paused: Arc<AtomicBool>,
    was_paused: bool,
    jobs: mpsc::UnboundedSender<TranscribeJob>,
    sink: Arc<dyn RecorderSink>,
    level_sum: f32,
    level_count: usize,
}

impl SourceProcessor {
    pub fn new(
        speaker: Speaker,
        input_rate: u32,
        paused: Arc<AtomicBool>,
        jobs: mpsc::UnboundedSender<TranscribeJob>,
        sink: Arc<dyn RecorderSink>,
    ) -> Self {
        Self {
            speaker,
            input_rate,
            resampler: Resampler::new(input_rate, TARGET_SAMPLE_RATE),
            chunker: Chunker::new(ChunkerConfig::default()),
            base_ms: None,
            input_samples: 0,
            was_paused: paused.load(Ordering::SeqCst),
            paused,
            jobs,
            sink,
            level_sum: 0.0,
            level_count: 0,
        }
    }

    /// 取り込んだモノラル音声（入力のサンプルレートのまま）を処理する。
    pub fn process(&mut self, frame: &AudioFrame) {
        if self.paused.load(Ordering::SeqCst) {
            if !self.was_paused {
                // 一時停止した時点までの発話は確定させる。一時停止中の音声は捨てる
                self.flush();
                self.was_paused = true;
                self.sink.on_level(self.speaker, 0.0);
            }
            return;
        }
        if self.was_paused {
            self.resampler.reset();
            self.chunker.reset();
            self.base_ms = None;
            self.was_paused = false;
        }

        let samples = &frame.samples;
        let frame_ms = self.samples_to_ms(samples.len() as u64);
        let base = *self.base_ms.get_or_insert_with(|| {
            self.input_samples = 0;
            frame.captured_at - frame_ms
        });

        // 音声が届かなかった時間（ループバックは無音のあいだデータが来ない）を無音で埋め、
        // 発話の時刻が実時間とずれないようにする
        let audio_end_ms = base + self.samples_to_ms(self.input_samples) + frame_ms;
        let gap = frame.captured_at - audio_end_ms;
        if gap > GAP_TOLERANCE_MS {
            let missing = (gap as u64 * self.input_rate as u64 / 1000) as usize;
            self.feed(&vec![0.0; missing]);
        }
        self.feed(samples);
    }

    fn feed(&mut self, samples: &[f32]) {
        self.input_samples += samples.len() as u64;
        let resampled = self.resampler.process(samples);
        self.update_level(&resampled);
        for chunk in self.chunker.push(&resampled) {
            self.send(chunk);
        }
    }

    fn samples_to_ms(&self, samples: u64) -> i64 {
        (samples * 1000 / self.input_rate as u64) as i64
    }

    fn update_level(&mut self, samples: &[f32]) {
        for chunk in samples.chunks(LEVEL_INTERVAL_SAMPLES) {
            self.level_sum += rms(chunk).powi(2) * chunk.len() as f32;
            self.level_count += chunk.len();
            if self.level_count >= LEVEL_INTERVAL_SAMPLES {
                let level = (self.level_sum / self.level_count as f32).sqrt();
                self.sink.on_level(self.speaker, level);
                self.level_sum = 0.0;
                self.level_count = 0;
            }
        }
    }

    fn flush(&mut self) {
        if let Some(chunk) = self.chunker.flush() {
            self.send(chunk);
        }
    }

    fn send(&self, chunk: Chunk) {
        let Some(base) = self.base_ms else { return };
        let to_ms = |sample: u64| base + (sample * 1000 / TARGET_SAMPLE_RATE as u64) as i64;
        let job = TranscribeJob {
            speaker: self.speaker,
            started_at: to_ms(chunk.start_sample),
            ended_at: to_ms(chunk.end_sample()),
            wav: encode_wav_mono_i16(&chunk.samples, TARGET_SAMPLE_RATE),
        };
        // 受け手が先に終わっている（停止処理中）なら捨ててよい
        let _ = self.jobs.send(job);
    }

    /// 取り込み終了時に、途中の発話を確定させる。
    pub fn finish(&mut self) {
        if !self.was_paused {
            self.flush();
        }
        self.sink.on_level(self.speaker, 0.0);
    }
}

/// 取り込みスレッドから届く音声を処理する専用スレッドを起動する。送信側が閉じたら終了する。
pub fn spawn_source_thread(
    mut processor: SourceProcessor,
    frames: std::sync::mpsc::Receiver<AudioFrame>,
) -> std::thread::JoinHandle<()> {
    let name = format!("kiroscribe-audio-{}", processor.speaker.as_str());
    std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            while let Ok(frame) = frames.recv() {
                processor.process(&frame);
            }
            processor.finish();
        })
        .expect("音声処理スレッドを起動できませんでした")
}

fn clock_label(ms: i64) -> String {
    use chrono::{Local, TimeZone};
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_default()
}

/// 1 音源ぶんの文字起こしを順番に処理する。ジョブの送信側がすべて閉じたら終了する。
pub async fn run_transcription_worker(
    mut jobs: mpsc::UnboundedReceiver<TranscribeJob>,
    transcriber: Arc<dyn Transcribe>,
    sink: Arc<dyn RecorderSink>,
    policy: RetryPolicy,
) {
    let mut context = String::new();
    let mut auth_failed = false;
    while let Some(job) = jobs.recv().await {
        if auth_failed {
            continue;
        }
        let prompt = (!context.is_empty()).then(|| context.clone());
        let result = retry_async(
            &policy,
            || transcriber.transcribe(job.wav.clone(), prompt.clone()),
            |_, _| {},
        )
        .await;
        match result {
            Ok(text) => {
                if let Some(text) = clean_transcript(&text) {
                    context = context_tail(&format!("{context}{text}"), 200);
                    sink.on_transcript(job.speaker, job.started_at, job.ended_at, text);
                }
            }
            Err(err @ CoreError::Api { status: 401 | 403, .. }) => {
                // キーが無効なら以降も失敗するので、一度だけ知らせて残りは送らない
                auth_failed = true;
                sink.on_error(format!(
                    "文字起こしを停止しました。API キーを確認してください（{err}）"
                ));
            }
            Err(err) => sink.on_error(format!(
                "{}の発話（{}〜）を文字起こしできませんでした: {err}",
                job.speaker.label(),
                clock_label(job.started_at)
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI64;
    use std::sync::Mutex;
    use std::time::Duration;

    use super::*;
    use crate::error::Result;
    use crate::transcribe::BoxFuture;

    #[derive(Default)]
    struct TestSink {
        transcripts: Mutex<Vec<(Speaker, i64, i64, String)>>,
        levels: Mutex<Vec<(Speaker, f32)>>,
        errors: Mutex<Vec<String>>,
    }

    impl RecorderSink for TestSink {
        fn on_transcript(&self, speaker: Speaker, started_at: i64, ended_at: i64, text: String) {
            self.transcripts
                .lock()
                .unwrap()
                .push((speaker, started_at, ended_at, text));
        }
        fn on_level(&self, speaker: Speaker, level: f32) {
            self.levels.lock().unwrap().push((speaker, level));
        }
        fn on_error(&self, message: String) {
            self.errors.lock().unwrap().push(message);
        }
    }

    /// 呼ばれた順に「発話1」「発話2」…を返す偽の文字起こし。
    #[derive(Default)]
    struct FakeTranscriber {
        calls: Mutex<Vec<Option<String>>>,
        fail_status: Option<u16>,
    }

    impl Transcribe for FakeTranscriber {
        fn transcribe(&self, wav: Vec<u8>, prompt: Option<String>) -> BoxFuture<'_, Result<String>> {
            Box::pin(async move {
                assert_eq!(&wav[0..4], b"RIFF");
                let mut calls = self.calls.lock().unwrap();
                calls.push(prompt);
                match self.fail_status {
                    Some(status) => Err(CoreError::Api {
                        service: "test",
                        status,
                        message: "fail".into(),
                    }),
                    None => Ok(format!("発話{}。", calls.len())),
                }
            })
        }
    }

    const RATE: u32 = 48_000;
    const T0: i64 = 1_700_000_000_000;

    /// 話し声に近い信号（200ms 発声 + 100ms 弱音の繰り返し）。一定音は背景雑音とみなされるため
    fn tone(seconds: f32) -> Vec<f32> {
        (0..(RATE as f32 * seconds) as usize)
            .map(|i| {
                let s = 0.3 * (2.0 * std::f32::consts::PI * 200.0 * i as f32 / RATE as f32).sin();
                if (i as u64 * 1000 / RATE as u64) % 300 < 200 { s } else { s * 0.02 }
            })
            .collect()
    }

    fn silence(seconds: f32) -> Vec<f32> {
        vec![0.0; (RATE as f32 * seconds) as usize]
    }

    struct Harness {
        clock: Arc<AtomicI64>,
        paused: Arc<AtomicBool>,
        sink: Arc<TestSink>,
        processor: SourceProcessor,
        jobs: mpsc::UnboundedReceiver<TranscribeJob>,
    }

    fn harness(speaker: Speaker) -> Harness {
        let clock = Arc::new(AtomicI64::new(T0));
        let paused = Arc::new(AtomicBool::new(false));
        let sink = Arc::new(TestSink::default());
        let (tx, rx) = mpsc::unbounded_channel();
        let processor = SourceProcessor::new(speaker, RATE, paused.clone(), tx, sink.clone());
        Harness {
            clock,
            paused,
            sink,
            processor,
            jobs: rx,
        }
    }

    impl Harness {
        /// 10ms ずつ実時間を進めながら、取り込み時刻付きで音声を入れる（実際の取り込みと同じ）
        fn play(&mut self, audio: &[f32]) {
            for block in audio.chunks((RATE / 100) as usize) {
                let captured_at = self.clock.fetch_add(10, Ordering::SeqCst) + 10;
                self.processor.process(&AudioFrame {
                    captured_at,
                    samples: block.to_vec(),
                });
            }
        }

        fn drain_jobs(&mut self) -> Vec<TranscribeJob> {
            let mut jobs = Vec::new();
            while let Ok(job) = self.jobs.try_recv() {
                jobs.push(job);
            }
            jobs
        }
    }

    #[test]
    fn utterances_become_jobs_with_wall_clock_times() {
        let mut h = harness(Speaker::Me);
        h.play(&silence(1.0));
        h.play(&tone(1.5)); // 1.0s〜2.5s
        h.play(&silence(1.5));
        let jobs = h.drain_jobs();
        assert_eq!(jobs.len(), 1);
        let job = &jobs[0];
        assert_eq!(job.speaker, Speaker::Me);
        assert!((job.started_at - (T0 + 700)).abs() <= 60, "{}", job.started_at - T0);
        assert!((job.ended_at - (T0 + 2_700)).abs() <= 80, "{}", job.ended_at - T0);
        // 16kHz・16bit の WAV
        assert_eq!(job.wav.len() as i64, 44 + (job.ended_at - job.started_at) * 32);
        // 入力レベルが通知されている
        assert!(h.sink.levels.lock().unwrap().iter().any(|(_, l)| *l > 0.1));
    }

    #[test]
    fn gaps_without_audio_are_filled_so_times_stay_real() {
        // ループバックは相手が無音のあいだデータが来ない。5 秒空いても時刻がずれないこと
        let mut h = harness(Speaker::Them);
        h.play(&tone(1.0));
        h.play(&silence(1.0));
        h.clock.fetch_add(5_000, Ordering::SeqCst);
        h.play(&tone(1.0)); // 実時間で 7.0s〜8.0s
        h.play(&silence(1.5));
        let jobs = h.drain_jobs();
        assert_eq!(jobs.len(), 2);
        assert!((jobs[1].started_at - (T0 + 6_700)).abs() <= 120, "{}", jobs[1].started_at - T0);
    }

    #[test]
    fn paused_audio_is_dropped_and_clock_restarts_on_resume() {
        let mut h = harness(Speaker::Me);
        h.play(&tone(1.0));
        h.paused.store(true, Ordering::SeqCst);
        h.play(&tone(3.0)); // 一時停止中の発話は送られない
        let before_resume = h.drain_jobs();
        assert_eq!(before_resume.len(), 1, "一時停止の時点で途中の発話が確定する");

        h.clock.fetch_add(10_000, Ordering::SeqCst);
        h.paused.store(false, Ordering::SeqCst);
        let resumed_at = h.clock.load(Ordering::SeqCst);
        // 一時停止前の音声は一切使われない
        h.play(&tone(1.0));
        h.play(&silence(1.5));
        let jobs = h.drain_jobs();
        assert_eq!(jobs.len(), 1);
        assert!((jobs[0].started_at - resumed_at).abs() <= 60);
    }

    #[test]
    fn processing_backlog_does_not_shift_times() {
        // 処理スレッドが遅れて、後からまとめて処理しても、時刻は取り込み時刻どおり
        let mut h = harness(Speaker::Me);
        let mut frames = Vec::new();
        let mut audio = silence(1.0);
        audio.extend(tone(1.5));
        audio.extend(silence(1.5));
        for block in audio.chunks((RATE / 100) as usize) {
            let captured_at = h.clock.fetch_add(10, Ordering::SeqCst) + 10;
            frames.push(AudioFrame { captured_at, samples: block.to_vec() });
        }
        for frame in &frames {
            h.processor.process(frame);
        }
        let jobs = h.drain_jobs();
        assert_eq!(jobs.len(), 1);
        assert!((jobs[0].started_at - (T0 + 700)).abs() <= 60, "{}", jobs[0].started_at - T0);
    }

    #[test]
    fn finish_flushes_speech_in_progress() {
        let mut h = harness(Speaker::Me);
        h.play(&tone(2.0));
        assert!(h.drain_jobs().is_empty());
        h.processor.finish();
        assert_eq!(h.drain_jobs().len(), 1);
    }

    fn fast_policy() -> RetryPolicy {
        RetryPolicy {
            max_attempts: 2,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(1),
        }
    }

    fn job(speaker: Speaker, started_at: i64) -> TranscribeJob {
        TranscribeJob {
            speaker,
            started_at,
            ended_at: started_at + 1_000,
            wav: encode_wav_mono_i16(&[0.0; 16], TARGET_SAMPLE_RATE),
        }
    }

    #[tokio::test]
    async fn worker_transcribes_in_order_with_context() {
        let (tx, rx) = mpsc::unbounded_channel();
        let transcriber = Arc::new(FakeTranscriber::default());
        let sink = Arc::new(TestSink::default());
        tx.send(job(Speaker::Them, T0)).unwrap();
        tx.send(job(Speaker::Them, T0 + 2_000)).unwrap();
        drop(tx);
        run_transcription_worker(rx, transcriber.clone(), sink.clone(), fast_policy()).await;

        let transcripts = sink.transcripts.lock().unwrap();
        assert_eq!(
            *transcripts,
            vec![
                (Speaker::Them, T0, T0 + 1_000, "発話1。".to_string()),
                (Speaker::Them, T0 + 2_000, T0 + 3_000, "発話2。".to_string()),
            ]
        );
        // 2 回目には直前の発話が文脈として渡る
        assert_eq!(
            *transcriber.calls.lock().unwrap(),
            vec![None, Some("発話1。".to_string())]
        );
    }

    #[tokio::test]
    async fn worker_reports_failures_and_stops_on_auth_error() {
        let (tx, rx) = mpsc::unbounded_channel();
        let transcriber = Arc::new(FakeTranscriber {
            fail_status: Some(401),
            ..Default::default()
        });
        let sink = Arc::new(TestSink::default());
        for i in 0..3 {
            tx.send(job(Speaker::Me, T0 + i * 2_000)).unwrap();
        }
        drop(tx);
        run_transcription_worker(rx, transcriber.clone(), sink.clone(), fast_policy()).await;
        assert_eq!(transcriber.calls.lock().unwrap().len(), 1, "401 は再試行せず、以降も送らない");
        let errors = sink.errors.lock().unwrap();
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("API キーを確認"));
    }

    #[tokio::test]
    async fn worker_retries_server_errors_then_reports() {
        let (tx, rx) = mpsc::unbounded_channel();
        let transcriber = Arc::new(FakeTranscriber {
            fail_status: Some(500),
            ..Default::default()
        });
        let sink = Arc::new(TestSink::default());
        tx.send(job(Speaker::Me, T0)).unwrap();
        tx.send(job(Speaker::Me, T0 + 2_000)).unwrap();
        drop(tx);
        run_transcription_worker(rx, transcriber.clone(), sink.clone(), fast_policy()).await;
        // 1 件につき 2 回（再試行 1 回）、2 件とも試す
        assert_eq!(transcriber.calls.lock().unwrap().len(), 4);
        assert_eq!(sink.errors.lock().unwrap().len(), 2);
        assert!(sink.errors.lock().unwrap()[0].starts_with("自分の発話"));
    }
}
