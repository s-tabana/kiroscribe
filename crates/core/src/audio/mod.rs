//! 音声処理。マイク・システム音声の取り込み（capture）と、文字起こしに送る前の加工
//! （モノラル化・リサンプル・無音での区切り・WAV 化）。

pub mod capture;
pub mod chunker;
pub mod dsp;
pub mod wav;

/// 文字起こし API に送るサンプルレート。音声認識には 16kHz モノラルで十分。
pub const TARGET_SAMPLE_RATE: u32 = 16_000;
