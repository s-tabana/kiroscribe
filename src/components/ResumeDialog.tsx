import { useEffect, useRef } from "react";

export type ResumeChoice = "continue" | "newNote";

interface ResumeDialogProps {
  /** 選んだら呼ぶ。閉じた・取り消したときは null */
  onClose(choice: ResumeChoice | null): void;
}

/** 議事録のあるノートで「録音を再開」を押したときに、同じ会議の続きか、新しいノートにするかを聞く */
export function ResumeDialog({ onClose }: ResumeDialogProps) {
  const primaryRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose(null);
    window.addEventListener("keydown", onKey);
    primaryRef.current?.focus();
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose(null)}>
      <div
        className="modal resume-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="resume-title"
        aria-describedby="resume-body"
      >
        <header className="modal-header">
          <h2 id="resume-title">録音の再開</h2>
        </header>
        <div className="modal-body" id="resume-body">
          <p>このノートには議事録があります。同じ会議の続きとして録音しますか？</p>
          <p className="muted">
            別の会議なら、新しいノートで録音してください。続きとして録音すると、文字起こしに区切りを入れ、議事録は録音の回ごとに分けて作ります。
          </p>
        </div>
        <footer className="modal-footer">
          <button type="button" className="button" onClick={() => onClose(null)}>
            取り消す
          </button>
          <button type="button" className="button" onClick={() => onClose("newNote")}>
            新しいノートで録音
          </button>
          <button
            type="button"
            className="button button-primary"
            ref={primaryRef}
            onClick={() => onClose("continue")}
          >
            続きとして録音
          </button>
        </footer>
      </div>
    </div>
  );
}
