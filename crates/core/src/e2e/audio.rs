//! 模擬の音声の取り込み。実際の音声デバイスの代わりに、合成音か WAV ファイルを実時間で流し込む。
//!
//! 取り込みより後ろ（16kHz 化・発話での区切り・WAV 化・文字起こし API の呼び出し）は製品と同じコードが動く。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::audio::capture::{AudioDevice, OpenedSource};
use crate::error::{CoreError, Result};
use crate::recorder::AudioFrame;
use crate::time::now_ms;
use crate::transcript::Speaker;

/// 合成音のサンプルレート（実際のデバイスと同じく、16kHz への変換を通す）
const SYNTH_RATE: u32 = 48_000;
/// 音声を送る単位（実際のデバイスと同じく 10 ミリ秒ごと）
const FRAME_MS: u64 = 10;

/// 発話に似た合成音の 1 周期: 無音 → 発話 → 無音。発話のあとの無音で区切りが検出される。
const SPEECH_LEAD_MS: u32 = 300;
const SPEECH_TALK_MS: u32 = 1_800;
const SPEECH_TAIL_MS: u32 = 900;
/// 相手の声は、自分の声と重ならないよう遅らせて始める
const THEM_DELAY_MS: u32 = 1_500;

/// 模擬の音声の種類（`config.json` の `audio.mic`・`audio.system`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// 発話に似た合成音を繰り返す（約 3 秒ごとに 1 つの発話として区切られる）
    Speech,
    /// 無音
    Silence,
    /// WAV ファイル（16bit PCM）を 1 回流し、そのあとは無音
    Wav(PathBuf),
}

pub fn parse_source(spec: &str) -> Source {
    match spec.trim() {
        "speech" => Source::Speech,
        "silence" => Source::Silence,
        path => Source::Wav(PathBuf::from(path)),
    }
}

/// 模擬の音声が指定されているか（されていれば、実際の音声デバイスは使わない）。
pub fn enabled() -> bool {
    super::config().audio.mic.is_some()
}

/// 設定画面に見せるマイクの一覧。
pub fn devices() -> Vec<AudioDevice> {
    vec![AudioDevice {
        id: "e2e-mic".into(),
        name: "模擬のマイク".into(),
        is_default: true,
    }]
}

/// 発話に似た信号（`crates/core/tests/pipeline.rs` と同じ作り方）: 180Hz の音を 0.2 秒鳴らして 0.1 秒弱める、の繰り返し。
pub fn speechlike(sample_rate: u32, ms: u32) -> Vec<f32> {
    let count = sample_rate as u64 * ms as u64 / 1000;
    (0..count)
        .map(|i| {
            let s = 0.25 * (2.0 * std::f32::consts::PI * 180.0 * i as f32 / sample_rate as f32).sin();
            if (i * 1000 / sample_rate as u64) % 300 < 200 {
                s
            } else {
                s * 0.02
            }
        })
        .collect()
}

fn silence(sample_rate: u32, ms: u32) -> Vec<f32> {
    vec![0.0; (sample_rate as u64 * ms as u64 / 1000) as usize]
}

/// 合成音の 1 周期ぶん。
pub fn speech_period(sample_rate: u32) -> Vec<f32> {
    let mut samples = silence(sample_rate, SPEECH_LEAD_MS);
    samples.extend(speechlike(sample_rate, SPEECH_TALK_MS));
    samples.extend(silence(sample_rate, SPEECH_TAIL_MS));
    samples
}

/// 流し込む音声。`lead` → `body`（`looped` なら繰り返し）→ 無音、の順に取り出す。
struct Signal {
    sample_rate: u32,
    lead: usize,
    body: Vec<f32>,
    looped: bool,
    position: usize,
}

