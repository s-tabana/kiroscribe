import { useEffect, useRef, useState } from "react";
import { errorMessage, useBackend } from "../lib/backend";
import type { PopupPayload } from "../lib/types";
import { elapsedMs, formatElapsed } from "../lib/elapsed";
import { useNow } from "../hooks/useNow";
import { useRecordingStatus } from "../hooks/useRecordingStatus";
import { CloseIcon, LogoMark, SparklesIcon, StopIcon } from "./icons";

/** 停止を知らせたあと、自動で閉じるまでの時間 */
const AUTO_CLOSE_MS = 30_000;

/**
 * 会議を検出したときに画面右下に出す小さなウィンドウ。
 * 録音の開始・停止を知らせ、ノートを開く・停止する・議事録を作るの操作を受け付ける。
 */
export function MeetingPopup() {
  const backend = useBackend();
  const [payload, setPayload] = useState<PopupPayload | null>(null);
  const [busy, setBusy] = useState(false);
  /** このポップアップの「停止」で止めた（会議が終わって自動で止まったのではない） */
  const [stoppedHere, setStoppedHere] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [status] = useRecordingStatus(backend);
  const now = useNow(payload?.kind === "recording" || payload?.kind === "confirmEnd");

  useEffect(() => {
    let active = true;
    void backend.getPopupState().then((p) => active && p && setPayload(p));
    const off = backend.onPopupUpdate((p) => {
      setPayload(p);
      setStoppedHere(false);
    });
    return () => {
      active = false;
      off();
    };
  }, [backend]);

  // 録音を止めたあとは、しばらくしたら自動で閉じる
  useEffect(() => {
    if (payload?.kind !== "stopped") return;
    const timer = setTimeout(() => void backend.closePopup(), AUTO_CLOSE_MS);
    return () => clearTimeout(timer);
  }, [payload, backend]);

  // 知らせている録音が、このポップアップの外（メインウィンドウの「停止」など）で止まったら、表示を合わせる。
  // 録音中の表示のまま残ると、その「停止」が、あとから始めた別の録音を止めてしまう
  const sawRecording = useRef<string | null>(null);
  useEffect(() => {
    if (!payload || payload.kind === "error" || payload.kind === "stopped" || payload.kind === "stoppedUnanswered") return;
    const mine = status.noteId === payload.noteId;
    const ended = !mine || status.state === "idle";
    if (payload.kind === "recording") {
      if (mine && (status.state === "recording" || status.state === "paused")) {
        sawRecording.current = payload.noteId;
      } else if (mine && status.state === "finishing") {
        setStoppedHere(true);
        setPayload({ ...payload, kind: "finishing" });
      } else if (ended && sawRecording.current === payload.noteId) {
        setPayload({ ...payload, kind: "stopped" });
      }
    } else if (stoppedHere && ended && !busy) {
      // 利用者が止めた録音の仕上げが終わった（会議の終了で止まるときは、本体からの知らせを待つ）
      setPayload({ ...payload, kind: "stopped" });
    }
  }, [payload, status, stoppedHere, busy]);

  if (!payload) return <div className="popup" />;

  const stop = async () => {
    if (payload.kind !== "recording") return;
    setBusy(true);
    try {
      // 止めるのは、このポップアップが知らせている録音だけ（もう終わっていたら、表示を合わせるだけにする）
      const current = await backend.getRecordingStatus();
      if (current.noteId !== payload.noteId || current.state === "idle") {
        setPayload({ ...payload, kind: "stopped" });
        return;
      }
      setStoppedHere(true);
      setPayload({ ...payload, kind: "finishing" });
      await backend.stopRecording();
      // 本体が先に停止を知らせていたら（そのときのタイトルが入っている）、そのままにする
      setPayload((shown) => (shown?.kind === "stopped" ? shown : { ...payload, kind: "stopped" }));
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  /** 会議が終わったかの確認に答える（keep なら録音を続ける） */
  const answer = async (keep: boolean) => {
    if (payload.kind !== "confirmEnd") return;
    const { app, noteId, title } = payload;
    setBusy(true);
    try {
      if (!keep) {
        setStoppedHere(true);
        setPayload({ kind: "finishing", app, noteId, title });
      }
      await backend.answerRecordingCheck(noteId, keep);
      // 止めたときは、本体が停止の表示を知らせる。知らせが先に届いていなければ、ここで停止の表示にする
      if (!keep) setPayload((shown) => (shown?.kind === "stopped" ? shown : { kind: "stopped", app, noteId, title }));
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  const recordingThis = (noteId: string) => status.noteId === noteId && status.state !== "idle";

  return (
    <div className={`popup popup-${payload.kind}`} role="alertdialog" aria-label="会議の自動録音">
      <header className="popup-header" data-tauri-drag-region>
        <LogoMark size={18} />
        <span className="popup-app" data-tauri-drag-region>
          {payload.app}
        </span>
        <button
          type="button"
          className="icon-button"
          onClick={() => void backend.closePopup()}
          aria-label="閉じる"
          title="閉じる（録音は続きます）"
        >
          <CloseIcon size={14} />
        </button>
      </header>

      {payload.kind === "recording" && (
        <>
          <p className="popup-message">
            <span className="rec-dot is-live" aria-hidden="true" />
            自動で録音・文字起こしを始めました
            {recordingThis(payload.noteId) && <strong role="timer">{formatElapsed(elapsedMs(status, now))}</strong>}
          </p>
          <div className="popup-actions">
            <button type="button" className="button" onClick={() => void backend.openNoteFromPopup(payload.noteId, false)}>
              ノートを開く
            </button>
            <button type="button" className="button popup-stop" onClick={() => void stop()} disabled={busy}>
              <StopIcon size={13} />
              停止
            </button>
          </div>
        </>
      )}

      {payload.kind === "finishing" && (
        <p className="popup-message">
          <span className="spinner" aria-hidden="true" />
          {stoppedHere
            ? "録音を停止しました。文字起こしを仕上げています…"
            : "会議が終わりました。文字起こしを仕上げています…"}
        </p>
      )}

      {payload.kind === "stopped" && (
        <>
          <p className="popup-message">録音を停止しました。「{payload.title}」に保存しています。</p>
          <div className="popup-actions">
            <button type="button" className="button" onClick={() => void backend.openNoteFromPopup(payload.noteId, false)}>
              ノートを開く
            </button>
            <button
              type="button"
              className="button button-primary"
              onClick={() => void backend.openNoteFromPopup(payload.noteId, true)}
            >
              <SparklesIcon size={14} />
              議事録を作成
            </button>
          </div>
        </>
      )}

      {payload.kind === "confirmEnd" && (
        <>
          <p className="popup-message">
            <strong>会議は終わりましたか？</strong>
            <span>
              <span role="timer">{formatElapsed(Math.max(0, payload.deadline - now))}</span> 後に止めて保存します
            </span>
          </p>
          <div className="popup-actions">
            <button type="button" className="button" onClick={() => void answer(true)} disabled={busy}>
              続ける
            </button>
            <button type="button" className="button popup-stop" onClick={() => void answer(false)} disabled={busy}>
              <StopIcon size={13} />
              止める
            </button>
          </div>
        </>
      )}

      {payload.kind === "stoppedUnanswered" && (
        <>
          <p className="popup-message">答えがなかったため、録音を停止しました。「{payload.title}」に保存しています。</p>
          <div className="popup-actions">
            <button type="button" className="button" onClick={() => void backend.openNoteFromPopup(payload.noteId, false)}>
              ノートを開く
            </button>
            <button
              type="button"
              className="button button-primary"
              onClick={() => void backend.openNoteFromPopup(payload.noteId, true)}
            >
              <SparklesIcon size={14} />
              議事録を作成
            </button>
          </div>
        </>
      )}

      {payload.kind === "error" && (
        <>
          <p className="popup-message error-text">{payload.message}</p>
          <div className="popup-actions">
            <button type="button" className="button button-primary" onClick={() => void backend.openSettingsFromPopup()}>
              設定を開く
            </button>
          </div>
        </>
      )}

      {error && <p className="popup-message error-text">{error}</p>}
    </div>
  );
}
