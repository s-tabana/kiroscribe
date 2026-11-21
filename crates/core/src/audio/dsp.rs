//! 小さな信号処理: モノラル化、リサンプル、音量（RMS）。

/// インターリーブされた多チャンネル音声をモノラルに平均する。
pub fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect()
}

/// 二乗平均平方根（0.0〜1.0 程度の音量）。
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// ストリーム向けのリサンプラ。
///
/// 間引くとき（48kHz → 16kHz など）は、窓関数付き sinc の FIR ローパスで新しいナイキスト周波数より
/// 上の成分を落としてから線形補間する（折り返し雑音の防止）。音声認識の入力としては十分な品質。
pub struct Resampler {
    step: f64,
    pos: f64,
    pending: Vec<f32>,
    taps: Vec<f32>,
    history: Vec<f32>,
    history_index: usize,
}

/// Blackman 窓をかけた sinc ローパス。`cutoff` は入力サンプルレートに対する比（0〜0.5）。
fn lowpass_taps(cutoff: f64, len: usize) -> Vec<f32> {
    let mid = (len - 1) as f64 / 2.0;
    let mut taps: Vec<f64> = (0..len)
        .map(|i| {
            let x = i as f64 - mid;
            let sinc = if x == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * std::f64::consts::PI * cutoff * x).sin() / (std::f64::consts::PI * x)
            };
            let t = i as f64 / (len - 1) as f64;
            let window = 0.42 - 0.5 * (2.0 * std::f64::consts::PI * t).cos()
                + 0.08 * (4.0 * std::f64::consts::PI * t).cos();
            sinc * window
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    taps.iter_mut().for_each(|t| *t /= sum);
    taps.into_iter().map(|t| t as f32).collect()
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        assert!(in_rate > 0 && out_rate > 0, "サンプルレートは正の値");
        let step = in_rate as f64 / out_rate as f64;
        let taps = if step > 1.0 {
            // 出力のナイキスト周波数の 90% を通過域の上限にする
            let cutoff = 0.45 / step;
            let len = ((step * 16.0).ceil() as usize) | 1;
            lowpass_taps(cutoff, len)
        } else {
            vec![1.0]
        };
        Self {
            step,
            pos: 0.0,
            pending: Vec::new(),
            history: vec![0.0; taps.len()],
            history_index: 0,
            taps,
        }
    }

    pub fn is_passthrough(&self) -> bool {
        self.step == 1.0
    }

    fn low_pass(&mut self, sample: f32) -> f32 {
        let len = self.taps.len();
        if len == 1 {
            return sample;
        }
        self.history[self.history_index] = sample;
        self.history_index = (self.history_index + 1) % len;
        // history_index は最も古いサンプルの位置
        let mut acc = 0.0;
        for (k, tap) in self.taps.iter().enumerate() {
            acc += tap * self.history[(self.history_index + k) % len];
        }
        acc
    }

    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        if self.is_passthrough() {
            return input.to_vec();
        }
        for &sample in input {
            let filtered = self.low_pass(sample);
            self.pending.push(filtered);
        }
        let mut out = Vec::with_capacity((input.len() as f64 / self.step) as usize + 2);
        while self.pos + 1.0 < self.pending.len() as f64 {
            let index = self.pos.floor() as usize;
            let frac = (self.pos - index as f64) as f32;
            let a = self.pending[index];
            let b = self.pending[index + 1];
            out.push(a + (b - a) * frac);
            self.pos += self.step;
        }
        let consumed = (self.pos.floor() as usize).min(self.pending.len());
        self.pending.drain(..consumed);
        self.pos -= consumed as f64;
        out
    }

    pub fn reset(&mut self) {
        self.pos = 0.0;
        self.pending.clear();
        self.history.iter_mut().for_each(|h| *h = 0.0);
        self.history_index = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, rate: u32, seconds: f32, amp: f32) -> Vec<f32> {
        let n = (rate as f32 * seconds) as usize;
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin())
            .collect()
    }

    #[test]
    fn downmix_averages_channels() {
        assert_eq!(downmix(&[1.0, 0.0, 0.5, 0.5], 2), vec![0.5, 0.5]);
        assert_eq!(downmix(&[0.3, 0.1], 1), vec![0.3, 0.1]);
    }

    #[test]
    fn rms_of_sine_is_amplitude_over_sqrt2() {
        let s = sine(440.0, 16_000, 1.0, 0.5);
        assert!((rms(&s) - 0.5 / 2f32.sqrt()).abs() < 0.01);
        assert_eq!(rms(&[]), 0.0);
    }

    #[test]
    fn downsample_48k_keeps_length_and_level() {
        let input = sine(440.0, 48_000, 1.0, 0.5);
        let mut r = Resampler::new(48_000, 16_000);
        // 細切れに入れても結果が連続する
        let out: Vec<f32> = input.chunks(441).flat_map(|c| r.process(c)).collect();
        assert!((out.len() as i64 - 16_000).abs() <= 2, "len = {}", out.len());
        let level = rms(&out[100..]);
        assert!((level - 0.5 / 2f32.sqrt()).abs() < 0.02, "rms = {level}");
    }

    #[test]
    fn downsample_44_1k_non_integer_ratio() {
        let input = sine(300.0, 44_100, 2.0, 0.4);
        let mut r = Resampler::new(44_100, 16_000);
        let out: Vec<f32> = input.chunks(1000).flat_map(|c| r.process(c)).collect();
        assert!((out.len() as i64 - 32_000).abs() <= 2, "len = {}", out.len());
        assert!((rms(&out[200..]) - 0.4 / 2f32.sqrt()).abs() < 0.02);
    }

    #[test]
    fn frequencies_above_new_nyquist_are_removed() {
        // 16kHz では表せない 11kHz は、そのままだと 5kHz に折り返して聞こえてしまう
        let input = sine(11_000.0, 48_000, 0.5, 0.5);
        let mut r = Resampler::new(48_000, 16_000);
        let out = r.process(&input);
        assert!(rms(&out[100..]) < 0.01, "rms = {}", rms(&out[100..]));
    }

    #[test]
    fn speech_band_passes_through() {
        let input = sine(3_000.0, 48_000, 0.5, 0.5);
        let mut r = Resampler::new(48_000, 16_000);
        let out = r.process(&input);
        assert!((rms(&out[100..]) - 0.5 / 2f32.sqrt()).abs() < 0.02);
    }

    #[test]
    fn same_rate_is_passthrough() {
        let mut r = Resampler::new(16_000, 16_000);
        assert!(r.is_passthrough());
        assert_eq!(r.process(&[0.1, 0.2]), vec![0.1, 0.2]);
    }
}