impl Signal {
    fn load(source: &Source, speaker: Speaker) -> Result<Self> {
        let lead_ms = if speaker == Speaker::Them { THEM_DELAY_MS } else { 0 };
        let (sample_rate, body, looped) = match source {
            Source::Speech => (SYNTH_RATE, speech_period(SYNTH_RATE), true),
            Source::Silence => (SYNTH_RATE, Vec::new(), false),
            Source::Wav(path) => {
                let (sample_rate, samples) = read_wav(path)?;
                (sample_rate, samples, false)
            }
        };
        Ok(Self {
            sample_rate,
            lead: (sample_rate as u64 * lead_ms as u64 / 1000) as usize,
            body,
            looped,
            position: 0,
        })
    }

    fn next(&mut self, count: usize) -> Vec<f32> {
        (0..count)
            .map(|_| {
                let position = self.position;
                self.position += 1;
                let Some(index) = position.checked_sub(self.lead) else {
                    return 0.0;
                };
                if self.body.is_empty() {
                    0.0
                } else if self.looped {
                    self.body[index % self.body.len()]
                } else {
                    self.body.get(index).copied().unwrap_or(0.0)
                }
            })
            .collect()
    }
}

/// 16bit PCM の WAV を読み、モノラルにして返す（サンプルレート, 音声）。
pub fn decode_wav(bytes: &[u8]) -> std::result::Result<(u32, Vec<f32>), String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("WAV ファイルではありません".into());
    }
    let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let u32_at = |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);

    let mut format: Option<(usize, u32)> = None; // (チャンネル数, サンプルレート)
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32_at(at + 4) as usize;
        let body = at + 8;
        let end = body.saturating_add(len).min(bytes.len());
        match id {
            b"fmt " => {
                if end - body < 16 {
                    return Err("fmt チャンクが短すぎます".into());
                }
                let (tag, channels, bits) = (u16_at(body), u16_at(body + 2), u16_at(body + 14));
                if tag != 1 || bits != 16 || channels == 0 {
                    return Err(format!(
                        "対応しているのは 16bit PCM だけです（形式 {tag}・{bits}bit・{channels} チャンネル）"
                    ));
                }
                format = Some((channels as usize, u32_at(body + 4)));
            }
            b"data" => {
                let (channels, sample_rate) = format.ok_or("data チャンクの前に fmt チャンクがありません")?;
                let samples = bytes[body..end]
                    .chunks_exact(2 * channels)
                    .map(|frame| {
                        let sum: f32 = frame
                            .chunks_exact(2)
                            .map(|s| i16::from_le_bytes([s[0], s[1]]) as f32 / i16::MAX as f32)
                            .sum();
                        sum / channels as f32
                    })
                    .collect();
                return Ok((sample_rate, samples));
            }
            _ => {}
        }
        // チャンクは偶数バイトに揃えられている
        at = body.saturating_add(len).saturating_add(len % 2);
    }
    Err("data チャンクがありません".into())
}

fn read_wav(path: &Path) -> Result<(u32, Vec<f32>)> {
    let bytes = std::fs::read(path)
        .map_err(|err| CoreError::Audio(format!("模擬の音声 {} を読めません: {err}", path.display())))?;
    decode_wav(&bytes).map_err(|err| CoreError::Audio(format!("模擬の音声 {} を読めません: {err}", path.display())))
}

