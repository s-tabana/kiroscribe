//! 録音セッション。音声の取り込み（専用スレッド）と、音源ごとの処理スレッド・文字起こしワーカーを束ねる。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::audio::capture;
use crate::error::{CoreError, Result};
use crate::recorder::{run_transcription_worker_shared, spawn_source_thread, RecorderSink, SourceProcessor};
use crate::retry::RetryPolicy;
use crate::transcribe::Transcribe;
use crate::transcript::Speaker;

#[derive(Debug, Clone, Default)]
pub struct SessionOptions {
    pub mic_device_id: Option<String>,
    pub capture_system_audio: bool,
}

pub struct RecordingSession {
    paused: Arc<AtomicBool>,
    stop_tx: Option<std::sync::mpsc::Sender<()>>,
    capture_thread: Option<JoinHandle<()>>,
    processor_threads: Vec<JoinHandle<()>>,
    workers: Vec<tokio::task::JoinHandle<()>>,
    sources: Vec<Speaker>,
}

impl RecordingSession {
    /// 録音を始める。音声デバイスは専用スレッドで開き、停止までそのスレッドが保持する
    /// （Windows の COM 初期化を UI スレッドや非同期ランタイムと混ぜないため）。
    pub fn start(
        options: SessionOptions,
        transcriber: Arc<dyn Transcribe>,
        sink: Arc<dyn RecorderSink>,
        runtime: &tokio::runtime::Handle,
    ) -> Result<Self> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let capture_sink = sink.clone();
        let capture_thread = std::thread::Builder::new()
            .name("kiroscribe-capture".into())
            .spawn(move || {
                match capture::open_sources(
                    options.mic_device_id.as_deref(),
                    options.capture_system_audio,
                    capture_sink,
                ) {
                    Ok((streams, sources)) => {
                        if ready_tx.send(Ok(sources)).is_err() {
                            return;
                        }
                        // 停止の合図（または送信側の破棄）まで取り込みを続ける
                        let _ = stop_rx.recv();
                        drop(streams);
                    }
                    Err(err) => {
                        let _ = ready_tx.send(Err(err));
                    }
                }
            })
            .map_err(|e| CoreError::Audio(format!("録音スレッドを起動できません: {e}")))?;

        let sources = ready_rx
            .recv()
            .map_err(|_| CoreError::Audio("録音スレッドが異常終了しました".into()))??;

        let paused = Arc::new(AtomicBool::new(false));
        // API キーの誤りは、音源ごとではなく録音 1 回につき 1 回だけ知らせる
        let auth_failed = Arc::new(AtomicBool::new(false));
        let mut processor_threads = Vec::new();
        let mut workers = Vec::new();
        let mut speakers = Vec::new();
        for source in sources {
            let (job_tx, job_rx) = tokio::sync::mpsc::unbounded_channel();
            let processor = SourceProcessor::new(
                source.speaker,
                source.sample_rate,
                paused.clone(),
                job_tx,
                sink.clone(),
            );
            processor_threads.push(spawn_source_thread(processor, source.frames));
            workers.push(runtime.spawn(run_transcription_worker_shared(
                job_rx,
                transcriber.clone(),
                sink.clone(),
                RetryPolicy::default(),
                auth_failed.clone(),
            )));
            speakers.push(source.speaker);
        }

        Ok(Self {
            paused,
            stop_tx: Some(stop_tx),
            capture_thread: Some(capture_thread),
            processor_threads,
            workers,
            sources: speakers,
        })
    }

    /// 取り込んでいる音源（自分・相手）。
    pub fn sources(&self) -> &[Speaker] {
        &self.sources
    }

    pub fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        self.paused.store(false, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// 取り込みを止め、残っている発話の文字起こしがすべて終わるまで待つ。
    pub async fn stop(mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        let capture_thread = self.capture_thread.take();
        let processor_threads = std::mem::take(&mut self.processor_threads);
        // スレッドの終了待ちは同期処理なので、非同期ランタイムを塞がないよう別スレッドで待つ
        let _ = tokio::task::spawn_blocking(move || {
            if let Some(handle) = capture_thread {
                let _ = handle.join();
            }
            for handle in processor_threads {
                let _ = handle.join();
            }
        })
        .await;
        for worker in std::mem::take(&mut self.workers) {
            let _ = worker.await;
        }
    }
}

impl Drop for RecordingSession {
    fn drop(&mut self) {
        // stop() を呼ばずに破棄された場合も取り込みは止める
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
    }
}
