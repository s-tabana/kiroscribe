//! cpal による音声の取り込み。
//!
//! - マイク: 入力デバイスから取り込み「自分」とする
//! - システム音声: 既定の出力デバイスを WASAPI のループバックで取り込み「相手」とする（Windows のみ）。
//!   cpal は出力デバイスに対して入力ストリームを作ると自動的にループバック録音になる。

use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig, SupportedStreamConfig};
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};
use crate::recorder::{AudioFrame, RecorderSink};
use crate::time::now_ms;
use crate::transcript::Speaker;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

fn audio_error(context: &str, err: impl std::fmt::Display) -> CoreError {
    CoreError::Audio(format!("{context}: {err}"))
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "（名前を取得できないデバイス）".to_string())
}

fn device_id(device: &cpal::Device) -> Option<String> {
    device.id().ok().map(|id| id.to_string())
}

/// 録音に使えるマイクの一覧。
pub fn list_input_devices() -> Result<Vec<AudioDevice>> {
    let host = cpal::default_host();
    let default_id = host.default_input_device().as_ref().and_then(device_id);
    let devices = host
        .input_devices()
        .map_err(|e| audio_error("入力デバイスの一覧を取得できません", e))?;
    Ok(devices
        .filter_map(|device| {
            let id = device_id(&device)?;
            Some(AudioDevice {
                is_default: Some(&id) == default_id.as_ref(),
                name: device_name(&device),
                id,
            })
        })
        .collect())
}

/// 開いた音源。`frames` にモノラル化した音声が届く。
pub struct OpenedSource {
    pub speaker: Speaker,
    pub sample_rate: u32,
    pub device_name: String,
    pub frames: Receiver<AudioFrame>,
}

fn build_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    tx: Sender<AudioFrame>,
    speaker: Speaker,
    sink: Arc<dyn RecorderSink>,
) -> std::result::Result<cpal::Stream, cpal::Error>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let channels = config.channels as usize;
    device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let mono: Vec<f32> = if channels <= 1 {
                data.iter().map(|s| s.to_sample::<f32>()).collect()
            } else {
                data.chunks_exact(channels)
                    .map(|frame| {
                        frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / channels as f32
                    })
                    .collect()
            };
            let _ = tx.send(AudioFrame {
                captured_at: now_ms(),
                samples: mono,
            });
        },
        move |err: cpal::Error| {
            // 取りこぼし（xrun）は頻繁に起きうるので通知しない
            if !matches!(err.kind(), cpal::ErrorKind::Xrun) {
                sink.on_error(format!("{}の音声デバイスでエラーが発生しました: {err}", speaker.label()));
            }
        },
        None,
    )
}

fn open_device(
    device: &cpal::Device,
    config: SupportedStreamConfig,
    speaker: Speaker,
    sink: Arc<dyn RecorderSink>,
) -> Result<(cpal::Stream, OpenedSource)> {
    let (tx, rx) = channel();
    let sample_rate = config.sample_rate();
    let format = config.sample_format();
    let stream_config: StreamConfig = config.into();
    let stream = match format {
        SampleFormat::F32 => build_stream::<f32>(device, stream_config, tx, speaker, sink),
        SampleFormat::I16 => build_stream::<i16>(device, stream_config, tx, speaker, sink),
        SampleFormat::I32 => build_stream::<i32>(device, stream_config, tx, speaker, sink),
        SampleFormat::U16 => build_stream::<u16>(device, stream_config, tx, speaker, sink),
        SampleFormat::U8 => build_stream::<u8>(device, stream_config, tx, speaker, sink),
        other => {
            return Err(CoreError::Audio(format!(
                "未対応の音声フォーマットです: {other}"
            )))
        }
    }
    .map_err(|e| audio_error(&format!("{}の音声を開けません", speaker.label()), e))?;
    stream
        .play()
        .map_err(|e| audio_error(&format!("{}の録音を開始できません", speaker.label()), e))?;
    Ok((
        stream,
        OpenedSource {
            speaker,
            sample_rate,
            device_name: device_name(device),
            frames: rx,
        },
    ))
}

fn open_microphone(
    host: &cpal::Host,
    device_id: Option<&str>,
    sink: Arc<dyn RecorderSink>,
) -> Result<(cpal::Stream, OpenedSource)> {
    let selected = device_id.and_then(|id| {
        let parsed = id.parse::<cpal::DeviceId>().ok()?;
        host.device_by_id(&parsed)
    });
    if device_id.is_some() && selected.is_none() {
        sink.on_error("選択したマイクが見つからないため、既定のマイクで録音します".to_string());
    }
    let device = selected
        .or_else(|| host.default_input_device())
        .ok_or_else(|| CoreError::Audio("マイクが見つかりません。接続と Windows のプライバシー設定（マイクへのアクセス）を確認してください".into()))?;
    let config = device
        .default_input_config()
        .map_err(|e| audio_error("マイクの設定を取得できません", e))?;
    open_device(&device, config, Speaker::Me, sink)
}

#[cfg(windows)]
fn open_system_loopback(
    host: &cpal::Host,
    sink: Arc<dyn RecorderSink>,
) -> Result<(cpal::Stream, OpenedSource)> {
    let device = host
        .default_output_device()
        .ok_or_else(|| CoreError::Audio("音声の出力デバイスが見つかりません".into()))?;
    let config = device
        .default_output_config()
        .map_err(|e| audio_error("出力デバイスの設定を取得できません", e))?;
    open_device(&device, config, Speaker::Them, sink)
}

#[cfg(not(windows))]
fn open_system_loopback(
    _host: &cpal::Host,
    _sink: Arc<dyn RecorderSink>,
) -> Result<(cpal::Stream, OpenedSource)> {
    Err(CoreError::Audio(
        "相手の声（システム音声）の取り込みは Windows のみ対応しています".into(),
    ))
}

/// マイクと（指定があれば）システム音声を開く。マイクが開けなければエラー、
/// システム音声だけ失敗した場合はマイクのみで続ける（`sink` に知らせる）。
///
/// 返す `cpal::Stream` を破棄すると取り込みが止まり、`frames` の送信側も閉じる。
pub fn open_sources(
    mic_device_id: Option<&str>,
    capture_system_audio: bool,
    sink: Arc<dyn RecorderSink>,
) -> Result<(Vec<cpal::Stream>, Vec<OpenedSource>)> {
    let host = cpal::default_host();
    let mut streams = Vec::new();
    let mut sources = Vec::new();

    let (stream, source) = open_microphone(&host, mic_device_id, sink.clone())?;
    streams.push(stream);
    sources.push(source);

    if capture_system_audio {
        match open_system_loopback(&host, sink.clone()) {
            Ok((stream, source)) => {
                streams.push(stream);
                sources.push(source);
            }
            Err(err) => sink.on_error(format!("相手の声は取り込まず、マイクだけで録音します（{err}）")),
        }
    }
    Ok((streams, sources))
}
