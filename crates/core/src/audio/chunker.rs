//! 音声を「発話のまとまり」に区切る簡易 VAD（音声区間検出）。
//!
//! 30ms ごとのフレームの音量を見て、背景雑音より十分大きければ発話とみなす。
//! 発話のあとに一定時間の無音が続いたら、そこで 1 チャンクとして切り出す。
//! 無音だけの区間は API に送らない（料金の節約と、無音から文章を作ってしまう誤認識の防止）。

use super::dsp::rms;

#[derive(Debug, Clone)]
pub struct ChunkerConfig {
    pub sample_rate: u32,
    /// 判定の単位
    pub frame_ms: u32,
    /// これより小さい音量は常に無音（-40dBFS 付近）
    pub min_threshold: f32,
    /// 背景雑音の何倍を超えたら発話とみなすか
    pub noise_multiplier: f32,
    /// 背景雑音の推定に使う直近フレーム数（約 3 秒）
    pub noise_window_frames: usize,
    /// 背景雑音として使う下位パーセンタイル
    pub noise_percentile: f32,
    /// 発話後この長さの無音が続いたら区切る
    pub end_silence_ms: u32,
    /// これより短いチャンクは区切らずに待つ
    pub min_chunk_ms: u32,
    /// これより長くなったら強制的に区切る
    pub max_chunk_ms: u32,
    /// 発話の直前に残しておく無音（語頭の欠け防止）
    pub preroll_ms: u32,
    /// 末尾に残す無音
    pub keep_trailing_ms: u32,
    /// 発話がこれより短いチャンクは捨てる（咳・クリック音など）
    pub min_speech_ms: u32,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            sample_rate: super::TARGET_SAMPLE_RATE,
            frame_ms: 30,
            min_threshold: 0.01,
            noise_multiplier: 2.5,
            noise_window_frames: 100,
            noise_percentile: 0.1,
            end_silence_ms: 700,
            min_chunk_ms: 1_500,
            max_chunk_ms: 25_000,
            preroll_ms: 300,
            keep_trailing_ms: 200,
            min_speech_ms: 400,
        }
    }
}

/// 切り出した発話。`start_sample` は区切り開始（`reset` 時点）からのサンプル位置。
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub start_sample: u64,
    pub samples: Vec<f32>,
    pub speech_ms: u32,
}

impl Chunk {
    pub fn end_sample(&self) -> u64 {
        self.start_sample + self.samples.len() as u64
    }
}

pub struct Chunker {
    cfg: ChunkerConfig,
    frame_len: usize,
    partial: Vec<f32>,
    consumed: u64,
    current: Vec<f32>,
    current_start: u64,
    in_speech: bool,
    speech_frames: u32,
    silence_frames: u32,
    recent_levels: Vec<f32>,
    recent_index: usize,
}

impl Chunker {
    pub fn new(cfg: ChunkerConfig) -> Self {
        let frame_len = (cfg.sample_rate * cfg.frame_ms / 1000) as usize;
        Self {
            frame_len,
            partial: Vec::with_capacity(frame_len),
            consumed: 0,
            current: Vec::new(),
            current_start: 0,
            in_speech: false,
            speech_frames: 0,
            silence_frames: 0,
            recent_levels: Vec::with_capacity(cfg.noise_window_frames),
            recent_index: 0,
            cfg,
        }
    }

    fn ms_to_samples(&self, ms: u32) -> usize {
        (self.cfg.sample_rate as u64 * ms as u64 / 1000) as usize
    }

    fn samples_to_ms(&self, samples: usize) -> u32 {
        (samples as u64 * 1000 / self.cfg.sample_rate as u64) as u32
    }

    /// 直近の音量の下位パーセンタイルを背景雑音とみなし、発話判定のしきい値を返す。
    fn threshold(&mut self, level: f32) -> f32 {
        if self.recent_levels.len() < self.cfg.noise_window_frames {
            self.recent_levels.push(level);
        } else {
            self.recent_levels[self.recent_index] = level;
            self.recent_index = (self.recent_index + 1) % self.cfg.noise_window_frames;
        }
        let mut sorted = self.recent_levels.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let index = ((sorted.len() - 1) as f32 * self.cfg.noise_percentile).round() as usize;
        (sorted[index] * self.cfg.noise_multiplier).max(self.cfg.min_threshold)
    }

    /// 16kHz モノラルの音声を追加し、確定したチャンクを返す。
    pub fn push(&mut self, samples: &[f32]) -> Vec<Chunk> {
        let mut chunks = Vec::new();
        let mut rest = samples;
        while !rest.is_empty() {
            let need = self.frame_len - self.partial.len();
            let take = need.min(rest.len());
            self.partial.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.partial.len() == self.frame_len {
                let frame = std::mem::take(&mut self.partial);
                if let Some(chunk) = self.process_frame(&frame) {
                    chunks.push(chunk);
                }
                self.partial = frame;
                self.partial.clear();
            }
        }
        chunks
    }

