import { createContext, useContext } from "react";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ask, save } from "@tauri-apps/plugin-dialog";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import type {
  AiKind,
  ApiKeyStatus,
  AudioDevice,
  ChatMessage,
  DeltaEvent,
  ExportOptions,
  ExportResult,
  Guide,
  LevelEvent,
  Note,
  NotePatch,
  NoteSummary,
  OpenNoteRequest,
  PopupPayload,
  Provider,
  RecordingErrorEvent,
  RecordingStatus,
  ReleaseNotes,
  Segment,
  Settings,
  UpdateStatus,
} from "./types";
import { createMemoryBackend } from "./memoryBackend";

export type Unsubscribe = () => void;

/**
 * 画面から見たバックエンド。Tauri 上では Rust のコマンドを呼び、
 * ブラウザ（デザイン確認用のプレビュー）やテストではメモリ上の実装を使う。
 */
export interface Backend {
  /** プレビュー実装かどうか（画面に注記を出すため） */
  readonly isPreview: boolean;
  listNotes(): Promise<NoteSummary[]>;
  createNote(): Promise<Note>;
  getNote(id: string): Promise<Note>;
  updateNote(id: string, patch: NotePatch): Promise<Note>;
  deleteNote(id: string): Promise<void>;
  getSettings(): Promise<Settings>;
  saveSettings(settings: Settings): Promise<Settings>;
  getApiKeyStatus(): Promise<ApiKeyStatus>;
  /** 空文字を渡すと削除。値は保存後に画面へ返さない */
  setApiKey(provider: Provider, value: string): Promise<ApiKeyStatus>;
  /** 確認ダイアログ。kind は見た目（既定は警告。削除などの取り消せない操作用） */
  confirm(message: string, title?: string, kind?: "warning" | "info"): Promise<boolean>;

  // ---- 録音・文字起こし ----
  listAudioDevices(): Promise<AudioDevice[]>;
  listSegments(noteId: string): Promise<Segment[]>;
  getRecordingStatus(): Promise<RecordingStatus>;
  startRecording(noteId: string): Promise<RecordingStatus>;
  pauseRecording(): Promise<RecordingStatus>;
  resumeRecording(): Promise<RecordingStatus>;
  /** 停止し、残りの文字起こしが終わってから解決する */
  stopRecording(): Promise<RecordingStatus>;
  onRecordingStatus(cb: (status: RecordingStatus) => void): Unsubscribe;
  onSegment(cb: (segment: Segment) => void): Unsubscribe;
  onLevel(cb: (event: LevelEvent) => void): Unsubscribe;
  onRecordingError(cb: (event: RecordingErrorEvent) => void): Unsubscribe;
  // ---- AI 議事録・チャット ----
  /** メモと文字起こしから議事録を作って保存し、更新後のノートを返す。途中経過は onEnhanceDelta */
  enhanceNote(noteId: string): Promise<Note>;
  cancelAi(noteId: string, kind: AiKind): Promise<void>;
  onEnhanceDelta(cb: (event: DeltaEvent) => void): Unsubscribe;
  listChatMessages(noteId: string): Promise<ChatMessage[]>;
  /** 質問を送り、保存された回答を返す。途中経過は onChatDelta */
  sendChatMessage(noteId: string, content: string): Promise<ChatMessage>;
  clearChatMessages(noteId: string): Promise<void>;
  onChatDelta(cb: (event: DeltaEvent) => void): Unsubscribe;

  // ---- 書き出し ----
  exportMarkdown(noteId: string, options: ExportOptions): Promise<ExportResult>;
  copyText(text: string): Promise<void>;
  /** 保存ダイアログを出して Markdown を保存する。保存したパス、取り消したら null */
  saveMarkdown(noteId: string, options: ExportOptions, fileName: string): Promise<string | null>;

  // ---- 会議の自動検出 ----
  /** 自動検出でノートが作られた */
  onNoteCreated(cb: (note: Note) => void): Unsubscribe;
  /** ポップアップから「ノートを開く」「議事録を作成」が押された */
  onOpenNote(cb: (request: OpenNoteRequest) => void): Unsubscribe;
  onOpenSettings(cb: () => void): Unsubscribe;
  /** ポップアップ用 */
  getPopupState(): Promise<PopupPayload | null>;
  onPopupUpdate(cb: (payload: PopupPayload) => void): Unsubscribe;
  closePopup(): Promise<void>;
  openNoteFromPopup(noteId: string, enhance: boolean): Promise<void>;
  openSettingsFromPopup(): Promise<void>;

  // ---- バージョン・ヘルプ ----
  getAppVersion(): Promise<string>;
  /** 使い方。取得できないときは保存済みか同梱の内容が返る（source で分かる） */
  getGuide(): Promise<Guide>;
  getReleaseNotes(): Promise<ReleaseNotes>;
  /** https:// の URL を外部ブラウザで開く（アプリの画面は遷移させない） */
  openExternal(url: string): Promise<void>;

