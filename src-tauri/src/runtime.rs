//! このアプリが使う Tauri のランタイム。
//!
//! ふだんは Wry（実際のウィンドウとウェブビュー）。単体テストのビルドでだけ `tauri::test` の模擬のランタイムにして、
//! ウィンドウを開かずに、コマンドの呼び出し（画面と同じ IPC の経路）とイベントを確かめられるようにする。

#[cfg(not(test))]
pub type Runtime = tauri::Wry;
#[cfg(test)]
pub type Runtime = tauri::test::MockRuntime;

pub type AppHandle = tauri::AppHandle<Runtime>;