    fn process_frame(&mut self, frame: &[f32]) -> Option<Chunk> {
        let level = rms(frame);
        let is_speech = level > self.threshold(level);
        self.consumed += frame.len() as u64;

        if !self.in_speech {
            self.current.extend_from_slice(frame);
            if is_speech {
                self.in_speech = true;
                self.speech_frames = 1;
                self.silence_frames = 0;
            } else {
                let preroll = self.ms_to_samples(self.cfg.preroll_ms);
                if self.current.len() > preroll {
                    let excess = self.current.len() - preroll;
                    self.current.drain(..excess);
                    self.current_start += excess as u64;
                }
            }
            return None;
        }

        self.current.extend_from_slice(frame);
        if is_speech {
            self.speech_frames += 1;
            self.silence_frames = 0;
        } else {
            self.silence_frames += 1;
        }

        let length_ms = self.samples_to_ms(self.current.len());
        if self.silence_frames * self.cfg.frame_ms >= self.cfg.end_silence_ms
            && length_ms >= self.cfg.min_chunk_ms
        {
            return self.finalize(true);
        }
        if length_ms >= self.cfg.max_chunk_ms {
            let chunk = self.finalize(false);
            // 話の途中で切った場合は、次のチャンクも発話中として続ける
            self.in_speech = is_speech;
            return chunk;
        }
        None
    }

    fn finalize(&mut self, trim_trailing_silence: bool) -> Option<Chunk> {
        let mut samples = std::mem::take(&mut self.current);
        let start = self.current_start;

        if trim_trailing_silence {
            let trailing = self.silence_frames as usize * self.frame_len;
            let remove = trailing.saturating_sub(self.ms_to_samples(self.cfg.keep_trailing_ms));
            let cut = samples.len() - remove.min(samples.len());
            let mut tail = samples.split_off(cut);
            // 切り落とした無音の末尾は、次の発話の語頭用に残す
            let preroll = self.ms_to_samples(self.cfg.preroll_ms);
            let drop = tail.len().saturating_sub(preroll);
            tail.drain(..drop);
            self.current_start = start + (cut + drop) as u64;
            self.current = tail;
        } else {
            self.current_start = self.consumed;
        }

        let speech_ms = self.speech_frames * self.cfg.frame_ms;
        self.in_speech = false;
        self.speech_frames = 0;
        self.silence_frames = 0;

        (speech_ms >= self.cfg.min_speech_ms && !samples.is_empty()).then_some(Chunk {
            start_sample: start,
            samples,
            speech_ms,
        })
    }

    /// 録音停止・一時停止時に、途中の発話を確定させる。
    pub fn flush(&mut self) -> Option<Chunk> {
        let partial = std::mem::take(&mut self.partial);
        if self.in_speech {
            self.current.extend_from_slice(&partial);
            self.consumed += partial.len() as u64;
            let chunk = self.finalize(true);
            self.current.clear();
            self.current_start = self.consumed;
            chunk
        } else {
            self.consumed += partial.len() as u64;
            self.current.clear();
            self.current_start = self.consumed;
            None
        }
    }

