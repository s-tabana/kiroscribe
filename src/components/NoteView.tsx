import { useCallback, useEffect, useRef, useState } from "react";
import { errorMessage, useBackend } from "../lib/backend";
import { currentEnhanceRun, startEnhanceRun } from "../lib/enhanceRuns";
import type { Note, NotePatch, RecordingStatus, Segment } from "../lib/types";
import { fullDateLabel } from "../lib/dates";
import { UNTITLED } from "../lib/notes";
import { RecordingBar } from "./RecordingBar";
import { SidePanel, type PanelTab } from "./SidePanel";
import { MinutesView } from "./MinutesView";
import { ShareMenu } from "./ShareMenu";
import { TrashIcon } from "./icons";

const SAVE_DELAY_MS = 500;

type SaveState = "idle" | "saving" | "saved" | "error";
type View = "minutes" | "notes";

export interface RecordingControls {
  status: RecordingStatus;
  busy: boolean;
  start(noteId: string): void;
  pause(): void;
  resume(): void;
  stop(): void;
}

interface NoteViewProps {
  noteId: string;
  /** 新規作成直後はタイトルにフォーカスする */
  autoFocusTitle?: boolean;
  recording: RecordingControls;
  onChanged(note: Note): void;
  onDelete(id: string): void;
  onOpenNote(id: string): void;
  onError(message: string): void;
  /** 開いたらすぐ議事録の作成を始める（会議終了のポップアップから） */
  autoEnhance?: boolean;
  onAutoEnhanceHandled?(): void;
}

/**
 * 1 つのノートの編集画面。親で `key={noteId}` を付けて、ノートごとに作り直す前提。
 * 入力は少し待ってからまとめて保存し、画面を離れるときに未保存分を書き出す。
 */
