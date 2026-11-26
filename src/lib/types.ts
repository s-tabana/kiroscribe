// Rust 側（kiroscribe-core）の serde 定義と対応する型。フィールド名は camelCase。

export interface Note {
  id: string;
  title: string;
  /** 会議中に自分で書いたメモ */
  userNotes: string;
  /** AI が仕上げた議事録（Markdown）。未生成なら null */
  enhancedNotes: string | null;
  enhancedAt: number | null;
  createdAt: number;
  updatedAt: number;
}

export interface NoteSummary {
  id: string;
  title: string;
  preview: string;
  hasEnhanced: boolean;
  createdAt: number;
  updatedAt: number;
}

export interface NotePatch {
  title?: string;
  userNotes?: string;
  enhancedNotes?: string;
}

export interface Settings {
  language: string;
  transcriptionModel: string;
  claudeModel: string;
  inputDeviceId: string | null;
  captureSystemAudio: boolean;
  myName: string;
  /** 会議を検出したら自動で録音する */
  autoRecordMeetings: boolean;
  /** Windows の起動時に起動してタスクトレイに常駐する */
  launchAtLogin: boolean;
  /** 見つけた新しいバージョンを自動でインストールする（既定はオフで、知らせるだけ） */
  autoUpdate: boolean;
}

export type Provider = "openai" | "anthropic";

export interface ApiKeyStatus {
  openai: boolean;
  anthropic: boolean;
  /** 保存先の説明（例: Windows 資格情報マネージャー） */
  storage: string;
}

export const DEFAULT_SETTINGS: Settings = {
  language: "ja",
  transcriptionModel: "gpt-4o-transcribe",
  claudeModel: "claude-opus-5-5",
  inputDeviceId: null,
  captureSystemAudio: true,
  myName: "",
  autoRecordMeetings: true,
  launchAtLogin: true,
  autoUpdate: false,
};

// ---- 録音・文字起こし ----

/** me = 自分（マイク）、them = 相手（PC から流れる音声） */
export type Speaker = "me" | "them";

export interface Segment {
  id: number;
  noteId: string;
  speaker: Speaker;
  startedAt: number;
  endedAt: number;
  text: string;
}

export interface AudioDevice {
  id: string;
  name: string;
  isDefault: boolean;
}

export type RecordingState = "idle" | "recording" | "paused" | "finishing";

export interface RecordingStatus {
  noteId: string | null;
  state: RecordingState;
  /** runningSince より前の録音時間（一時停止を除く） */
  elapsedMs: number;
  /** 録音中ならその区間の開始時刻 */
  runningSince: number | null;
  sources: Speaker[];
}

export interface LevelEvent {
  speaker: Speaker;
  level: number;
}

export interface RecordingErrorEvent {
  noteId: string;
  message: string;
}

export const IDLE_STATUS: RecordingStatus = {
  noteId: null,
  state: "idle",
  elapsedMs: 0,
  runningSince: null,
  sources: [],
};

export const SPEAKER_LABEL: Record<Speaker, string> = { me: "自分", them: "相手" };

// ---- AI 議事録・チャット ----

export type ChatRole = "user" | "assistant";

export interface ChatMessage {
  id: number;
  noteId: string;
  role: ChatRole;
  content: string;
  createdAt: number;
}

/** 生成中の文字（議事録・チャットとも） */
export interface DeltaEvent {
  noteId: string;
  delta: string;
}

export type AiKind = "enhance" | "chat";

// ---- 書き出し ----

export interface ExportOptions {
  minutes: boolean;
  notes: boolean;
  transcript: boolean;
}

export interface ExportResult {
  markdown: string;
  /** 保存ダイアログの既定のファイル名 */
  fileName: string;
}

// ---- 会議の自動検出 ----

export type PopupPayload =
  | { kind: "recording"; app: string; noteId: string; title: string }
  | { kind: "finishing"; app: string; noteId: string; title: string }
  | { kind: "stopped"; app: string; noteId: string; title: string }
  | { kind: "error"; app: string; message: string }
  /** 長い録音で、会議が終わったかを聞いている。deadline は、答えがなければ止める時刻（UNIX ミリ秒） */
  | { kind: "confirmEnd"; app: string; noteId: string; title: string; deadline: number }
  /** 会議が終わったかの確認に答えがなかったので、録音を止めた */
  | { kind: "stoppedUnanswered"; app: string; noteId: string; title: string };

export interface OpenNoteRequest {
  noteId: string;
  /** 開いたあと議事録の作成を始める */
  enhance: boolean;
}

// ---- ヘルプ（使い方・リリースノート） ----

/** 表示している内容の出どころ。network = いま取得、cache = 前回取得して保存した内容、bundled = アプリ同梱 */
export type ContentSource = "network" | "cache" | "bundled";

export interface Guide {
  markdown: string;
  source: ContentSource;
  /** 取得した時刻（同梱版なら null） */
  fetchedAt: number | null;
}

export interface ReleaseNote {
  /** タグ名（例: v0.2.0） */
  tagName: string;
  /** タグ名から先頭の v を除いたもの。アプリのバージョンと比べる */
  version: string;
  name: string;
  /** 公開日時。ISO 8601、または日付だけ（2026-11-21） */
  publishedAt: string | null;
  /** 本文（Markdown） */
  body: string;
  /** GitHub のリリースページ */
  htmlUrl: string | null;
}

export interface ReleaseNotes {
  releases: ReleaseNote[];
  source: ContentSource;
  fetchedAt: number | null;
}

export type HelpTab = "guide" | "releases";

// ---- アプリ内更新 ----

export interface AvailableUpdate {
  version: string;
  /** リリースノートの本文（Markdown） */
  notes: string | null;
  publishedAt: number | null;
}

/** downloading = ダウンロード中、installing = インストーラを起動している（このあとアプリが終了する） */
export type UpdatePhase = "idle" | "downloading" | "installing";

export interface UpdateStatus {
  /** 見つかった新しい版（なければ null） */
  available: AvailableUpdate | null;
  /** 最後に確認できた時刻 */
  checkedAt: number | null;
  phase: UpdatePhase;
  downloadedBytes: number;
  /** ダウンロードする大きさ（分からなければ null） */
  totalBytes: number | null;
  /** ダウンロードが済んでいて、ウィンドウを閉じると自動でインストールする（自動更新がオンのときだけ true になる） */
  ready: boolean;
}

export const NO_UPDATE: UpdateStatus = {
  available: null,
  checkedAt: null,
  phase: "idle",
  downloadedBytes: 0,
  totalBytes: null,
  ready: false,
};