/// 動いている模擬の取り込み。破棄すると止まり、音声の送信側も閉じる（実際のデバイスの `cpal::Stream` と同じ）。
pub struct MockCapture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for MockCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(mut signal: Signal, tx: Sender<AudioFrame>, stop: Arc<AtomicBool>) {
    let frame_len = (signal.sample_rate as u64 * FRAME_MS / 1000) as usize;
    let started = Instant::now();
    let started_ms = now_ms();
    let mut sent: u64 = 0;
    while !stop.load(Ordering::SeqCst) {
        // 遅れたぶんはまとめて送る（時刻は、その音声が本来届いたはずの時刻にする）
        let due = started.elapsed().as_millis() as u64 / FRAME_MS;
        while sent < due {
            sent += 1;
            let frame = AudioFrame {
                captured_at: started_ms + (sent * FRAME_MS) as i64,
                samples: signal.next(frame_len),
            };
            if tx.send(frame).is_err() {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(FRAME_MS / 2));
    }
}

fn source_for(speaker: Speaker) -> Result<Source> {
    let audio = &super::config().audio;
    let spec = match speaker {
        Speaker::Me => audio.mic.as_deref(),
        Speaker::Them => audio.system.as_deref(),
    };
    spec.map(parse_source)
        .ok_or_else(|| CoreError::Audio(format!("{}の模擬の音声が指定されていません", speaker.label())))
}

/// 作業フォルダにこのファイルがあるあいだは、マイクを開けなかったことにする（中身が理由の文）。
/// マイクがない・使えないときの動きを確かめるために、テストが実行中に置いたり消したりする。
const MIC_ERROR_FILE: &str = "mic-error.txt";

fn mic_error() -> Option<String> {
    let text = std::fs::read_to_string(super::file(MIC_ERROR_FILE)?).ok()?;
    Some(text.trim().to_string())
}

/// 模擬の音源を開く。
pub fn open(speaker: Speaker) -> Result<(MockCapture, OpenedSource)> {
    if speaker == Speaker::Me {
        if let Some(reason) = mic_error() {
            return Err(CoreError::Audio(reason));
        }
    }
    let source = source_for(speaker)?;
    let signal = Signal::load(&source, speaker)?;
    let sample_rate = signal.sample_rate;
    let (tx, rx) = channel();
    let stop = Arc::new(AtomicBool::new(false));
    let thread = std::thread::Builder::new()
        .name(format!("kiroscribe-e2e-audio-{}", speaker.as_str()))
        .spawn({
            let stop = stop.clone();
            move || run(signal, tx, stop)
        })
        .map_err(|err| CoreError::Audio(format!("模擬の音声のスレッドを起動できません: {err}")))?;
    Ok((
        MockCapture {
            stop,
            thread: Some(thread),
        },
        OpenedSource {
            speaker,
            sample_rate,
            device_name: format!("模擬の音声（{source:?}）"),
            frames: rx,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::chunker::{Chunker, ChunkerConfig};
    use crate::audio::wav::encode_wav_mono_i16;
    use crate::audio::TARGET_SAMPLE_RATE;

    #[test]
    fn source_names() {
        assert_eq!(parse_source("speech"), Source::Speech);
        assert_eq!(parse_source(" silence "), Source::Silence);
        assert_eq!(parse_source("/tmp/a.wav"), Source::Wav(PathBuf::from("/tmp/a.wav")));
    }

    /// 合成音は、製品の区切りの設定で 1 周期につき 1 つの発話として切り出される
    /// （区切りの設定を変えて切り出されなくなったら、E2E テストより先にここで気づける）。
    #[test]
    fn synthetic_speech_is_chunked_once_per_period() {
        let mut chunker = Chunker::new(ChunkerConfig::default());
        let period = speech_period(TARGET_SAMPLE_RATE);
        let mut chunks = Vec::new();
        for _ in 0..3 {
            for block in period.chunks(TARGET_SAMPLE_RATE as usize / 100) {
                chunks.extend(chunker.push(block));
            }
        }
        assert_eq!(chunks.len(), 3, "3 周期で 3 つの発話");
        for chunk in &chunks {
            assert!(chunk.speech_ms >= 1_000, "発話の長さ: {} ms", chunk.speech_ms);
        }
        // 1 つ目は、最初の発話が終わって区切りの無音（0.7 秒）がたった時点で確定している
        let first_end_ms = chunks[0].end_sample() * 1000 / TARGET_SAMPLE_RATE as u64;
        assert!(first_end_ms <= (SPEECH_LEAD_MS + SPEECH_TALK_MS + 700) as u64, "{first_end_ms}");
    }

    #[test]
    fn signal_plays_lead_then_body() {
        let mut once = Signal {
            sample_rate: 1000,
            lead: 2,
            body: vec![0.5, -0.5],
            looped: false,
            position: 0,
        };
        assert_eq!(once.next(6), vec![0.0, 0.0, 0.5, -0.5, 0.0, 0.0]);

        let mut looped = Signal {
            sample_rate: 1000,
            lead: 1,
            body: vec![0.5, -0.5],
            looped: true,
            position: 0,
        };
        assert_eq!(looped.next(3), vec![0.0, 0.5, -0.5]);
        assert_eq!(looped.next(3), vec![0.5, -0.5, 0.5], "続きから取り出す");
    }

    #[test]
    fn wav_roundtrip_and_stereo_downmix() {
        let mono = encode_wav_mono_i16(&[0.0, 0.5, -0.5, 1.0], 16_000);
        let (rate, samples) = decode_wav(&mono).unwrap();
        assert_eq!(rate, 16_000);
        assert_eq!(samples.len(), 4);
        assert!((samples[1] - 0.5).abs() < 0.001 && (samples[2] + 0.5).abs() < 0.001);

        // ステレオ（左 1.0・右 0.0 → 0.5）。fmt と data のあいだに別のチャンク（奇数長）があっても読める
        let mut stereo = Vec::new();
        stereo.extend_from_slice(b"RIFF\0\0\0\0WAVEfmt ");
        stereo.extend_from_slice(&16u32.to_le_bytes());
        stereo.extend_from_slice(&1u16.to_le_bytes());
        stereo.extend_from_slice(&2u16.to_le_bytes());
        stereo.extend_from_slice(&44_100u32.to_le_bytes());
        stereo.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
        stereo.extend_from_slice(&4u16.to_le_bytes());
        stereo.extend_from_slice(&16u16.to_le_bytes());
        stereo.extend_from_slice(b"LIST");
        stereo.extend_from_slice(&3u32.to_le_bytes());
        stereo.extend_from_slice(b"abc\0");
        stereo.extend_from_slice(b"data");
        stereo.extend_from_slice(&4u32.to_le_bytes());
        stereo.extend_from_slice(&i16::MAX.to_le_bytes());
        stereo.extend_from_slice(&0i16.to_le_bytes());
        let (rate, samples) = decode_wav(&stereo).unwrap();
        assert_eq!(rate, 44_100);
        assert_eq!(samples, vec![0.5]);
    }

    #[test]
    fn unsupported_wav_is_an_error() {
        assert!(decode_wav(b"not a wav").is_err());
        let mut float = encode_wav_mono_i16(&[0.0], 16_000);
        float[20] = 3; // IEEE float
        assert!(decode_wav(&float).unwrap_err().contains("16bit PCM"));
        let header_only = &encode_wav_mono_i16(&[], 16_000)[..36];
        assert!(decode_wav(header_only).unwrap_err().contains("data"));
    }

    /// 実時間で 10 ミリ秒ごとに届き、破棄すると止まって送信側が閉じる。
    #[test]
    fn capture_streams_in_real_time_until_dropped() {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let signal = Signal {
            sample_rate: 16_000,
            lead: 0,
            body: vec![0.25; 160],
            looped: true,
            position: 0,
        };
        let before = now_ms();
        let thread = std::thread::spawn({
            let stop = stop.clone();
            move || run(signal, tx, stop)
        });
        let capture = MockCapture {
            stop,
            thread: Some(thread),
        };
        std::thread::sleep(Duration::from_millis(250));
        drop(capture);

        let frames: Vec<AudioFrame> = rx.iter().collect(); // 送信側が閉じているので終わる
        assert!((15..=30).contains(&frames.len()), "約 0.25 秒ぶん: {}", frames.len());
        assert!(frames.iter().all(|f| f.samples.len() == 160));
        for pair in frames.windows(2) {
            assert_eq!(pair[1].captured_at - pair[0].captured_at, FRAME_MS as i64);
        }
        assert!((frames[0].captured_at - before - FRAME_MS as i64).abs() < 100);
    }
}
