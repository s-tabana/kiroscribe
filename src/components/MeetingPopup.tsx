import { useEffect, useState } from "react";
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
  const [error, setError] = useState<string | null>(null);
  const [status] = useRecordingStatus(backend);
  const now = useNow(payload?.kind === "recording");

  useEffect(() => {
    let active = true;
    void backend.getPopupState().then((p) => active && p && setPayload(p));
    const off = backend.onPopupUpdate((p) => setPayload(p));
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

  if (!payload) return <div className="popup" />;

  const stop = async () => {
    if (payload.kind !== "recording") return;
    setBusy(true);
    setPayload({ ...payload, kind: "finishing" });
    try {
      await backend.stopRecording();
      setPayload({ ...payload, kind: "stopped" });
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  const recordingThis =
    payload.kind === "recording" && status.noteId === payload.noteId && status.state !== "idle";

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
            {recordingThis && <strong role="timer">{formatElapsed(elapsedMs(status, now))}</strong>}
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
          会議が終わりました。文字起こしを仕上げています…
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
