//! 長い録音で、会議が続いているかを確かめる。終わった様子なら「会議は終わりましたか？」と聞き、
//! 答えがなければ録音を止めて保存する（判定は kiroscribe_core::recording_check）。

use std::time::Duration;

use kiroscribe_core::meetings::MeetingApp;
use kiroscribe_core::recording_check::CheckAction;
use kiroscribe_core::time::now_ms;
use tauri::Manager;

use crate::background::{
    notify, popup_note_title, popup_recording_stopped, show_popup, withdraw_confirm_end, PopupPayload,
};
use crate::error::CmdResult;
use crate::meeting_watch::MeetingState;
use crate::recording::{finish_stop, Answer, CheckStep, RecorderState};
use crate::runtime::AppHandle;

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
    popup_note_title(app, note_id).unwrap_or_default()
}

/// 1 回ぶん進める。`now_ms` は、いまの時刻（テストでは先の時刻を渡す）。
/// 確認を出す・取り下げる・止め始めるのは、録音のロックの中で行う（その間に止めた・答えた録音を取り違えないように）。
pub(crate) async fn tick(app: &AppHandle, now_ms: i64) {
    let meeting_detected = app.state::<MeetingState>().current_meeting().is_some();
    let on_step = |step: &CheckStep| match step.action {
        CheckAction::Ask(_) => ask(app, step),
        CheckAction::Stop(_) => show_popup(
            app,
            PopupPayload::Finishing {
                app: label(step.auto).into(),
                note_id: step.note_id.clone(),
                title: note_title(app, &step.note_id),
            },
        ),
        CheckAction::Withdraw => withdraw_confirm_end(app, &step.note_id, step.auto.map(MeetingApp::label)),
    };
    let Some((step, taken)) = app.state::<RecorderState>().check(meeting_detected, now_ms, on_step).await else {
        return;
    };
    crate::debug::info(
        app,
        "recording.check",
        serde_json::json!({
            "noteId": step.note_id,
            "auto": step.auto,
            "action": format!("{:?}", step.action),
            "meetingDetected": meeting_detected,
        }),
    );
    if let Some(taken) = taken {
        finish_stop(app, taken).await;
        stopped_unanswered(app, &step);
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

/// 答えがなく止めた録音の保存が終わった: ポップアップと Windows の通知で知らせる。
fn stopped_unanswered(app: &AppHandle, step: &CheckStep) {
    // 録音のあいだにタイトルを直していたら、いまのタイトルを出す
    let title = note_title(app, &step.note_id);
    notify(app, &unanswered_message(&title));
    show_popup(
        app,
        PopupPayload::StoppedUnanswered {
            app: label(step.auto).into(),
            note_id: step.note_id.clone(),
            title,
        },
    );
}

/// 答えがなく録音を止めたときの知らせ。
pub fn unanswered_message(title: &str) -> String {
    format!("答えがなかったため、録音を停止しました。「{title}」に保存しています。")
}

/// 会議が終わったかの確認に答える。`deadline` は、答えた確認の止める時刻（どの確認への答えかを表す）。
/// `keep` なら録音を続け（次に確かめるのは 30 分後）、そうでなければ止めて保存する。
/// その確認がもう有効でなければ（答えがなく止めた・ほかで止めた・取り下げた・同じノートで録音し直した）、録音には触れない。
#[tauri::command]
pub async fn answer_recording_check(app: AppHandle, note_id: String, deadline: i64, keep: bool) -> CmdResult<()> {
    let recorder = app.state::<RecorderState>();
    let answer = if keep {
        let withdraw = |auto: Option<MeetingApp>| withdraw_confirm_end(&app, &note_id, auto.map(MeetingApp::label));
        crate::debug::info(&app, "recording.check.answered", serde_json::json!({ "noteId": note_id, "keep": true }));
        match recorder.keep(&note_id, deadline, now_ms(), withdraw).await {
            Answer::Applied(()) => return Ok(()),
            Answer::Stale { note_busy } => note_busy,
        }
    } else {
        crate::debug::info(&app, "recording.check.answered", serde_json::json!({ "noteId": note_id, "keep": false }));
        match recorder.take_for_answer(&note_id, deadline).await {
            Answer::Applied(taken) => {
                let stopped = finish_stop(&app, taken).await;
                // 保存が終わってから、ポップアップを停止の表示にする
                popup_recording_stopped(&app, &stopped.note_id);
                return Ok(());
            }
            Answer::Stale { note_busy } => note_busy,
        }
    };
    // そのノートの録音が、もう終わって保存されている: 残っている確認の表示を、停止の表示にする。
    // まだ録音中・仕上げ中なら、止めた側（または次の確認）が表示を変えるので、ここでは触れない
    if !answer {
        popup_recording_stopped(&app, &note_id);
    }
    Ok(())
}
