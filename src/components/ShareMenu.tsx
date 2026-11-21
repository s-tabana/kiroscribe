import { useEffect, useRef, useState } from "react";
import { errorMessage, useBackend } from "../lib/backend";
import type { ExportOptions } from "../lib/types";
import { CopyIcon, DownloadIcon, ShareIcon } from "./icons";

interface ShareMenuProps {
  noteId: string;
  hasMinutes: boolean;
  hasNotes: boolean;
  hasTranscript: boolean;
  /** 書き出す前に未保存の入力を保存する */
  beforeExport(): Promise<void>;
}

/** 議事録を Markdown でコピー・保存するメニュー */
export function ShareMenu({ noteId, hasMinutes, hasNotes, hasTranscript, beforeExport }: ShareMenuProps) {
  const backend = useBackend();
  const [open, setOpen] = useState(false);
  const [options, setOptions] = useState<ExportOptions>({
    minutes: true,
    notes: !hasMinutes,
    transcript: false,
  });
  const [message, setMessage] = useState<{ kind: "ok" | "error"; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  // 開くたびに既定値へ（議事録があれば議事録だけ、なければメモ）
  useEffect(() => {
    if (!open) return;
    setOptions({ minutes: true, notes: !hasMinutes, transcript: false });
    setMessage(null);
  }, [open, hasMinutes]);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [open]);

  // 実際に書き出す内容（存在しないものは含めない）
  const effective: ExportOptions = {
    minutes: options.minutes && hasMinutes,
    notes: options.notes && hasNotes,
    transcript: options.transcript && hasTranscript,
  };
  const nothingSelected = !effective.minutes && !effective.notes && !effective.transcript;

  const run = async (action: "copy" | "save") => {
    setBusy(true);
    setMessage(null);
    try {
      await beforeExport();
      const exported = await backend.exportMarkdown(noteId, effective);
      if (action === "copy") {
        await backend.copyText(exported.markdown);
        setMessage({ kind: "ok", text: "Markdown をコピーしました" });
      } else {
        const path = await backend.saveMarkdown(noteId, effective, exported.fileName);
        if (path) setMessage({ kind: "ok", text: `保存しました: ${path}` });
      }
    } catch (err) {
      setMessage({ kind: "error", text: errorMessage(err) });
    } finally {
      setBusy(false);
    }
  };

  const toggle = (key: keyof ExportOptions) => setOptions((o) => ({ ...o, [key]: !o[key] }));

  return (
    <div className="share" ref={rootRef}>
      <button
        type="button"
        className="button button-ghost share-trigger"
        aria-expanded={open}
        aria-haspopup="dialog"
        onClick={() => setOpen((o) => !o)}
      >
        <ShareIcon size={15} />
        共有
      </button>
      {open && (
        <div className="share-popover" role="dialog" aria-label="共有">
          <p className="share-heading">含める内容</p>
          <label className="check">
            <input
              type="checkbox"
              checked={effective.minutes}
              disabled={!hasMinutes}
              onChange={() => toggle("minutes")}
            />
            <span>
              議事録
              {!hasMinutes && <small>まだ作成していません</small>}
            </span>
          </label>
          <label className="check">
            <input
              type="checkbox"
              checked={effective.notes}
              disabled={!hasNotes}
              onChange={() => toggle("notes")}
            />
            <span>自分のメモ</span>
          </label>
          <label className="check">
            <input
              type="checkbox"
              checked={effective.transcript}
              disabled={!hasTranscript}
              onChange={() => toggle("transcript")}
            />
            <span>文字起こし</span>
          </label>
          <div className="share-actions">
            <button
              type="button"
              className="button button-primary"
              disabled={busy || nothingSelected}
              onClick={() => void run("copy")}
            >
              <CopyIcon size={15} />
              Markdown をコピー
            </button>
            <button
              type="button"
              className="button"
              disabled={busy || nothingSelected}
              onClick={() => void run("save")}
            >
              <DownloadIcon size={15} />
              ファイルに保存…
            </button>
          </div>
          {message && (
            <p className={message.kind === "ok" ? "ok-text share-message" : "error-text share-message"} role="status">
              {message.text}
            </p>
          )}
        </div>
      )}
    </div>
  );
}
