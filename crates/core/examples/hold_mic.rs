//! マイクを開いたまま、指定の秒数だけ待つ（会議アプリがマイクを使っている状態を、E2E テストで再現するための道具）。
//!
//! ```text
//! cargo build -p kiroscribe-core --example hold_mic
//! ```
//!
//! できた実行ファイルを「Zoom.exe」などの名前に写して起動すると、Windows の録音デバイスに、その名前のプロセスの
//! 録音のセッションができる。Kiroscribe の会議の検出は、このオーディオのセッションを読む。
//! 引数は秒数（既定は 60 秒）。終わるか、止められると、マイクを閉じる。
//!
//! 秒数のあとに `--pause <秒>` を付けると、使い終わったあと、ストリームを止めて（閉じずに）その秒数だけ待つ。
//! `--drop <秒>` を付けると、ストリームを閉じてから、その秒数だけプロセスを残す。
//! ミュートや通話の終了で、オーディオのセッションの状態（Inactive・Expired）がどう変わるかを確かめるために使う。
//!
//! `--play` を付けると、マイクを使っている間、既定の再生のデバイスに小さな音（440 Hz）も流す
//! （会議の検出の確認用の道具が、再生のセッションと音の大きさを読めることを確かめるため）。
//!
//! `--window <クラス名>` を付けると、マイクを使っている間、そのクラス名の窓を出す（Zoom の会議の窓の
//! ZPContentViewWndClass など。判定表の窓の印を確かめるため。Windows だけ）。`--button <名前>` を付けると、その窓に
//! その名前のボタンを置く（Discord の「Disconnect」など。判定表の部品の印を確かめるため）。

use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seconds: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(60);
    let after = |flag: &str| -> Option<u64> {
        let at = args.iter().position(|arg| arg == flag)?;
        args.get(at + 1)?.parse().ok()
    };
    let host = cpal::default_host();
    let device = host.default_input_device().expect("マイクが見つかりません");
    let config = device.default_input_config().expect("マイクの設定を取得できません");
    let format = config.sample_format();
    let stream = device
        .build_input_stream_raw(
            config.into(),
            format,
            |_data: &cpal::Data, _info: &cpal::InputCallbackInfo| {},
            |err| eprintln!("マイクでエラーが発生しました: {err}"),
            None,
        )
        .expect("マイクを開けません");
    stream.play().expect("マイクの取り込みを始められません");
    let tone = args.iter().any(|arg| arg == "--play").then(|| play_tone(&host));
    let value = |flag: &str| -> Option<String> {
        let at = args.iter().position(|arg| arg == flag)?;
        args.get(at + 1).cloned()
    };
    println!("マイクを使っています（{seconds} 秒）");
    match value("--window") {
        Some(class) => window::show_for(&class, value("--button").as_deref(), Duration::from_secs(seconds)),
        None => std::thread::sleep(Duration::from_secs(seconds)),
    }
    drop(tone);
    if let Some(wait) = after("--pause") {
        stream.pause().expect("マイクの取り込みを止められません");
        println!("ストリームを止めました（閉じずに {wait} 秒待ちます）");
        std::thread::sleep(Duration::from_secs(wait));
    } else if let Some(wait) = after("--drop") {
        drop(stream);
        println!("ストリームを閉じました（{wait} 秒待ちます）");
        std::thread::sleep(Duration::from_secs(wait));
    }
}

/// 既定の再生のデバイスに、小さな音を流し続ける（戻り値を捨てると止まる）。
fn play_tone(host: &cpal::Host) -> cpal::Stream {
    let device = host.default_output_device().expect("再生のデバイスが見つかりません");
    let config = device.default_output_config().expect("再生のデバイスの設定を取得できません");
    let format = config.sample_format();
    let config: cpal::StreamConfig = config.into();
    let channels = config.channels.max(1) as usize;
    let step = 440.0 * std::f32::consts::TAU / config.sample_rate as f32;
    let mut phase = 0.0f32;
    let mut next = move || {
        phase = (phase + step) % std::f32::consts::TAU;
        0.1 * phase.sin()
    };
    let stream = device
        .build_output_stream_raw(
            config,
            format,
            move |data: &mut cpal::Data, _info: &cpal::OutputCallbackInfo| {
                if let Some(samples) = data.as_slice_mut::<f32>() {
                    for frame in samples.chunks_mut(channels) {
                        frame.fill(next());
                    }
                } else if let Some(samples) = data.as_slice_mut::<i16>() {
                    for frame in samples.chunks_mut(channels) {
                        frame.fill((next() * i16::MAX as f32) as i16);
                    }
                } else {
                    data.bytes_mut().fill(0);
                }
            },
            |err| eprintln!("再生でエラーが発生しました: {err}"),
            None,
        )
        .expect("再生のデバイスを開けません");
    stream.play().expect("音を流し始められません");
    println!("既定の再生のデバイスに音を流しています");
    stream
}

/// 会議の窓の代わりの窓（Windows だけ）。
mod window {
    use std::time::Duration;

    /// `class` のクラス名の窓を出し（`button` があれば、その名前のボタンを置く）、`duration` のあいだ窓のメッセージを処理する
    /// （UI Automation がボタンの名前を読むには、窓のスレッドがメッセージを処理している必要がある）。
    #[cfg(windows)]
    pub fn show_for(class: &str, button: Option<&str>, duration: Duration) {
        use std::time::Instant;
        use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
        use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DispatchMessageW, MsgWaitForMultipleObjects, PeekMessageW, RegisterClassW,
            ShowWindow, TranslateMessage, BS_PUSHBUTTON, MSG, PM_REMOVE, QS_ALLINPUT, SW_SHOWNOACTIVATE, WNDCLASSW,
            WS_CHILD, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
        };
        unsafe extern "system" fn procedure(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        let wide = |text: &str| text.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let class_name = wide(class);
        let title = wide("hold_mic");
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let definition = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                lpszClassName: class_name.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassW(&definition);
            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                100,
                100,
                480,
                320,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                eprintln!("窓を作れませんでした");
                std::thread::sleep(duration);
                return;
            }
            if let Some(name) = button {
                let button_class = wide("BUTTON");
                let label = wide(name);
                CreateWindowExW(
                    0,
                    button_class.as_ptr(),
                    label.as_ptr(),
                    WS_CHILD | WS_VISIBLE | BS_PUSHBUTTON as u32,
                    20,
                    20,
                    200,
                    40,
                    hwnd,
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null(),
                );
            }
            // 起動したプログラムが窓を隠す指定（windowsHide）をしていても、最初の ShowWindow だけに効くので、2 回呼ぶ
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            println!("窓（クラス名 {class}）を出しています");
            let deadline = Instant::now() + duration;
            let mut message: MSG = std::mem::zeroed();
            while let Some(left) = deadline.checked_duration_since(Instant::now()).filter(|left| !left.is_zero()) {
                MsgWaitForMultipleObjects(0, std::ptr::null(), 0, left.as_millis().min(u32::MAX as u128) as u32, QS_ALLINPUT);
                while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
    }

    #[cfg(not(windows))]
    pub fn show_for(_class: &str, _button: Option<&str>, duration: Duration) {
        eprintln!("窓を出せるのは Windows だけです");
        std::thread::sleep(duration);
    }
}
