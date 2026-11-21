import { useCallback, useEffect, useMemo, useState } from "react";
import { errorMessage, useBackend } from "./lib/backend";
import { toSummary } from "./lib/notes";
import type { Note, NoteSummary, RecordingStatus } from "./lib/types";
import { useRecordingStatus } from "./hooks/useRecordingStatus";
import { useUpdater } from "./hooks/useUpdater";
import { Sidebar } from "./components/Sidebar";
import { NoteView, type RecordingControls } from "./components/NoteView";
import { SettingsDialog } from "./components/SettingsDialog";
import { HelpDialog } from "./components/HelpDialog";
import { CloseIcon, NoteIcon, PlusIcon } from "./components/icons";

interface Notice {
  message: string;
  /** API キー未設定などのときに設定画面へ誘導する */
  openSettings?: boolean;
}

function toNotice(err: unknown): Notice {
  const message = errorMessage(err);
  return { message, openSettings: message.includes("API キー") };
}

export function App() {
  const backend = useBackend();
  const [notes, setNotes] = useState<NoteSummary[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [justCreatedId, setJustCreatedId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [query, setQuery] = useState("");
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);
  const updater = useUpdater(backend);
  const [recordingStatus, setRecordingStatus] = useRecordingStatus(backend);
  const [recordingBusy, setRecordingBusy] = useState(false);

  useEffect(() => {
    backend
      .listNotes()
      .then((list) => {
        setNotes(list);
        setSelectedId(list[0]?.id ?? null);
      })
      .catch((err) => setNotice(toNotice(err)))
      .finally(() => setLoading(false));
  }, [backend]);

  // 録音中に起きた問題（文字起こしの失敗・デバイスのエラーなど）を知らせる
  useEffect(() => backend.onRecordingError((e) => setNotice(toNotice(e.message))), [backend]);

  // 会議の自動検出でノートが作られたら一覧に加える（ウィンドウを閉じてもトレイに常駐し、録音は続く）
  useEffect(
    () =>
      backend.onNoteCreated((note) =>
        setNotes((prev) => (prev.some((n) => n.id === note.id) ? prev : [toSummary(note), ...prev])),
      ),
    [backend],
  );

  // ポップアップから「ノートを開く」「議事録を作成」「設定を開く」
  const [enhanceRequestId, setEnhanceRequestId] = useState<string | null>(null);
  useEffect(() => {
    const offOpen = backend.onOpenNote(({ noteId, enhance }) => {
      setQuery("");
      setSelectedId(noteId);
      setEnhanceRequestId(enhance ? noteId : null);
      // 一覧にまだなければ読み直す
      void backend.listNotes().then(setNotes);
    });
    const offSettings = backend.onOpenSettings(() => setSettingsOpen(true));
    return () => {
      offOpen();
      offSettings();
    };
  }, [backend]);

  const createNote = useCallback(async () => {
    try {
      const note = await backend.createNote();
      setNotes((prev) => [toSummary(note), ...prev]);
      setQuery("");
      setSelectedId(note.id);
      setJustCreatedId(note.id);
    } catch (err) {
      setNotice(toNotice(err));
    }
  }, [backend]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "n") {
        e.preventDefault();
        void createNote();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [createNote]);

  const handleChanged = useCallback((note: Note) => {
    setNotes((prev) => prev.map((s) => (s.id === note.id ? toSummary(note) : s)));
  }, []);

  const deleteNote = async (id: string) => {
    const ok = await backend.confirm(
      "このノートを削除しますか？\nメモ・文字起こし・議事録もすべて削除され、元に戻せません。",
      "ノートの削除",
    );
    if (!ok) return;
    try {
      await backend.deleteNote(id);
      const index = notes.findIndex((n) => n.id === id);
      const next = notes.filter((n) => n.id !== id);
      setNotes(next);
      setSelectedId(next[Math.min(index, next.length - 1)]?.id ?? null);
    } catch (err) {
      setNotice(toNotice(err));
    }
  };

  const recording: RecordingControls = useMemo(() => {
    const run = async (action: () => Promise<RecordingStatus>) => {
      setRecordingBusy(true);
      try {
        setRecordingStatus(await action());
      } catch (err) {
        setNotice(toNotice(err));
      } finally {
        setRecordingBusy(false);
      }
    };
    return {
      status: recordingStatus,
      busy: recordingBusy,
      start: (noteId) => void run(() => backend.startRecording(noteId)),
      pause: () => void run(() => backend.pauseRecording()),
      resume: () => void run(() => backend.resumeRecording()),
      stop: () => void run(() => backend.stopRecording()),
    };
  }, [backend, recordingStatus, recordingBusy, setRecordingStatus]);

  const recordingNoteId = recordingStatus.state === "idle" ? null : recordingStatus.noteId;

  return (
    <div className="app">
      <Sidebar
        notes={notes}
        selectedId={selectedId}
        recordingNoteId={recordingNoteId}
        query={query}
        isPreview={backend.isPreview}
        updater={updater}
        onQueryChange={setQuery}
        onSelect={setSelectedId}
        onCreate={() => void createNote()}
        onOpenSettings={() => setSettingsOpen(true)}
        onOpenHelp={() => setHelpOpen(true)}
      />

      <main className="main">
        {notice && (
          <div className="toast" role="alert">
            <span>{notice.message}</span>
            {notice.openSettings && (
              <button
                type="button"
                className="button"
                onClick={() => {
                  setSettingsOpen(true);
                  setNotice(null);
                }}
              >
                設定を開く
              </button>
            )}
            <button
              type="button"
              className="icon-button"
              onClick={() => setNotice(null)}
              aria-label="お知らせを閉じる"
            >
              <CloseIcon size={15} />
            </button>
          </div>
        )}
        {selectedId ? (
          <NoteView
            key={selectedId}
            noteId={selectedId}
            autoFocusTitle={selectedId === justCreatedId}
            recording={recording}
            onChanged={handleChanged}
            onDelete={(id) => void deleteNote(id)}
            onOpenNote={setSelectedId}
            onError={(message) => setNotice(toNotice(message))}
            autoEnhance={enhanceRequestId === selectedId}
            onAutoEnhanceHandled={() => setEnhanceRequestId(null)}
          />
        ) : (
          !loading && (
            <div className="empty-state">
              <NoteIcon size={40} />
              <h1>最初の会議ノートを作りましょう</h1>
              <p>
                会議中は要点だけメモ。録音した文字起こしと合わせて、
                <br />
                あとで AI が議事録に仕上げます。
              </p>
              <button type="button" className="button button-primary" onClick={() => void createNote()}>
                <PlusIcon size={16} />
                新しいノート
              </button>
            </div>
          )
        )}
      </main>

      {settingsOpen && <SettingsDialog onClose={() => setSettingsOpen(false)} />}
      {helpOpen && <HelpDialog onClose={() => setHelpOpen(false)} />}
    </div>
  );
}
