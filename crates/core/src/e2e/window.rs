//! ウィンドウに、OS の決まりどおりに問い合わせる（E2E テスト用）。

/// ウィンドウが OS に答える「利用者が縮められる最小の大きさ」（枠を含む外側の大きさ。物理ピクセル）。
///
/// 利用者がウィンドウの端をドラッグしたとき、Windows はウィンドウにこの問い合わせ（`WM_GETMINMAXINFO`）をして、
/// 答えの大きさ（`ptMinTrackSize`）で止める。プログラムからの大きさの変更には当てはめないので、
/// 「最小の大きさがウィンドウに届いているか」は、同じ問い合わせをして確かめる。
/// 最小の大きさを決めていないウィンドウは、何も書き込まないので (0, 0) が返る。
///
/// ウィンドウを作ったスレッドから呼ぶこと（別のスレッドからだと、相手が応答するまで戻らない）。
#[cfg(windows)]
pub fn min_track_size(hwnd: isize) -> (i32, i32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SendMessageW, MINMAXINFO, WM_GETMINMAXINFO};

    // SAFETY: MINMAXINFO は整数だけの構造体なので、0 で埋めた値は正しい値。
    let mut info: MINMAXINFO = unsafe { std::mem::zeroed() };
    // SAFETY: 渡すポインタは、この呼び出しが戻るまで有効（SendMessageW は、相手が処理し終えるまで戻らない）。
    // ウィンドウがもうないときは、何もせずに 0 を返す。
    unsafe {
        SendMessageW(
            hwnd as windows_sys::Win32::Foundation::HWND,
            WM_GETMINMAXINFO,
            0,
            &mut info as *mut MINMAXINFO as isize,
        );
    }
    (info.ptMinTrackSize.x, info.ptMinTrackSize.y)
}
