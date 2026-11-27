//! コマンドの層の結合テスト: 画面と同じ経路（IPC）でコマンドを呼び、録音・AI・会議の検出・更新などの
//! 振る舞いを、模擬の音声と模擬サーバーを相手に確かめる。土台は `fixture`（テストは 1 件ずつ順に動く）。

mod ai;
mod debug_log;
mod log_submit;
mod meeting;
mod misc;
mod recording;
mod recording_check;
mod startup;
mod update;
