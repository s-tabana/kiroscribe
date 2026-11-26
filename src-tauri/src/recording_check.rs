//! 長い録音で、会議が続いているかを確かめる。終わった様子なら「会議は終わりましたか？」と聞き、
//! 答えがなければ録音を止めて保存する（判定は kiroscribe_core::recording_check）。

use std::time::Duration;

use kiroscribe_core::meetings::MeetingApp;
use kiroscribe_core::recording_check::CheckAction;
use kiroscribe_core::time::now_ms;
use tauri::Manager;

use crate::background::{notify, popup_recording_stopped, show_popup, withdraw_confirm_end, PopupPayload};
use crate::error::CmdResult;
use crate::meeting_watch::MeetingState;
use crate::recording::{stop_active, CheckStep, RecorderState};
use crate::runtime::AppHandle;
use crate::state::AppState;

/// 確かめる処理を進める間隔（確かめる間隔・答えを待つ時間は、kiroscribe_core::recording_check の決まり）
const TICK: Duration = Duration::from_secs(5);

/// 手動で始めた録音のときに、ポップアップの見出しに出す名前
const MANUAL_LABEL: &str = "Kiroscribe";

/// 確かめる処理を、アプリが動いている間ずっと進める。
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(TICK).await;
            tick(&app, now_ms()).await;
        }
    });
}

fn label(auto: Option<MeetingApp>) -> &'static str {
    auto.map_or(MANUAL_LABEL, MeetingApp::label)
}

fn note_title(app: &AppHandle, note_id: &str) -> String {
    app.state::<AppState>()
        .db()
        .get_note(note_id)
        .map(|note| note.title)
        .unwrap_or_default()
}

/// 1 回ぶん進める。`now_ms` は、いまの時刻（テストでは先の時刻を渡す）。
pub(crate) async fn tick(app: &AppHandle, now_ms: i64) {
    let meeting_detected = app.state::<MeetingState>().current_meeting().is_some();
    let Some(step) = app.state::<RecorderState>().check(meeting_detected, now_ms).await else {
        return;
    };
    match step.action {
        CheckAction::Ask(_) => ask(app, &step),
        CheckAction::Stop(_) => stop_unanswered(app, &step).await,
        CheckAction::Withdraw => withdraw_confirm_end(app, &step.note_id, step.auto.map(MeetingApp::label)),
    }
}

fn ask(app: &AppHandle, step: &CheckStep) {
    show_popup(
        app,
        PopupPayload::ConfirmEnd {
            app: label(step.auto).into(),
            note_id: step.note_id.clone(),
            title: note_title(app, &step.note_id),
            deadline: step.deadline.unwrap_or_default(),
        },
    );
}

/// 答えがなかった: 録音を止めて保存し、ポップアップと Windows の通知で知らせる。
async fn stop_unanswered(app: &AppHandle, step: &CheckStep) {
    let label = label(step.auto);
    show_popup(
        app,
        PopupPayload::Finishing {
            app: label.into(),
            note_id: step.note_id.clone(),
            title: note_title(app, &step.note_id),
        },
    );
    if stop_active(app, Some(&step.note_id)).await.is_none() {
        return;
    }
    // 録音のあいだにタイトルを直していたら、いまのタイトルを出す
    let title = note_title(app, &step.note_id);
    notify(app, &unanswered_message(&title));
    show_popup(
        app,
        PopupPayload::StoppedUnanswered {
            app: label.into(),
            note_id: step.note_id.clone(),
            title,
        },
    );
}

/// 答えがなく録音を止めたときの知らせ。
pub fn unanswered_message(title: &str) -> String {
    format!("答えがなかったため、録音を停止しました。「{title}」に保存しています。")
}

/// 会議が終わったかの確認に答える。`keep` なら録音を続け（次に確かめるのは 30 分後）、そうでなければ止めて保存する。
#[tauri::command]
pub async fn answer_recording_check(app: AppHandle, note_id: String, keep: bool) -> CmdResult<()> {
    if keep {
        match app.state::<RecorderState>().keep(&note_id, now_ms()).await {
            Some(auto) => withdraw_confirm_end(&app, &note_id, auto.map(MeetingApp::label)),
            // 答える前に、録音が止まっていた
            None => popup_recording_stopped(&app, &note_id),
        }
        return Ok(());
    }
    stop_active(&app, Some(&note_id)).await;
    // 止めたあと（その録音がもう終わっていたときも）、ポップアップは停止の表示にする
    popup_recording_stopped(&app, &note_id);
    Ok(())
}