export function NoteView({
  noteId,
  autoFocusTitle,
  recording,
  onChanged,
  onDelete,
  onOpenNote,
  onError,
  autoEnhance,
  onAutoEnhanceHandled,
}: NoteViewProps) {
  const backend = useBackend();
  const [note, setNote] = useState<Note | null>(null);
  const [title, setTitle] = useState("");
  const [userNotes, setUserNotes] = useState("");
  const [minutes, setMinutes] = useState<string | null>(null);
  const [view, setView] = useState<View>("notes");
  const [editingMinutes, setEditingMinutes] = useState(false);
  const [streaming, setStreaming] = useState<string | null>(null);
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const [error, setError] = useState<string | null>(null);
  const [segments, setSegments] = useState<Segment[]>([]);
  const [panel, setPanel] = useState<PanelTab | null>(null);

  const pending = useRef<NotePatch>({});
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const bodyRef = useRef<HTMLTextAreaElement>(null);
  const onChangedRef = useRef(onChanged);
  onChangedRef.current = onChanged;
  const minutesRef = useRef(minutes);
  minutesRef.current = minutes;

  const status = recording.status;
  const recordingHere = status.noteId === noteId && status.state !== "idle";
  const live = recordingHere && status.state !== "finishing";

  useEffect(() => {
    let cancelled = false;
    backend
      .getNote(noteId)
      .then((loaded) => {
        if (cancelled) return;
        setNote(loaded);
        setTitle(loaded.title);
        setUserNotes(loaded.userNotes);
        // 中身のない議事録は「議事録なし」として扱う
        const loadedMinutes = loaded.enhancedNotes?.trim() ? loaded.enhancedNotes : null;
        setMinutes(loadedMinutes);
        // 議事録の作成中に戻ってきたときは、議事録（作成中）の表示のままにする
        const enhancing = currentEnhanceRun(backend, noteId) !== null;
        setView(loadedMinutes !== null || enhancing ? "minutes" : "notes");
      })
      .catch((err) => !cancelled && setError(errorMessage(err)));
    return () => {
      cancelled = true;
    };
  }, [backend, noteId]);

  // 文字起こし: 保存済みのものを読み込み、録音中は届いたものを追加する
  useEffect(() => {
    let cancelled = false;
    const unsubscribe = backend.onSegment((segment) => {
      if (segment.noteId !== noteId) return;
      setSegments((prev) =>
        prev.some((s) => s.id === segment.id)
          ? prev
          : [...prev, segment].sort((a, b) => a.startedAt - b.startedAt || a.id - b.id),
      );
    });
    void backend.listSegments(noteId).then((loaded) => {
      if (cancelled) return;
      setSegments((prev) => {
        const ids = new Set(loaded.map((s) => s.id));
        return [...loaded, ...prev.filter((s) => !ids.has(s.id))];
      });
    });
    return () => {
      cancelled = true;
      unsubscribe();
    };
  }, [backend, noteId]);

  // 議事録の生成中は、届いた文字を表示に足していく（この画面が生成中の表示をしているときだけ）
  useEffect(
    () =>
      backend.onEnhanceDelta((e) => {
        if (e.noteId === noteId) setStreaming((prev) => (prev === null ? prev : prev + e.delta));
      }),
    [backend, noteId],
  );

  // 議事録の作成中に別のノートへ移って、戻ってきたとき: 作成は裏で続いているので、それまでに届いた文章とともに
  // 作成中の表示に戻し、終わったら結果を反映する（終わってから戻ったときは、上の読み込みで完成した議事録が出る）
  useEffect(() => {
    const run = currentEnhanceRun(backend, noteId);
    if (!run) return;
    let active = true;
    setStreaming(run.text);
    setView("minutes");
    run.done
      .then(
        (saved) => {
          if (!active) return;
          setNote(saved);
          setMinutes(saved.enhancedNotes);
          setTitle(saved.title);
        },
        // 失敗の知らせは、作成を始めたときの処理（generate）が出す
        () => {
          if (active && minutesRef.current === null) setView("notes");
        },
      )
      .finally(() => {
        if (active) setStreaming(null);
      });
    return () => {
      active = false;
    };
  }, [backend, noteId]);

  // このノートで録音が始まったら文字起こしを開く
  useEffect(() => {
    if (live) setPanel("transcript");
  }, [live]);

  const flush = useCallback(async () => {
    clearTimeout(timer.current);
    const patch = pending.current;
    pending.current = {};
    if (Object.keys(patch).length === 0) return;
    try {
      const saved = await backend.updateNote(noteId, patch);
      onChangedRef.current(saved);
      setSaveState("saved");
      setError(null);
    } catch (err) {
      setSaveState("error");
      setError(errorMessage(err));
    }
  }, [backend, noteId]);

  // ノートを切り替える・画面を閉じるときに未保存分を保存する
  useEffect(() => () => void flush(), [flush]);

  const queue = (patch: NotePatch) => {
    pending.current = { ...pending.current, ...patch };
    setSaveState("saving");
    clearTimeout(timer.current);
    timer.current = setTimeout(() => void flush(), SAVE_DELAY_MS);
  };

  const generate = async () => {
    if (streaming !== null) return;
    if (minutes !== null) {
      const ok = await backend.confirm(
        "議事録を作り直しますか？\n今の議事録（手で直した部分も含む）は新しい内容に置き換わります。",
        "議事録の作り直し",
      );
      if (!ok) return;
    }
    await flush(); // 最新のメモを使う
    setEditingMinutes(false);
    setView("minutes");
    setStreaming("");
    try {
      // 作成はこの画面を離れても続く。結果は、戻ってきた画面からも受け取れるようにしておく
      const saved = await startEnhanceRun(backend, noteId).done;
      setNote(saved);
      setMinutes(saved.enhancedNotes);
      setTitle(saved.title);
      onChangedRef.current(saved);
    } catch (err) {
      const message = errorMessage(err);
      if (message !== "中止しました") onError(message);
      if (minutes === null) setView("notes");
    } finally {
      setStreaming(null);
    }
  };

  // 会議終了のポップアップから「議事録を作成」が押されたとき
  const autoEnhanceStarted = useRef(false);
  useEffect(() => {
    if (!autoEnhance || !note || autoEnhanceStarted.current) return;
    autoEnhanceStarted.current = true;
    onAutoEnhanceHandled?.();
    void generate();
    // generate は描画ごとに作り直されるため依存に含めない
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autoEnhance, note]);

  if (!note) {
    return (
      <div className="note-view">
        {error ? <p className="error-text">{error}</p> : <p className="muted">読み込み中…</p>}
      </div>
    );
  }

  const hasMinutes = minutes !== null || streaming !== null;
  const canEnhance = userNotes.trim().length > 0 || segments.length > 0;

  return (
    <div className="note-layout">
      <div className="note-scroll">
        <article className="note-view">
          <header className="note-meta">
            <time dateTime={new Date(note.createdAt).toISOString()}>
              {fullDateLabel(note.createdAt)}
            </time>
            <span className={`save-state save-${saveState}`} role="status">
              {saveState === "saving" && "保存中…"}
              {saveState === "saved" && "保存しました"}
              {saveState === "error" && "保存できませんでした"}
            </span>
            <ShareMenu
              noteId={note.id}
              hasMinutes={!!minutes?.trim()}
              hasNotes={!!userNotes.trim()}
              hasTranscript={segments.length > 0}
              beforeExport={flush}
            />
            <button
              type="button"
              className="icon-button"
              onClick={() => onDelete(note.id)}
              aria-label="ノートを削除"
              title="ノートを削除"
            >
              <TrashIcon size={17} />
            </button>
          </header>

          <input
            className="note-title"
            value={title}
            placeholder={UNTITLED}
            aria-label="タイトル"
            autoFocus={autoFocusTitle}
            onChange={(e) => {
              setTitle(e.target.value);
              queue({ title: e.target.value });
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.nativeEvent.isComposing) {
                e.preventDefault();
                bodyRef.current?.focus();
              }
            }}
          />

          {hasMinutes && (
            <div className="view-switch" role="tablist" aria-label="表示の切り替え">
              <button
                type="button"
                role="tab"
                aria-selected={view === "minutes"}
                onClick={() => setView("minutes")}
              >
                議事録
              </button>
              <button
                type="button"
                role="tab"
                aria-selected={view === "notes"}
                onClick={() => setView("notes")}
              >
                自分のメモ
              </button>
            </div>
          )}

          {error && <p className="error-text">{error}</p>}

          {view === "minutes" && hasMinutes ? (
            <MinutesView
              minutes={minutes ?? ""}
              userNotes={userNotes}
              editing={editingMinutes}
              streaming={streaming}
              onEdit={(value) => {
                setMinutes(value);
                queue({ enhancedNotes: value });
              }}
              onToggleEdit={() => {
                if (editingMinutes) {
                  void flush();
                  // 空（空白だけ）にして編集を終えたら「議事録なし」に戻す（保存の側でも議事録を消す）
                  if (!minutes?.trim()) {
                    setMinutes(null);
                    setView("notes");
                  }
                }
                setEditingMinutes((e) => !e);
              }}
              onRegenerate={() => void generate()}
              onCancel={() => void backend.cancelAi(noteId, "enhance")}
            />
          ) : (
            <textarea
              ref={bodyRef}
              className="note-body"
              value={userNotes}
              aria-label="自分のメモ"
              placeholder={
                "会議中に気づいたことを走り書きしてください。\n" +
                "整った文章でなくて大丈夫です。あとで AI が文字起こしと合わせて議事録に仕上げます。"
              }
              onChange={(e) => {
                setUserNotes(e.target.value);
                queue({ userNotes: e.target.value });
              }}
              onBlur={() => void flush()}
            />
          )}
        </article>

        <div className="bar-dock">
          <RecordingBar
            noteId={noteId}
            status={status}
            busy={recording.busy}
            segmentCount={segments.length}
            panel={panel}
            enhance={
              minutes === null && canEnhance
                ? { running: streaming !== null, onClick: () => void generate() }
                : null
            }
            onStart={() => recording.start(noteId)}
            onPause={recording.pause}
            onResume={recording.resume}
            onStop={recording.stop}
            onTogglePanel={(tab) => setPanel((current) => (current === tab ? null : tab))}
            onOpenRecordingNote={() => status.noteId && onOpenNote(status.noteId)}
          />
        </div>
      </div>

      {panel && (
        <SidePanel
          noteId={noteId}
          tab={panel}
          segments={segments}
          live={live}
          sources={status.sources}
          onTabChange={setPanel}
          onClose={() => setPanel(null)}
          onError={onError}
        />
      )}
    </div>
  );
}