    /// 時刻の基準を 0 に戻す（再開時）。背景雑音の推定は引き継ぐ。
    pub fn reset(&mut self) {
        self.partial.clear();
        self.consumed = 0;
        self.current.clear();
        self.current_start = 0;
        self.in_speech = false;
        self.speech_frames = 0;
        self.silence_frames = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: usize = 16_000;

    fn tone(seconds: f32, amp: f32) -> Vec<f32> {
        let n = (RATE as f32 * seconds) as usize;
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * 220.0 * i as f32 / RATE as f32).sin())
            .collect()
    }

    /// 音節ごとに強弱がある話し声に近い信号（200ms 発声 + 100ms 弱音の繰り返し）。
    /// 一定の音量が続く音は背景雑音とみなされるため、長い発話のテストにはこちらを使う。
    fn speechlike(seconds: f32, amp: f32) -> Vec<f32> {
        tone(seconds, amp)
            .into_iter()
            .enumerate()
            .map(|(i, s)| if (i * 1000 / RATE) % 300 < 200 { s } else { s * 0.02 })
            .collect()
    }

    fn silence(seconds: f32) -> Vec<f32> {
        vec![0.0; (RATE as f32 * seconds) as usize]
    }

    /// 再現性のある疑似乱数の雑音
    fn noise(seconds: f32, amp: f32, seed: &mut u32) -> Vec<f32> {
        (0..(RATE as f32 * seconds) as usize)
            .map(|_| {
                *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (*seed as f32 / u32::MAX as f32 * 2.0 - 1.0) * amp
            })
            .collect()
    }

    fn run(chunker: &mut Chunker, audio: &[f32]) -> Vec<Chunk> {
        // 実際の取り込みと同じく細切れで入れる
        let mut out: Vec<Chunk> = audio.chunks(160).flat_map(|c| chunker.push(c)).collect();
        out.extend(chunker.flush());
        out
    }

    fn sec(sample: u64) -> f32 {
        sample as f32 / RATE as f32
    }

    #[test]
    fn silence_produces_nothing() {
        let mut c = Chunker::new(ChunkerConfig::default());
        assert!(run(&mut c, &silence(10.0)).is_empty());
    }

    #[test]
    fn utterances_separated_by_silence_become_separate_chunks() {
        let mut audio = silence(1.0);
        audio.extend(tone(1.2, 0.3)); // 1.0s〜2.2s
        audio.extend(silence(1.5));
        audio.extend(tone(2.0, 0.3)); // 3.7s〜5.7s
        audio.extend(silence(1.5));

        let mut c = Chunker::new(ChunkerConfig::default());
        let chunks = run(&mut c, &audio);
        assert_eq!(chunks.len(), 2, "{:?}", chunks.iter().map(|c| (sec(c.start_sample), sec(c.end_sample()))).collect::<Vec<_>>());

        // 語頭の前に約 0.3 秒の余白、語尾の後に約 0.2 秒の余白
        assert!((sec(chunks[0].start_sample) - 0.7).abs() < 0.05);
        assert!((sec(chunks[0].end_sample()) - 2.4).abs() < 0.06);
        assert!((sec(chunks[1].start_sample) - 3.4).abs() < 0.05);
        assert!((sec(chunks[1].end_sample()) - 5.9).abs() < 0.06);
        assert!(chunks[1].speech_ms >= 1_900);
    }

    #[test]
    fn long_speech_is_split_at_max_length() {
        let mut c = Chunker::new(ChunkerConfig::default());
        let chunks = run(&mut c, &speechlike(60.0, 0.3));
        assert_eq!(chunks.len(), 3);
        for chunk in &chunks {
            assert!(chunk.samples.len() <= 25 * RATE + 480);
        }
        // 途切れずにつながっている
        assert_eq!(chunks[0].end_sample(), chunks[1].start_sample);
        assert_eq!(chunks[1].end_sample(), chunks[2].start_sample);
        assert!((sec(chunks[2].end_sample()) - 60.0).abs() < 0.05);
    }

    #[test]
    fn constant_tone_is_treated_as_background() {
        // 換気扇やBGMのように一定の音が続くだけでは発話とみなさない
        let mut c = Chunker::new(ChunkerConfig::default());
        let chunks = run(&mut c, &tone(30.0, 0.3));
        assert!(chunks.iter().map(|c| c.speech_ms).sum::<u32>() < 1_000);
    }

    #[test]
    fn short_clicks_are_dropped() {
        let mut audio = silence(1.0);
        audio.extend(tone(0.1, 0.5));
        audio.extend(silence(2.0));
        let mut c = Chunker::new(ChunkerConfig::default());
        assert!(run(&mut c, &audio).is_empty());
    }

    #[test]
    fn steady_background_noise_is_not_speech() {
        let mut seed = 42;
        let mut audio = noise(3.0, 0.05, &mut seed);
        let mut speech = tone(1.5, 0.4);
        let bed = noise(1.5, 0.05, &mut seed);
        speech.iter_mut().zip(bed).for_each(|(s, n)| *s += n);
        audio.extend(speech); // 3.0s〜4.5s
        audio.extend(noise(3.0, 0.05, &mut seed));

        let mut c = Chunker::new(ChunkerConfig::default());
        let chunks = run(&mut c, &audio);
        assert_eq!(chunks.len(), 1, "{:?}", chunks.iter().map(|c| (sec(c.start_sample), sec(c.end_sample()))).collect::<Vec<_>>());
        assert!((sec(chunks[0].start_sample) - 2.7).abs() < 0.1);
        assert!(chunks[0].speech_ms >= 1_300);
    }

    #[test]
    fn flush_emits_speech_in_progress_and_reset_restarts_clock() {
        let mut c = Chunker::new(ChunkerConfig::default());
        let mut audio = silence(0.5);
        audio.extend(tone(1.0, 0.3));
        for part in audio.chunks(160) {
            assert!(c.push(part).is_empty());
        }
        let chunk = c.flush().expect("発話中のチャンクが確定する");
        assert!((sec(chunk.start_sample) - 0.2).abs() < 0.05);
        assert!(c.flush().is_none());

        c.reset();
        let chunks = run(&mut c, &[tone(1.0, 0.3), silence(1.0)].concat());
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].start_sample, 0, "reset 後は 0 から数える");
    }
}