  // ---- アプリ内更新 ----
  getUpdateStatus(): Promise<UpdateStatus>;
  /** 手動の確認。確認できなかったら理由のメッセージで reject する */
  checkForUpdate(): Promise<UpdateStatus>;
  /** インストールする（まだならダウンロードしてから）。成功するとアプリが終了して起動し直される。進み具合は onUpdateStatus */
  installUpdate(): Promise<void>;
  onUpdateStatus(cb: (status: UpdateStatus) => void): Unsubscribe;
  /**
   * 自動で更新したあと、ウィンドウを表示しているときに 1 回だけ、更新後のバージョンを返す。
   * 知らせるものがない・ウィンドウを閉じている間は null
   */
  takeUpdateNotice(): Promise<string | null>;
  /** 閉じていたメインウィンドウが表示された */
  onMainShown(cb: () => void): Unsubscribe;
}

/** Tauri のイベント購読を、同期的に解除できる形にする */
function subscribe<T>(event: string, cb: (payload: T) => void): Unsubscribe {
  let disposed = false;
  let unlisten: (() => void) | null = null;
  void listen<T>(event, (e) => cb(e.payload)).then((fn) => {
    if (disposed) fn();
    else unlisten = fn;
  });
  return () => {
    disposed = true;
    unlisten?.();
  };
}

export function createTauriBackend(): Backend {
  return {
    isPreview: false,
    listNotes: () => invoke("list_notes"),
    createNote: () => invoke("create_note"),
    getNote: (id) => invoke("get_note", { id }),
    updateNote: (id, patch) => invoke("update_note", { id, patch }),
    deleteNote: (id) => invoke("delete_note", { id }),
    getSettings: () => invoke("get_settings"),
    saveSettings: (settings) => invoke("save_settings", { settings }),
    getApiKeyStatus: () => invoke("get_api_key_status"),
    setApiKey: (provider, value) => invoke("set_api_key", { provider, value }),
    confirm: (message, title = "Kiroscribe", kind = "warning") => ask(message, { title, kind }),

    listAudioDevices: () => invoke("list_audio_devices"),
    listSegments: (noteId) => invoke("list_segments", { noteId }),
    getRecordingStatus: () => invoke("get_recording_status"),
    startRecording: (noteId) => invoke("start_recording", { noteId }),
    pauseRecording: () => invoke("pause_recording"),
    resumeRecording: () => invoke("resume_recording"),
    stopRecording: () => invoke("stop_recording"),
    onRecordingStatus: (cb) => subscribe("recording-status", cb),
    onSegment: (cb) => subscribe("transcript-segment", cb),
    onLevel: (cb) => subscribe("audio-level", cb),
    onRecordingError: (cb) => subscribe("recording-error", cb),
    enhanceNote: (noteId) => invoke("enhance_note", { noteId }),
    cancelAi: (noteId, kind) => invoke("cancel_ai", { noteId, kind }),
    onEnhanceDelta: (cb) => subscribe("enhance-delta", cb),
    listChatMessages: (noteId) => invoke("list_chat_messages", { noteId }),
    sendChatMessage: (noteId, content) => invoke("send_chat_message", { noteId, content }),
    clearChatMessages: (noteId) => invoke("clear_chat_messages", { noteId }),
    onChatDelta: (cb) => subscribe("chat-delta", cb),
    exportMarkdown: (noteId, options) => invoke("export_markdown", { noteId, options }),
    copyText: (text) => writeText(text),
    async saveMarkdown(noteId, options, fileName) {
      const path = await save({
        defaultPath: fileName,
        filters: [{ name: "Markdown", extensions: ["md"] }],
      });
      if (!path) return null;
      await invoke("save_markdown", { noteId, options, path });
      return path;
    },
    onNoteCreated: (cb) => subscribe("note-created", cb),
    onOpenNote: (cb) => subscribe("open-note", cb),
    onOpenSettings: (cb) => subscribe("open-settings", () => cb()),
    getPopupState: () => invoke("get_popup_state"),
    onPopupUpdate: (cb) => subscribe("meeting-popup", cb),
    closePopup: () => invoke("close_popup"),
    openNoteFromPopup: (noteId, enhance) => invoke("open_note_from_popup", { noteId, enhance }),
    openSettingsFromPopup: () => invoke("open_settings_from_popup"),

    getAppVersion: () => invoke("get_app_version"),
    getGuide: () => invoke("get_guide"),
    getReleaseNotes: () => invoke("get_release_notes"),
    openExternal: (url) => invoke("open_external", { url }),
    getUpdateStatus: () => invoke("get_update_status"),
    checkForUpdate: () => invoke("check_for_update"),
    installUpdate: () => invoke("install_update"),
    onUpdateStatus: (cb) => subscribe("update-status", cb),
    takeUpdateNotice: () => invoke("take_update_notice"),
    onMainShown: (cb) => subscribe("main-shown", () => cb()),
  };
}

export function createDefaultBackend(): Backend {
  if (isTauri()) return createTauriBackend();
  const preview = createMemoryBackend({ seed: true, demo: true });
  // デザイン確認用: ブラウザのコンソールから __kiroscribePreview.simulateUpdate("9.9.9", { announce: true }) などで状態を作れる
  (window as { __kiroscribePreview?: unknown }).__kiroscribePreview = preview;
  return preview;
}

export const BackendContext = createContext<Backend | null>(null);

export function useBackend(): Backend {
  const backend = useContext(BackendContext);
  if (!backend) throw new Error("BackendContext が設定されていません");
  return backend;
}

/** invoke の reject 値（Rust の CommandError は文字列）をメッセージにする */
export function errorMessage(err: unknown): string {
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message;
  return "不明なエラーが発生しました";
}
